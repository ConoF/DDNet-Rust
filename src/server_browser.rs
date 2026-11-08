//! Official DDNet master-list client and persistent, non-secret settings.
//! The list is public; listing a server does NOT prove 0.6 playability.
//!
//! Native HTTPS (rustls via ureq) is used only for the four known master
//! URLs. Maximum response size and DNS/network timeouts are bounded.

use std::fs;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

pub const MASTER_URLS: [&str; 4] = [
    "https://master1.ddnet.org/ddnet/15/servers.json",
    "https://master2.ddnet.org/ddnet/15/servers.json",
    "https://master3.ddnet.org/ddnet/15/servers.json",
    "https://master4.ddnet.org/ddnet/15/servers.json",
];
pub const MAX_MASTER_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SERVERS: usize = 20_000;
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserError {
    InvalidJson,
    InvalidSchema,
    TooLarge,
    NoCompatibleServers,
    Network(String),
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    /// Address from the official 0.6 UDP endpoint, without scheme.
    /// The IP and port are validated, not guessed from metadata.
    pub endpoint: String,
    pub name: String,
    pub map: String,
    pub game_type: String,
    pub location: String,
    pub players: u16,
    pub max_players: u16,
    pub password: bool,
}

fn display_text(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Only trust syntactically valid 0.6 UDP endpoints. No 0.7 fallback:
/// our transport currently handles only the original 0.6 handshake.
pub fn endpoint_06(address: &str) -> Option<String> {
    let remainder = address.strip_prefix("tw-0.6+udp://")?;
    let remote: SocketAddr = remainder.parse().ok()?;
    if remote.port() == 0 { return None; }
    Some(remote.to_string())
}

/// Bounded parser of the documented DDNet master response schema.
/// Malformed individual community-server metadata is skipped while
/// a malformed top-level response fails in a deterministic way.
pub fn parse_master(bytes: &[u8]) -> Result<Vec<ServerEntry>, BrowserError> {
    if bytes.len() > MAX_MASTER_BYTES {
        return Err(BrowserError::TooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| BrowserError::InvalidJson)?;
    let servers = value.get("servers").and_then(Value::as_array)
        .ok_or(BrowserError::InvalidSchema)?;
    if servers.len() > MAX_SERVERS {
        return Err(BrowserError::TooLarge);
    }

    let mut listed = Vec::new();
    let mut known = std::collections::HashSet::new();
    for server in servers {
        let Some(addresses) = server.get("addresses").and_then(Value::as_array) else {
            continue;
        };
        let Some(address) = addresses.iter().filter_map(Value::as_str)
            .find_map(endpoint_06) else {
            continue;
        };
        if !known.insert(address.clone()) { continue; }

        let Some(info) = server.get("info").and_then(Value::as_object) else {
            continue;
        };
        let Some(name) = info.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = display_text(name, 90);
        if name.trim().is_empty() { continue; }
        let map = info.get("map").and_then(|value| {
            value.as_str().or_else(|| value.get("name").and_then(Value::as_str))
        }).unwrap_or("unknown");
        let game_type = info.get("game_type").and_then(Value::as_str)
            .unwrap_or("unknown");
        let location = server.get("location").and_then(Value::as_str)
            .unwrap_or("");
        let players = info.get("num_players").and_then(Value::as_u64).unwrap_or(0)
            .min(u64::from(u16::MAX)) as u16;
        let max_players = info.get("max_players").and_then(Value::as_u64).unwrap_or(0)
            .min(u64::from(u16::MAX)) as u16;
        let password = info.get("passworded").and_then(Value::as_bool)
            .unwrap_or(false);
        listed.push(ServerEntry {
            endpoint: address,
            name,
            map: display_text(map, 64),
            game_type: display_text(game_type, 40),
            location: display_text(location, 32),
            players, max_players, password,
        });
    }
    if listed.is_empty() { return Err(BrowserError::NoCompatibleServers); }
    listed.sort_by(|a, b| b.players.cmp(&a.players)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.endpoint.cmp(&b.endpoint)));
    Ok(listed)
}

pub fn matches_filter(server: &ServerEntry, query: &str, hide_empty: bool) -> bool {
    if hide_empty && server.players == 0 { return false; }
    let filter = query.trim().to_lowercase();
    filter.is_empty()
        || [&server.name, &server.map, &server.game_type, &server.location,
            &server.endpoint].iter().any(|s| s.to_lowercase().contains(&filter))
}

/// Uses a single time-bounded HTTP agent and fails over across the four
/// upstream mirrors. A network error never injects an arbitrary server
/// address into the client's UDP connect path.
pub fn fetch_master() -> Result<(Vec<ServerEntry>, &'static str), BrowserError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout_read(Duration::from_secs(8))
        .timeout_write(Duration::from_secs(4))
        .build();
    let mut last = String::new();
    for &url in &MASTER_URLS {
        let response = match agent.get(url).call() {
            Ok(response) => response,
            Err(error) => {
                last = format!("{url}: {error}");
                continue;
            }
        };
        let mut bytes = Vec::new();
        let downloaded = response.into_reader()
            .take((MAX_MASTER_BYTES + 1) as u64)
            .read_to_end(&mut bytes);
        if let Err(error) = downloaded {
            last = format!("{url}: {error}");
            continue;
        }
        if bytes.len() > MAX_MASTER_BYTES {
            last = format!("{url}: too many bytes");
            continue;
        }
        match parse_master(&bytes) {
            Ok(servers) => return Ok((servers, url)),
            Err(error) => last = format!("{url}: {error:?}"),
        }
    }
    Err(BrowserError::Network(last))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSettings {
    pub nickname: String,
    pub search: String,
    pub hide_empty: bool,
    pub last_server: String,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            nickname: "RustTee".into(),
            search: String::new(),
            hide_empty: false,
            last_server: String::new(),
        }
    }
}

