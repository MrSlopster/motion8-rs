# 8bitdo-motion

Cemuhook (DSU) motion-data server for Linux, reading gyro/accelerometer through
**SDL 3**. Built with the [8BitDo Ultimate 2 / Ultimate 2C](https://www.8bitdo.com/)
in mind — whose motion is only visible through SDL on Linux — but works for any
SDL gamepad that reports a gyroscope.

Serve the data via the [Cemuhook UDP protocol v1001](https://v1993.github.io/cemuhook-protocol/),
so emulators that support "CemuHook compatible motion (DSU)" — Ryujinx, Cemu,
Dolphin, Citra, RPCS3, … — can use your controller's motion. This makes it the
SDL counterpart of [evdevhook2](https://github.com/v1993/evdevhook2), which only
reads motion via evdev.

## Requirements

- Linux
- SDL 3.2+ runtime (`libSDL3`) with the hidapi backend (distro packages normally include it)
- A gyro-capable controller (e.g. 8BitDo Ultimate 2 in any mode; gyro is exposed via SDL)

## Build

```bash
cargo build --release
# binary: target/release/8bitdo-motion
```

## Connect your controller

- **Bluetooth / 2.4 GHz dongle / USB** all work on recent firmwares.
- If buttons work but no gamepad with gyro appears (especially dongle/USB), add a udev rule:

```bash
sudo tee /etc/udev/rules.d/60-8bitdo.rules <<'EOF'
# 8BitDo Ultimate 2 dongle/USB — grant hidraw access for SDL motion
KERNEL=="hidraw*", ATTRS{idVendor}=="2dc8", ATTRS{idProduct}=="6012", TAG+="uaccess", MODE="0660"
EOF
sudo udevadm control --reload && sudo udevadm trigger
```

(Adapt `idProduct` if `lsusb` shows a different ID for your model.)

## Run

```bash
./target/release/8bitdo-motion            # binds 127.0.0.1:26760 by default
./target/release/8bitdo-motion --motion-test   # live sensor readout (see below)
```

Config is read from (first hit wins):

1. `--config <path>`
2. `$XDG_CONFIG_HOME/8bitdo-motion/config.toml`
3. `./8bitdo-motion.toml`

Without a config file everything defaults; `ExampleConfig.toml` documents the options:

```toml
port = 26760            # UDP port to serve on
bind = "127.0.0.1"      # bind address ("0.0.0.0" to serve other machines)
send_rate_hz = 100      # data-packet rate
device_filter = ["Ultimate"]   # substring match; absent = all gyro gamepads
```

## Configure Ryujinx

Options → Settings → Input → (your controller) → **Motion** → Configure:

- Motion Source: **CemuHook compatible motion (DSU)**
- Server Host: `127.0.0.1`, Server Port: `26760`, Controller Slot: `0`

The controller's buttons/sticks are mapped normally (e.g. as a Switch Pro
Controller); the DSU server only supplies the motion data.

## Checking motion / fixing mirrored axes

Run `--motion-test` and waggle the pad — you'll see live accel (g) and gyro
(deg/s) values:

- Hold flat and still: accel should show ≈ `+1` g on **Y**, gyro near 0.
- Roll the controller clockwise (right side down): **roll** should be non-zero —
  compare the sign you'd expect in game.

If a channel is mirrored in your emulator, flip the `GYRO_SIGN` / `ACCEL_SIGN`
constants in `src/sdl_source.rs` (`convert_motion`) and rebuild — DSU clients
have no standardized sign convention across all devices, so this is calibrated
empirically.

## How it works

Single-threaded ~100 Hz loop: pump SDL events (hotplug + sensor updates),
drain the UDP socket (version/info requests answered immediately, data
requests registered as subscribers), then stream 100-byte DSU data packets to
each active subscriber. Subscribers not re-requesting for 5 s are dropped.

Slots: the first 4 gyro-capable gamepads claim slots 0–3 (revealed by
`device_filter`). Since SDL doesn't expose MACs, a stable pseudo-MAC is
derived from each gamepad's SDL GUID. Battery level is reported via SDL power
info.

## Testing

```bash
cargo test     # protocol goldens, server integration tests (fake UDP client), unit conversions
```
