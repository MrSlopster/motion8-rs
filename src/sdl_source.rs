//! SDL3-backed `MotionSource`.
//!
//! Pure helper functions are unit-tested; the SDL glue is thin.

use std::collections::HashMap;
use std::time::Instant;

use sdl3::gamepad::Gamepad;
use sdl3::joystick::{ConnectionState, JoystickId, PowerLevel};
use sdl3::sensor::SensorType;

use crate::config::Config;
use crate::dsu::{CONN_BLUETOOTH, CONN_USB, MotionSample};
use crate::source::{ControllerSnapshot, MotionSource};

/// DSU battery encoding, from SDL power info.
pub(crate) fn battery_byte(level: PowerLevel, percentage: i32) -> u8 {
    match level {
        PowerLevel::Charging => 0xEE,
        PowerLevel::Charged => 0xEF,
        PowerLevel::OnBattery | PowerLevel::NoBattery | PowerLevel::Unknown | PowerLevel::Error => {
            match percentage {
                _ if percentage > 90 => 0x05,
                _ if percentage > 60 => 0x04,
                _ if percentage > 30 => 0x03,
                _ if percentage > 10 => 0x02,
                p if p >= 0 => 0x01,
                _ => 0x00, // unknown percentage
            }
        }
    }
}

/// DSU connection type from SDL connection state.
pub(crate) fn connection_byte(state: ConnectionState) -> u8 {
    match state {
        ConnectionState::Wired => CONN_USB,
        _ => CONN_BLUETOOTH, // wireless covers BT and the 2.4 GHz dongle
    }
}

/// Stable pseudo-MAC: CRC32 of the SDL GUID data (16 bytes), expanded to
/// 6 bytes. SDL does not expose the real MAC; stability across reconnects
/// is what matters (clients use it to track the same device between
/// launches).
pub(crate) fn guid_to_mac(guid_data: &[u8; 16]) -> [u8; 6] {
    let crc = crc32fast::hash(guid_data);
    let bytes = crc.to_le_bytes();
    [bytes[0], bytes[1], bytes[2], bytes[3], bytes[0] ^ 0x5a, bytes[1] ^ 0xa5]
}

/// SDL sensor readings → DSU motion sample.
///
/// SDL gyro axes are already (pitch, yaw, roll) per the SDL docs — same
/// order DSU wants — so only units change here. Sign calibration against
/// Ryujinx is done in game; adjust constants below if needed.
pub(crate) fn convert_motion(
    accel_ms2: [f32; 3],
    gyro_rad_s: [f32; 3],
    timestamp_us: u64,
) -> MotionSample {
    const GRAVITY: f32 = sdl3::sys::sensor::SDL_STANDARD_GRAVITY as f32;
    const RAD_TO_DEG: f32 = std::f32::consts::PI / 180.0;
    // Sign/calibration knobs; flip if motion is mirrored in-game.
    const GYRO_SIGN: [f32; 3] = [1.0, 1.0, 1.0];
    const ACCEL_SIGN: [f32; 3] = [1.0, 1.0, 1.0];
    MotionSample {
        accel: [
            ACCEL_SIGN[0] * accel_ms2[0] / GRAVITY,
            ACCEL_SIGN[1] * accel_ms2[1] / GRAVITY,
            ACCEL_SIGN[2] * accel_ms2[2] / GRAVITY,
        ],
        gyro: [
            GYRO_SIGN[0] * gyro_rad_s[0] / RAD_TO_DEG,
            GYRO_SIGN[1] * gyro_rad_s[1] / RAD_TO_DEG,
            GYRO_SIGN[2] * gyro_rad_s[2] / RAD_TO_DEG,
        ],
        timestamp_us,
    }
}

struct OpenPad {
    gamepad: Gamepad,
    name: String,
    mac: [u8; 6],
}

pub struct SdlSource {
    gamepad: sdl3::GamepadSubsystem,
    /// Pumped each poll; SDL maintains sensor readings via the event loop.
    events: sdl3::EventPump,
    /// JoystickId → opened pad (sensors enabled).
    open: HashMap<JoystickId, OpenPad>,
    /// Enumeration order defines slot assignment.
    order: Vec<JoystickId>,
    filter: Vec<String>,
    start: Instant,
}

impl SdlSource {
    /// SDL must already be initialized with the gamepad subsystem.
    pub fn new(gamepad: sdl3::GamepadSubsystem, events: sdl3::EventPump, config: &Config) -> Self {
        Self {
            gamepad,
            events,
            open: HashMap::new(),
            order: Vec::new(),
            filter: config.device_filter.clone(),
            start: Instant::now(),
        }
    }

