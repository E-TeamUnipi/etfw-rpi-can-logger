//! `logger.conf`: plain `key = value` lines, `#` comments. Lives on the FAT
//! boot partition so it can be edited from any laptop.

use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_PATH: &str = "/boot/logger.conf";
pub const DEFAULT_CTL_SOCKET: &str = "/run/canlog.sock";

#[derive(Debug, Clone, Default)]
pub struct Config {
    map: BTreeMap<String, String>,
}

impl Config {
    pub fn parse(text: &str) -> Config {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').to_string();
                map.insert(k.trim().to_ascii_lowercase(), v);
            }
        }
        Config { map }
    }

    /// Load a config file; a missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Config {
        match std::fs::read(path) {
            // FAT files edited on Windows may have CRLF or a BOM
            Ok(b) => Config::parse(String::from_utf8_lossy(&b).trim_start_matches('\u{feff}')),
            Err(_) => Config::default(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(&key.to_ascii_lowercase()).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }

    pub fn str_or(&self, key: &str, def: &str) -> String {
        self.get(key).unwrap_or(def).to_string()
    }

    pub fn u64_or(&self, key: &str, def: u64) -> u64 {
        self.get(key).and_then(parse_num).unwrap_or(def)
    }

    pub fn bool_or(&self, key: &str, def: bool) -> bool {
        match self.get(key).map(|s| s.to_ascii_lowercase()) {
            Some(s) if ["1", "yes", "on", "true"].contains(&s.as_str()) => true,
            Some(s) if ["0", "no", "off", "false"].contains(&s.as_str()) => false,
            _ => def,
        }
    }

    /// Per-CAN-adapter setting. Looks up, in order: `can.<serial>.<key>`,
    /// `can.<usb port>.<key>`, `can.<ifname>.<key>`, `can.<key>` (default).
    pub fn can_setting(&self, ids: &[&str], key: &str) -> Option<&str> {
        for id in ids.iter().filter(|s| !s.is_empty()) {
            if let Some(v) = self.get(&format!("can.{id}.{key}")) {
                return Some(v);
            }
        }
        self.get(&format!("can.{key}"))
    }

    pub fn ring_dev(&self) -> String {
        self.str_or("ring_device", "/dev/mmcblk0p3")
    }
}

/// Accepts `500000`, `500k`, `1M`, `0x1F`.
pub fn parse_num(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        return u64::from_str_radix(h, 16).ok();
    }
    let lower = s.to_ascii_lowercase();
    if let Some(k) = lower.strip_suffix('k') {
        return k.trim().parse::<f64>().ok().map(|v| (v * 1e3) as u64);
    }
    if let Some(m) = lower.strip_suffix('m') {
        return m.trim().parse::<f64>().ok().map(|v| (v * 1e6) as u64);
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse() {
        let c = Config::parse("a=1\r\n# x\ncan.bitrate = 500k\ncan.ABC123.bitrate=250000 # chassis\ncan.1-1.3.label=\"PT CAN\"\n");
        assert_eq!(c.u64_or("a", 0), 1);
        assert_eq!(c.can_setting(&["XYZ", "1-1.2", "can0"], "bitrate").and_then(parse_num), Some(500_000));
        assert_eq!(c.can_setting(&["abc123", "", "can0"], "bitrate").and_then(parse_num), Some(250_000));
        assert_eq!(c.can_setting(&["", "1-1.3", "can1"], "label"), Some("PT CAN"));
        assert_eq!(parse_num("2M"), Some(2_000_000));
    }
}
