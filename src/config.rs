//! TOML configuration with defaults.

use std::path::PathBuf;

use serde::Deserialize;

use crate::server::ServerConfig as CoreConfig;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// UDP port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,
    /// IP address to bind.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Data-packet send rate in Hz.
    #[serde(default = "default_send_rate")]
    pub send_rate_hz: u32,
    /// Only expose gamepads whose name contains one of these substrings.
    /// Absent or empty = expose all gyro-capable gamepads.
    #[serde(default)]
    pub device_filter: Vec<String>,
}

fn default_port() -> u16 {
    crate::dsu::DEFAULT_PORT
}
fn default_bind() -> String {
    "127.0.0.1".to_string()
}
fn default_send_rate() -> u32 {
    100
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind: default_bind(),
            send_rate_hz: default_send_rate(),
            device_filter: Vec::new(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("invalid config: {e}"))
    }

    /// Server-side settings (port/bind/rate) as understood by `server::Server`.
    pub fn to_server_config(&self) -> CoreConfig {
        CoreConfig {
            port: self.port,
            send_rate_hz: self.send_rate_hz,
            bind_addr: self.bind.clone(),
            client_timeout: None,
        }
    }

}

/// Shared filter semantics: empty filter allows everything; otherwise
/// case-sensitive substring match.
pub fn filter_allows(filter: &[String], name: &str) -> bool {
    filter.is_empty() || filter.iter().any(|f| name.contains(f))
}

/// Default config file search order: `$XDG_CONFIG_HOME/8bitdo-motion/config.toml`,
/// then `./8bitdo-motion.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(xdg).join("8bitdo-motion").join("config.toml");
        if p.exists() {
            return Some(p);
        }
    }
    let local = PathBuf::from("8bitdo-motion.toml");
    local.exists().then_some(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_yields_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.port, 26760);
        assert_eq!(c.bind, "127.0.0.1");
        assert_eq!(c.send_rate_hz, 100);
        assert!(c.device_filter.is_empty());
    }

    #[test]
    fn parses_all_fields() {
        let c = Config::parse(
            "port = 1234\nbind = \"0.0.0.0\"\nsend_rate_hz = 60\ndevice_filter = [\"Ultimate\"]\n",
        )
        .unwrap();
        assert_eq!(c.port, 1234);
        assert_eq!(c.bind, "0.0.0.0");
        assert_eq!(c.send_rate_hz, 60);
        assert_eq!(c.device_filter, vec!["Ultimate".to_string()]);
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        assert!(Config::parse("banana = 3").is_err());
        assert!(Config::parse("port = \"abc\"").is_err());
    }

    #[test]
    fn to_server_config_carries_settings() {
        let c = Config::parse("port = 9999\nsend_rate_hz = 60\nbind = \"0.0.0.0\"").unwrap();
        let s = c.to_server_config();
        assert_eq!(s.port, 9999);
        assert_eq!(s.send_rate_hz, 60);
        assert_eq!(s.bind_addr, "0.0.0.0");
    }

    #[test]
    fn device_filter_semantics() {
        let mut c = Config::default();
        assert!(filter_allows(&c.device_filter, "8BitDo Ultimate 2 Wireless"), "empty filter allows all");
        c.device_filter = vec!["Ultimate".into()];
        assert!(filter_allows(&c.device_filter, "8BitDo Ultimate 2 Wireless"));
        assert!(!filter_allows(&c.device_filter, "Sony DualSense"));
        assert!(!filter_allows(&c.device_filter, "ultimate"), "substring match is case-sensitive by design");
    }
}