    fn passes_filter(&self, name: &str) -> bool {
        crate::config::filter_allows(&self.filter, name)
    }
}

impl MotionSource for SdlSource {
    fn poll(&mut self) {
        self.events.pump_events();
        let ids = match self.gamepad.gamepads() {
            Ok(ids) => ids,
            Err(_) => return,
        };
        // Forget pads that disappeared.
        self.open.retain(|id, _| ids.contains(id));
        // Open new pads (that pass the filter and have a gyro).
        for id in &ids {
            if self.open.contains_key(id) {
                continue;
            }
            let name = self.gamepad.name_for_id(*id).unwrap_or_default();
            if !self.passes_filter(&name) {
                continue;
            }
            let Ok(pad) = self.gamepad.open(*id) else { continue };
            // We hold a validly-opened Gamepad, so this is safe to call.
            let has_gyro = unsafe { pad.has_sensor(SensorType::Gyroscope) };
            if !has_gyro {
                continue; // no gyro → not exposed (lean core)
            }
            pad.sensor_set_enabled(SensorType::Gyroscope, true).ok();
            pad.sensor_set_enabled(SensorType::Accelerometer, true).ok();
            let mac = guid_to_mac(&self.gamepad.guid_for_id(*id).raw.data);
            log::info!("Slot-eligible gamepad: {name} (mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            self.open.insert(*id, OpenPad { gamepad: pad, name, mac });
        }
        self.order = ids;
    }

    fn controllers(&self) -> Vec<ControllerSnapshot> {
        let mut out = Vec::new();
        let mut slot = 0u8;
        for id in &self.order {
            if let Some(pad) = self.open.get(id) {
                if slot >= 4 {
                    break;
                }
                let connection = pad
                    .gamepad
                    .connection_state()
                    .map(connection_byte)
                    .unwrap_or(CONN_BLUETOOTH);
                let power = pad.gamepad.power_info();
                out.push(ControllerSnapshot {
                    slot,
                    connected: pad.gamepad.connected(),
                    connection,
                    mac: pad.mac,
                    battery: battery_byte(power.state, power.percentage),
                    name: pad.name.clone(),
                });
                slot += 1;
            }
        }
        out
    }

    fn motion(&mut self, slot: u8) -> Option<MotionSample> {
        let id = *self.order.get(slot as usize)?;
        let pad = self.open.get(&id)?;
        if !pad.gamepad.connected() {
            return None;
        }
        let mut accel = [0f32; 3];
        let mut gyro = [0f32; 3];
        pad.gamepad.sensor_get_data(SensorType::Accelerometer, &mut accel).ok()?;
        pad.gamepad.sensor_get_data(SensorType::Gyroscope, &mut gyro).ok()?;
        Some(convert_motion(
            accel,
            gyro,
            self.start.elapsed().as_micros() as u64,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsu::{MODEL_FULL_GYRO, PROTOCOL_VERSION, SLOT_STATE_CONNECTED};

    #[test]
    fn mac_is_stable_and_six_bytes() {
        let data: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x01,
        ];
        let a = guid_to_mac(&data);
        let b = guid_to_mac(&data);
        assert_eq!(a, b, "same GUID → same MAC");
        assert_eq!(a.len(), 6);
        // Different GUID → different MAC
        let mut other = data;
        other[0] ^= 0xff;
        assert_ne!(guid_to_mac(&other), a);
    }

    #[test]
    fn motion_unit_conversion() {
        // 90°/s about pitch, 1g downward on Y
        let s = convert_motion(
            [0.0, sdl3::sys::sensor::SDL_STANDARD_GRAVITY as f32, 0.0],
            [std::f32::consts::PI / 2.0, 0.0, -std::f32::consts::PI / 2.0],
            777,
        );
        assert!((s.gyro[0] - 90.0).abs() < 1e-4, "rad/s → deg/s, pitch");
        assert!((s.gyro[1]).abs() < 1e-6);
        assert!((s.gyro[2] + 90.0).abs() < 1e-4, "roll");
        assert!((s.accel[1] - 1.0).abs() < 1e-4, "m/s² → g");
        assert_eq!(s.accel[0], 0.0);
        assert_eq!(s.timestamp_us, 777);
    }

    #[test]
    fn snapshot_shape() {
        // Slot/state values the server relies on (compile-time sanity).
        let _ = ControllerSnapshot {
            slot: 0,
            connected: true,
            connection: CONN_USB,
            mac: [0; 6],
            battery: 0x05,
            name: String::new(),
        };
        assert_eq!(SLOT_STATE_CONNECTED, 2);
        assert_eq!(MODEL_FULL_GYRO, 2);
        assert_eq!(PROTOCOL_VERSION, 1001);
    }
}
