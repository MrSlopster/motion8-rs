# 8bitdo-motion

DSU (Cemuhook) motion server for Linux that reads gyro/accel over SDL 3.

8BitDo controllers only expose their gyro through SDL's hidapi drivers (not evdev), so [evdevhook2](https://github.com/v1993/evdevhook2) can't see it. 
This speaks [protocol v1001](https://v1993.github.io/cemuhook-protocol/) instead with the same result: Ryujinx, Cemu, Dolphin, Citra, RPCS3 etc.
Treat it as a CemuHook compatible motion source.

## Requirements

- Linux, SDL 3.2+ (`libSDL3`, hidapi backend included in distro packages)
- A gyro-capable controller (8BitDo Ultimate 2/2C: BT, dongle, or USB)

## Build

```bash
cargo build --release
```

## Setup

If buttons work but the gyro doesn't show up (dongle/USB), allow hidraw access:

```bash
sudo tee /etc/udev/rules.d/60-8bitdo.rules <<'EOF'
KERNEL=="hidraw*", ATTRS{idVendor}=="2dc8", ATTRS{idProduct}=="6012", TAG+="uaccess", MODE="0660"
EOF
sudo udevadm control --reload && sudo udevadm trigger
```

Check the product ID in `lsusb` for your model.

## Run

```bash
./target/release/8bitdo-motion             # 127.0.0.1:26760
./target/release/8bitdo-motion --motion-test
```

Config search order: `--config <path>`, `$XDG_CONFIG_HOME/8bitdo-motion/config.toml`,
`./8bitdo-motion.toml`. Everything is optional (see [ExampleConfig.toml](ExampleConfig.toml)):

```toml
port = 26760
bind = "127.0.0.1"             # "0.0.0.0" to serve the LAN
send_rate_hz = 100
device_filter = ["Ultimate"]   # substring match; empty = all gyro gamepads
```

## Ryujinx

Input → controller → **Motion** → Configure → CemuHook compatible motion (DSU),
host `127.0.0.1`, port `26760`, slot `0`. Map buttons/sticks as usual, e.g. as
a Switch Pro Controller — motion comes only from this server.

## If motion is mirrored

Run `--motion-test`: holding the pad flat should show ~`+1` g on accel Y and
near-zero gyro. If a channel is inverted in game, flip the sign in
`GYRO_SIGN`/`ACCEL_SIGN` (in `convert_motion`, `src/sdl_source.rs`) and rebuild.
DSU has no fixed sign convention across devices; yaw is already inverted for
Ryujinx by default.
