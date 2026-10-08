# 8BitDo Motion — SDL-based Cemuhook UDP (DSU) motion server

**Date:** 2026-10-08
**Status:** Draft for review
**Goal:** Enable motion (gyro/accel) controls in Ryujinx (and any other Cemuhook-protocol emulator) for the 8BitDo Ultimate 2 / Ultimate 2C on Linux, by serving the motion data over the Cemuhook UDP (DSU) protocol from an SDL3-based Rust utility.

## Background

- [evdevhook2](https://github.com/v1993/evdevhook2) is the reference DSU motion server for Linux, but it reads motion via **evdev only** and has no SDL backend. 8BitDo gyro on Linux is exposed through **SDL** (8BitDo contributed drivers to SDL's hidapi/udev layer), so evdevhook2 cannot see it.
- The DSU protocol is frozen at version **1001**; the authoritative reference (maintained by the evdevhook2 author) is <https://v1993.github.io/cemuhook-protocol/>.
- Ryujinx consumes it via *Input → controller → Motion → CemuHook compatible motion (DSU)* with host/port (default `127.0.0.1:26760`) and a controller slot (0–3).

## Scope

**In scope (lean core):**
- Cemuhook UDP/DSU **v1001** server: version (`0x100000`), controller info (`0x100001`), controller data (`0x100002`).
- SDL3 gamepad **motion source**: gyro + accelerometer via the SDL gamepad sensor API, hotplug/reconnect handling.
- Target device: **8BitDo Ultimate 2 / Ultimate 2C** (via Bluetooth, 2.4 GHz dongle, or USB), but implemented generically — any SDL gamepad exposing gyro works.
- Up to **4 slots** (protocol maximum per server).
- Small **TOML config**: UDP port, send rate, optional device-name filter.
- Battery reporting via SDL power info (cheap, included).
- Linux only.
- README with setup instructions, including the udev rule for hidraw access (`2dc8:6012` dongle/USB case).

**Out of scope (non-goals):**
- Rumble / unofficial messages (`0x110001`, `0x110002`) — buttons and rumble flow through the emulator's normal controller mapping, not the DSU server.
- Touch data (always zeroed).
- Non-SDL motion backends (evdev, hidraw).
- Windows/macOS.
- Calibration UI, orientation remapping, allowlist/blocklist beyond a simple name filter.

## Protocol details (from the reference)

- Transport: UDP, default port **26760**.
- **Header (16 bytes):** magic `DSUS` (server→client) / `DSUC` (client→server); u16 protocol version `1001`; u16 body length (packet length minus header); u32 **CRC32** of the whole packet with the CRC field zeroed (little-endian, standard CRC32); u32 client/server ID (random per run, constant while running). A u32 message type follows the header (counts toward body length).
- **Version request/response** (`0x100000`): body = u16 max supported protocol version (`1001`).
- **Info request** (`0x100001`): body = i32 slot count + one u8 slot per entry. Response: one packet per requested slot: 11-byte shared begin + `\0`.
- **Data request** (`0x100002`): body = bitmask (1 = register by slot, 2 = by MAC, 0 = all controllers) + slot byte and/or 6-byte MAC. No instant reply; the server then **streams** 100-byte data packets (header + type + payload) for the subscribed controller(s) at the configured rate. **Timeout:** stop streaming to a client that hasn't sent a data request for ~5 s.
- **Shared 11-byte begin:** slot (u8), state (u8: 0 disconnected / 2 connected), model (u8: 2 = full gyro), connection type (u8: 1 USB / 2 Bluetooth), 6-byte MAC, battery (u8: 0x01 dying … 0x05 full, 0xEE charging, 0xEF charged).
- **Data packet:** 11-byte begin, connected flag (u8), u32 packet number (per client), two button bitmask bytes (zeroed), HOME/TOUCH (0), sticks (128 = neutral), analog buttons (0), two 6-byte touch records (zeros), u64 **timestamp in µs**, 3×f32 **accel in g**, 3×f32 **gyro in deg/s** (order: pitch, yaw, roll).

## Architecture

Single Rust binary, single-threaded event loop at the send rate (~100 Hz default).

```
┌───────────┐   sensor samples    ┌──────────────┐   DSUS packets   ┌─────────┐
│ SDL3      │ ──────────────────▶ │  DSU server  │ ───────────────▶ │ Ryujinx │
│ gamepads  │   (polled per tick) │  (UDP 26760) │                  │  (DSUC) │
└───────────┘                     └──────────────┘                  └─────────┘
```

**Crate/binary name:** `8bitdo-motion` (project directory already `8bitdomotion`).

**Modules:**

| Module | Responsibility |
|---|---|
| `dsu` | Wire format: header + CRC, request parsing (version/info/data), response builders. No I/O — pure data in/out, fully unit-testable. |
| `server` | UDP socket (bound to configured port, `0.0.0.0`/localhost configurable), per-subscriber state (slot/MAC/all registration, packet counters, last-request timestamp), tick loop sending data packets to active subscribers. Talks to a `MotionSource` trait, not SDL directly. |
| `source` | `MotionSource` trait: enumerate controllers, per-slot snapshot (connected, model, conn type, mac, battery, gyro/accel samples). Production impl = SDL; test impl = synthetic. |
| `sdl_source` | SDL3 implementation of `MotionSource`: init gamepad subsystem, hotplug events → slot table (first 4 gyro-capable gamepads), per-tick read via `SDL_GetGamepadSensorData`. |
| `config` | TOML parse + defaults: `port = 26760`, `send_rate_hz = 100`, `bind = "127.0.0.1"`, optional `device_filter = ["Ultimate"]` (substring match on gamepad name; empty/absent = all). |
| `main` | CLI (`--config <path>`, default `$XDG_CONFIG_HOME/8bitdo-motion/config.toml` falling back to `./8bitdo-motion.toml`), logging via `env_logger`, wires everything, runs the loop. |

**Main loop (single thread, ~10 ms tick):**
1. Pump SDL events (non-blocking): hotplug handling, enable sensors on newly opened gamepads (`SDL_SetGamepadSensorEnabled` for GYRO + ACCEL).
2. Drain the UDP socket (non-blocking read): parse requests; update subscriber registry; answer version/info immediately.
3. For each subscriber whose controller is connected: read latest sensor data from SDL, build data packet, send. Skip subscribers past the 5 s re-request timeout.
4. Sleep to next tick.

Single-threaded avoids any shared-state synchronization; SDL event pumping and socket reads are both poll-able and fast enough at 100 Hz.

**Motion data specifics:**
- SDL gyro is in **rad/s** → multiply by `180/π` for DSU deg/s. SDL accel is in m/s² → divide by `9.8` (SDL_STANDARD_GRAVITY) for DSU g.
- Axis/sign mapping (SDL axes → DSU accel x/y/z and gyro pitch/yaw/roll) starts as a straightforward passthrough (pitch=x, yaw=y, roll=z) and is **verified empirically against Ryujinx during implementation**, cross-checked against the axis handling in evdevhook2's source (the ground-truth mapping for DSU conventions). Any sign flips discovered get recorded in the spec addendum and code comments.
- **MAC:** SDL doesn't expose MACs; derive a stable 6-byte pseudo-MAC by hashing the gamepad's SDL **GUID** (stable across reconnects). Deterministic, stable-per-device, matches what other DSU servers do.
- **Connection type:** SDL3 `SDL_GamepadConnectionState` → wired ⇒ USB (1), wireless ⇒ Bluetooth (2).
- **Battery:** `SDL_GetGamepadPowerInfo` percent → DSU buckets (0–10 dying, … 90–100 full; charging state ⇒ 0xEE).
- **Timestamp:** monotonic clock microseconds since process start.
- Slots claim order: gamepads reporting a gyro, in connection order. Gamepads without gyro are not exposed (Ryujinx needs motion here).

**Error handling:**
- Malformed/bad-CRC/short incoming packets: drop silently (debug-level log).
- Unknown message type: ignore.
- SDL init failure or socket bind failure: log error, exit non-zero.
- Gamepad disconnect mid-stream: subscriber keeps existing; info packets report state 0; data packets stop for that slot; auto re-claim slot on reconnect.

## Dependencies

- `sdl3` (via `sdl3-sys`) — system SDL3 ≥ 3.2 (distro package) preferred; `build-from-source` feature as fallback documented in README.
- `crc32fast` — CRC32 of packets.
- `serde` + `toml` — config.
- `env_logger` + `log` — logging.
- `clap` — CLI args.

## Testing

- **Unit tests (`dsu`):** encode/decode golden packets derived from the protocol reference (header layout, CRC zeroing semantics, info request/response, data request bitmask variants, 100-byte data packet layout and float units).
- **Integration test (`server` + fake source):** bind server on an ephemeral port with a synthetic `MotionSource`; a test UDP client performs the real handshake — version request, info request, data request (by slot and by "all") — and asserts received data packets carry the synthetic samples and increment packet numbers; asserts streaming stops after the timeout window (with a shortened test timeout).
- **Manual verification:** connect Ultimate 2 (BT and dongle), run server, point Ryujinx DSU motion at it, confirm gyro/accel respond correctly in a motion-sensitive game or Ryujinx's motion config; verify battery and hotplug behavior.

## Risks / open points

1. **Axis sign conventions** for DSU vs SDL — resolved empirically during implementation (see Motion data specifics).
2. **SDL runtime availability** — SDL 3.2+ must be installed or built; documented in README.
3. **udev permissions** — dongle/USB hidraw access may need the `2dc8:6012` udev rule; documented in README (Bluetooth typically works without it).
4. **Ryujinx slot/MAC handling quirks** — Ryujinx registers by slot by default; MAC-based registration is still implemented for protocol completeness.
