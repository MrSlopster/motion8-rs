//! Motion source abstraction.
//!
//! The DSU server talks to this trait, not to SDL directly, so the server
//! can be integration-tested with a synthetic source.

use crate::dsu::MotionSample;

/// Per-slot controller state snapshot, in DSU wire units.
#[derive(Debug, Clone)]
pub struct ControllerSnapshot {
    pub slot: u8,
    pub connected: bool,
    /// DSU connection type (1 = USB, 2 = Bluetooth).
    pub connection: u8,
    /// Stable pseudo-MAC for the device.
    pub mac: [u8; 6],
    /// DSU battery byte (0x01 dying … 0x05 full, 0xEE charging).
    pub battery: u8,
    pub name: String,
}

/// Anything that can supply motion data for up to 4 slots.
pub trait MotionSource {
    /// Refresh internal state (device hotplug, sensor reads). Called once
    /// per server tick.
    fn poll(&mut self);

    /// Snapshot for every managed slot, 0..=4 entries, ordered by slot.
    fn controllers(&self) -> Vec<ControllerSnapshot>;

    /// Latest motion sample for `slot`, if the controller is connected and
    /// has fresh sensor data.
    fn motion(&mut self, slot: u8) -> Option<MotionSample>;
}
