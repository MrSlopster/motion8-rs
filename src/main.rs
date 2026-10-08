mod config;
mod dsu;
mod sdl_source;
mod server;
#[cfg(test)]
mod server_tests;
mod source;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "8bitdo-motion", about = "SDL-based Cemuhook (DSU) motion server for Linux, built for 8BitDo controllers", version)]
struct Args {
    /// Path to the config file (default: $XDG_CONFIG_HOME/8bitdo-motion/config.toml, then ./8bitdo-motion.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Print live motion samples and exit — use this to sanity-check axis
    /// signs before configuring an emulator.
    #[arg(long)]
    motion_test: bool,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    let path = args.config.clone().or_else(config::default_config_path);
    let cfg = match path {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(text) => match config::Config::parse(&text) {
                Ok(c) => {
                    log::info!("Loaded config from {}", p.display());
                    c
                }
                Err(e) => {
                    eprintln!("Error: {e} (in {})", p.display());
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("Error: cannot read config {}: {e}", p.display());
                std::process::exit(1);
            }
        },
        None => {
            log::info!("No config file found; using defaults (run --help for options)");
            config::Config::default()
        }
    };

    let sdl = match sdl3::init() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: SDL init failed: {e}");
            std::process::exit(1);
        }
    };
    let gamepad_subsys = match sdl.gamepad() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: SDL gamepad subsystem init failed: {e}");
            std::process::exit(1);
        }
    };

    let events = match sdl.event_pump() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: SDL event subsystem init failed: {e}");
            std::process::exit(1);
        }
    };

    let mut src = sdl_source::SdlSource::new(gamepad_subsys, events, &cfg);

    if args.motion_test {
        run_motion_test(&mut src);
        return;
    }

    let server_id: u32 = rand_u32();
    let server_cfg = cfg.to_server_config();
    let server = match server::Server::bind(&server_cfg, src, server_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Error: cannot bind {}:{}: {e}",
                server_cfg.bind_addr, server_cfg.port
            );
            std::process::exit(1);
        }
    };
    log::info!(
        "DSU motion server listening on {}:{} (server id {server_id:#010x})",
        server_cfg.bind_addr,
        server.local_addr().port()
    );
    server.run(server_cfg.send_rate_hz);
}

/// Live sensor readout: shows converted DSU units so signs can be checked
/// by hand (roll right → roll positive? tilt nose-up → pitch?). Adjust the
/// sign constants in `sdl_source::convert_motion` if a channel is mirrored.
fn run_motion_test(src: &mut dyn source::MotionSource) {
    eprintln!("motion test: Ctrl-C to exit; values are DSU units (accel g, gyro deg/s)");
    loop {
        src.poll();
        for c in src.controllers() {
            if !c.connected {
                continue;
            }
            if let Some(m) = src.motion(c.slot) {
                println!(
                    "slot {} {:>30} accel [{:+.2} {:+.2} {:+.2}]g gyro [{:+.2} {:+.2} {:+.2}]deg/s batt {:#x}",
                    c.slot,
                    c.name,
                    m.accel[0],
                    m.accel[1],
                    m.accel[2],
                    m.gyro[0],
                    m.gyro[1],
                    m.gyro[2],
                    c.battery
                );
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn rand_u32() -> u32 {
    // No rand dep for a single value: derive from address entropy + time.
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let p = &t as *const u64 as u64;
    (t ^ p.rotate_left(21) ^ 0x9e3779b97f4a7c15) as u32
}