impl ClientSettings {
    /// If DDNET_RUST_CONFIG is set, use exactly that path (useful in CI).
    /// Otherwise use standard Windows %APPDATA% / XDG directories.
    pub fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os("DDNET_RUST_CONFIG") {
            return PathBuf::from(path);
        }
        let root = std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        root.join("ddnet-rust").join("settings.json")
    }

    pub fn load(path: &Path) -> Result<Self, BrowserError> {
        let metadata = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(BrowserError::Io(e.to_string())),
        };
        if metadata.len() > MAX_SETTINGS_BYTES {
            return Err(BrowserError::TooLarge);
        }
        let bytes = fs::read(path).map_err(|e| BrowserError::Io(e.to_string()))?;
        let obj: Value = serde_json::from_slice(&bytes)
            .map_err(|_| BrowserError::InvalidJson)?;
        let mut settings = Self::default();
        if let Some(name) = obj.get("nickname").and_then(Value::as_str) {
            if !name.is_empty() && name.len() <= 15 && !name.chars().any(char::is_control) {
                settings.nickname = name.to_owned();
            }
        }
        if let Some(query) = obj.get("search").and_then(Value::as_str) {
            settings.search = display_text(query, 64);
        }
        if let Some(hide_empty) = obj.get("hide_empty").and_then(Value::as_bool) {
            settings.hide_empty = hide_empty;
        }
        if let Some(server) = obj.get("last_server").and_then(Value::as_str) {
            // The selected endpoint is saved only if it is a valid IP:port.
            if server.parse::<SocketAddr>().is_ok() {
                settings.last_server = server.to_owned();
            }
        }
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> Result<(), BrowserError> {
        if self.nickname.is_empty() || self.nickname.len() > 15
            || self.nickname.chars().any(char::is_control)
            || self.search.chars().count() > 64
            || (!self.last_server.is_empty()
                && self.last_server.parse::<SocketAddr>().is_err())
        {
            return Err(BrowserError::InvalidSchema);
        }
        let json = json!({
            "nickname": self.nickname,
            "search": self.search,
            "hide_empty": self.hide_empty,
            "last_server": self.last_server,
        });
        let data = serde_json::to_vec_pretty(&json)
            .map_err(|_| BrowserError::InvalidSchema)?;
        if data.len() > MAX_SETTINGS_BYTES as usize {
            return Err(BrowserError::TooLarge);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| BrowserError::Io(e.to_string()))?;
        }
        fs::write(path, data).map_err(|e| BrowserError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "servers": [
            {"addresses":["tw-0.7+udp://127.0.0.3:8303",
                          "tw-0.6+udp://127.0.0.1:8303"],
             "location":"eu",
             "info":{"name":"Great Race","map":{"name":"Sunny"},"game_type":"DDRace",
                     "num_players":12,"max_players":64}},
            {"addresses":["tw-0.6+udp://[::1]:8304"],
             "info":{"name":"<EU> Race","map":"Kobra","game_type":"DDRace",
                     "num_players":0,"max_players":64}},
            {"addresses":["tw-0.7+udp://127.0.0.9:8303"],
             "info":{"name":"0.7-only","map":"A","num_players":30}},
            {"addresses":["tw-0.6+udp://127.0.0.1:8303"],
             "info":{"name":"Duplicate","map":"A","num_players":3}},
            {"addresses":["tw-0.6+udp://not-an-ip:8303"],
             "info":{"name":"Invalid","map":"A","num_players":10}}
        ]
    }"#;

    #[test]
    fn official_master_layout_filters_06_and_deduplicates_ip_addresses() {
        let servers = parse_master(FIXTURE.as_bytes()).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].endpoint, "127.0.0.1:8303");
        assert_eq!(servers[0].map, "Sunny");
        assert_eq!(servers[0].players, 12);
        assert_eq!(servers[1].endpoint, "[::1]:8304");
        assert!(matches_filter(&servers[0], "sunny", false));
        assert!(matches_filter(&servers[0], "rAcE", false));
        assert!(matches_filter(&servers[0], "8303", false));
        assert!(!matches_filter(&servers[1], "", true));
        assert!(!matches_filter(&servers[0], "ctf", false));
    }

    #[test]
    fn oversized_or_malformed_master_never_produces_addresses() {
        assert_eq!(parse_master(br#"{"servers":{}}"#), Err(BrowserError::InvalidSchema));
        assert_eq!(parse_master(b"not json"), Err(BrowserError::InvalidJson));
        assert_eq!(parse_master(&vec![0; MAX_MASTER_BYTES + 1]), Err(BrowserError::TooLarge));
        assert_eq!(parse_master(br#"{"servers":[]}"#), Err(BrowserError::NoCompatibleServers));
        assert_eq!(endpoint_06("tw-0.7+udp://127.0.0.1:8303"), None);
        assert_eq!(endpoint_06("tw-0.6+udp://example.com:8303"), None);
        assert_eq!(endpoint_06("tw-0.6+udp://127.0.0.1:0"), None);
        assert_eq!(endpoint_06("tw-0.6+udp://[::1]:8303"), Some("[::1]:8303".into()));
    }

    #[test]
    fn settings_roundtrip_and_bad_endpoint_are_sanitized() {
        let path = std::env::temp_dir().join(format!(
            "ddnet-rust-browser-settings-{}-{}.json", std::process::id(), "roundtrip",
        ));
        let settings = ClientSettings {
            nickname: "ConoF".into(),
            search: "DDRace".into(),
            hide_empty: true,
            last_server: "127.0.0.1:8303".into(),
        };
        settings.save(&path).unwrap();
        assert_eq!(ClientSettings::load(&path), Ok(settings.clone()));
        fs::write(&path, r#"{"nickname":"bad\u0000name","last_server":"bad:host"}"#).unwrap();
        let loaded = ClientSettings::load(&path).unwrap();
        assert_eq!(loaded.nickname, "RustTee");
        assert!(loaded.last_server.is_empty());
        fs::remove_file(path).unwrap();
    }
}
