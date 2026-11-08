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
pub const MAX_FAVORITES: usize = 128;
pub const MIN_CONNECT_TIMEOUT_SECS: u64 = 5;
pub const MAX_CONNECT_TIMEOUT_SECS: u64 = 60;
pub const MAX_RECONNECT_ATTEMPTS: u8 = 10;

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

/// Bindable subset of the controls defined by upstream DDNet 20.2.1.
pub const BIND_ACTIONS: [&str; 10] = [
    "MOVE LEFT", "MOVE RIGHT", "JUMP", "JUMP ALT", "FIRE",
    "HOOK", "HOOK ALT", "CHAT", "TEAM CHAT", "SCOREBOARD",
];
const DEFAULT_BINDS: [&str; 10] = [
    "A", "D", "Space", "W", "MouseLeft",
    "MouseRight", "E", "T", "Y", "Tab",
];

pub fn valid_binding(name: &str) -> bool {
    if matches!(name,
        "MouseLeft" | "MouseRight" | "MouseMiddle" | "Space" | "Tab"
        | "Up" | "Down" | "Left" | "Right"
        | "LeftShift" | "RightShift" | "LeftCtrl" | "RightCtrl"
        | "LeftAlt" | "RightAlt" | "Enter" | "Backspace"
        | "PageUp" | "PageDown"
        | "Equal" | "Minus" | "Comma" | "Period"
    ) {
        return true;
    }
    if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
        return true;
    }
    if let Some(num) = name.strip_prefix("Key") {
        return num.len() == 1 && num.as_bytes()[0].is_ascii_digit();
    }
    if let Some(num) = name.strip_prefix('F') {
        return num.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n));
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    pub slots: [String; BIND_ACTIONS.len()],
}
impl Default for KeyBindings {
    fn default() -> Self {
        Self { slots: DEFAULT_BINDS.map(str::to_owned) }
    }
}
impl KeyBindings {
    pub fn uses(&self, key: &str) -> bool {
        self.slots.iter().any(|binding|binding == key)
    }
    pub fn validate(&self) -> bool {
        let mut used = std::collections::HashSet::new();
        self.slots.iter().all(|name|
            valid_binding(name) && used.insert(name.as_str())
        )
    }

    /// Reusing a key swaps the two bindings rather than silently disabling
    /// another action, so every action stays reachable.
    pub fn set(&mut self, index: usize, key: &str) -> bool {
        if index >= self.slots.len() || !valid_binding(key) { return false; }
        if self.slots[index] == key { return false; }
        if let Some(other) = self.slots.iter().position(|name| name == key) {
            self.slots.swap(index, other);
        } else {
            self.slots[index] = key.to_owned();
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSettings {
    pub nickname: String,
    pub search: String,
    pub hide_empty: bool,
    pub last_server: String,
    /// Saved 0.6 IP:port favorites, independent of the master listing.
    pub favorites: Vec<String>,
    pub show_favorites: bool,
    pub connect_timeout_secs: u64,
    pub reconnect_attempts: u8,
    /// In-game menu preferences, all validated on read/write.
    pub show_nameplates: bool,
    pub show_chat: bool,
    pub sound_enabled: bool,
    /// Original DDNet audio mixer master level, 0-100.
    pub sound_volume: u8,
    /// Native window FPS limiter, not a simulation-tick setting.
    pub fps_limit: u16,
    /// Camera scale * 100 (normal gameplay is 150).
    pub zoom_percent: u16,
    pub bindings: KeyBindings,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            nickname: "RustTee".into(),
            search: String::new(),
            hide_empty: false,
            last_server: String::new(),
            favorites: Vec::new(),
            show_favorites: false,
            connect_timeout_secs: 20,
            reconnect_attempts: 3,
            show_nameplates: true,
            show_chat: true,
            sound_enabled: true,
            sound_volume: 100,
            fps_limit: 60,
            zoom_percent: 150,
            bindings: KeyBindings::default(),
        }
    }
}

/// Strict IP:port validation for settings and command-line favorites.
/// Domains and port zero are rejected instead of silently resolving them.
pub fn normalize_endpoint(text: &str) -> Option<String> {
    let address: SocketAddr = text.parse().ok()?;
    if address.port() == 0 { return None; }
    Some(address.to_string())
}

/// Sorted master results are retained, but favorite-only filters apply
/// after text/empty filtering. No heuristic address guessing is used.
pub fn visible_indices(servers: &[ServerEntry], settings: &ClientSettings) -> Vec<usize> {
    servers.iter().enumerate()
        .filter(|(_, server)| {
            matches_filter(server, &settings.search, settings.hide_empty)
                && (!settings.show_favorites || settings.is_favorite(&server.endpoint))
        })
        .map(|(index, _)| index)
        .collect()
}

impl ClientSettings {
    /// Even already playing voices become silent immediately when disabled.
    pub fn effective_volume(&self) -> u8 {
        if self.sound_enabled { self.sound_volume } else { 0 }
    }

    pub fn is_favorite(&self, endpoint: &str) -> bool {
        self.favorites.iter().any(|favorite| favorite == endpoint)
    }

    /// Returns true when the favorite set changed; no duplicate entries.
    pub fn set_favorite(&mut self, endpoint: &str, add: bool) -> Result<bool, BrowserError> {
        let normalized = normalize_endpoint(endpoint).ok_or(BrowserError::InvalidSchema)?;
        if add {
            if self.is_favorite(&normalized) { return Ok(false); }
            if self.favorites.len() >= MAX_FAVORITES { return Err(BrowserError::TooLarge); }
            self.favorites.push(normalized);
            self.favorites.sort_unstable();
            Ok(true)
        } else {
            let count_before = self.favorites.len();
            self.favorites.retain(|item| item != &normalized);
            Ok(self.favorites.len() != count_before)
        }
    }

    pub fn set_connection_options(
        &mut self, timeout_secs: u64, reconnects: u8,
    ) -> Result<(), BrowserError> {
        if !(MIN_CONNECT_TIMEOUT_SECS..=MAX_CONNECT_TIMEOUT_SECS).contains(&timeout_secs)
            || reconnects > MAX_RECONNECT_ATTEMPTS
        {
            return Err(BrowserError::InvalidSchema);
        }
        self.connect_timeout_secs = timeout_secs;
        self.reconnect_attempts = reconnects;
        Ok(())
    }

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
            if let Some(endpoint) = normalize_endpoint(server) {
                settings.last_server = endpoint;
            }
        }
        if let Some(favorites) = obj.get("favorites").and_then(Value::as_array) {
            for favorite in favorites {
                if let Some(addr) = favorite.as_str().and_then(normalize_endpoint) {
                    // Treat invalid or oversized on-disk lists as incomplete.
                    // Loading must not expose unvalidated endpoints.
                    if !settings.is_favorite(&addr) && settings.favorites.len() < MAX_FAVORITES {
                        settings.favorites.push(addr);
                    }
                }
            }
            settings.favorites.sort_unstable();
        }
        if let Some(value) = obj.get("show_favorites").and_then(Value::as_bool) {
            settings.show_favorites = value;
        }
        if let Some(value) = obj.get("connect_timeout_secs").and_then(Value::as_u64) {
            if (MIN_CONNECT_TIMEOUT_SECS..=MAX_CONNECT_TIMEOUT_SECS).contains(&value) {
                settings.connect_timeout_secs = value;
            }
        }
        if let Some(value) = obj.get("reconnect_attempts").and_then(Value::as_u64) {
            if value <= u64::from(MAX_RECONNECT_ATTEMPTS) {
                settings.reconnect_attempts = value as u8;
            }
        }
        if let Some(v) = obj.get("show_nameplates").and_then(Value::as_bool) {
            settings.show_nameplates = v;
        }
        if let Some(v) = obj.get("show_chat").and_then(Value::as_bool) {
            settings.show_chat = v;
        }
        if let Some(v) = obj.get("sound_enabled").and_then(Value::as_bool) {
            settings.sound_enabled = v;
        }
        if let Some(v) = obj.get("sound_volume").and_then(Value::as_u64) {
            if v <= 100 { settings.sound_volume = v as u8; }
        }
        if let Some(v) = obj.get("fps_limit").and_then(Value::as_u64) {
            if [30, 60, 120, 144, 240].contains(&v) {
                settings.fps_limit = v as u16;
            }
        }
        if let Some(v) = obj.get("zoom_percent").and_then(Value::as_u64) {
            if (50..=400).contains(&v) && v.is_multiple_of(10) {
                settings.zoom_percent = v as u16;
            }
        }
        if let Some(entries) = obj.get("bindings").and_then(Value::as_array) {
            if entries.len() == BIND_ACTIONS.len()
                && entries.iter().all(|v|v.as_str().is_some_and(valid_binding))
            {
                let mut bindings = KeyBindings::default();
                for (slot, item) in bindings.slots.iter_mut().zip(entries) {
                    *slot = item.as_str().unwrap_or_default().to_owned();
                }
                if bindings.validate() { settings.bindings = bindings; }
            }
        }
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> Result<(), BrowserError> {
        if self.nickname.is_empty() || self.nickname.len() > 15
            || self.nickname.chars().any(char::is_control)
            || self.search.chars().count() > 64
            || (!self.last_server.is_empty()
                && normalize_endpoint(&self.last_server).is_none())
            || self.favorites.len() > MAX_FAVORITES
            || self.favorites.iter().any(|s| normalize_endpoint(s).as_deref() != Some(s))
            || {
                let mut unique = std::collections::HashSet::new();
                self.favorites.iter().any(|s| !unique.insert(s))
            }
            || !(MIN_CONNECT_TIMEOUT_SECS..=MAX_CONNECT_TIMEOUT_SECS)
                .contains(&self.connect_timeout_secs)
            || self.reconnect_attempts > MAX_RECONNECT_ATTEMPTS
            || ![30, 60, 120, 144, 240].contains(&self.fps_limit)
            || !(50..=400).contains(&self.zoom_percent)
            || !self.zoom_percent.is_multiple_of(10)
            || !self.bindings.validate()
            || self.sound_volume > 100
        {
            return Err(BrowserError::InvalidSchema);
        }
        let json = json!({
            "nickname": self.nickname,
            "search": self.search,
            "hide_empty": self.hide_empty,
            "last_server": self.last_server,
            "favorites": self.favorites,
            "show_favorites": self.show_favorites,
            "connect_timeout_secs": self.connect_timeout_secs,
            "reconnect_attempts": self.reconnect_attempts,
            "show_nameplates": self.show_nameplates,
            "show_chat": self.show_chat,
            "sound_enabled": self.sound_enabled,
            "sound_volume": self.sound_volume,
            "fps_limit": self.fps_limit,
            "zoom_percent": self.zoom_percent,
            "bindings": self.bindings.slots,
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
    fn favorites_are_unique_bounded_and_consistent_with_visible_servers() {
        let servers = parse_master(FIXTURE.as_bytes()).unwrap();
        let mut settings = ClientSettings::default();
        settings.set_favorite("[::1]:8304", true).unwrap();
        assert!(!settings.set_favorite("[::1]:8304", true).unwrap());
        settings.show_favorites = true;
        assert_eq!(visible_indices(&servers, &settings), vec![1]);
        settings.hide_empty = true;
        assert!(visible_indices(&servers, &settings).is_empty());
        settings.hide_empty = false;
        settings.search = "Sunny".into();
        assert!(visible_indices(&servers, &settings).is_empty());
        settings.search.clear();
        assert_eq!(settings.favorites.len(), 1);
        assert_eq!(settings.set_favorite("[::1]:8304", false), Ok(true));
        assert_eq!(settings.set_favorite("[::1]:8304", false), Ok(false));
        assert!(settings.favorites.is_empty());
        assert_eq!(settings.set_favorite("example.com:8303", true), Err(BrowserError::InvalidSchema));
        assert_eq!(settings.set_favorite("127.0.0.1:0", true), Err(BrowserError::InvalidSchema));
        assert_eq!(normalize_endpoint("[::1]:8304"), Some("[::1]:8304".into()));
    }

    #[test]
    fn invalid_connection_options_are_rejected_transactionally() {
        let mut settings = ClientSettings::default();
        settings.set_connection_options(5, 0).unwrap();
        assert_eq!(settings.connect_timeout_secs, 5);
        assert_eq!(settings.reconnect_attempts, 0);
        let baseline = settings.clone();
        assert_eq!(settings.set_connection_options(4, 3), Err(BrowserError::InvalidSchema));
        assert_eq!(settings.set_connection_options(20, 11), Err(BrowserError::InvalidSchema));
        assert_eq!(settings, baseline);
        assert!(settings.set_connection_options(60, 10).is_ok());
        settings.favorites = (1..=MAX_FAVORITES)
            .map(|port| format!("127.0.0.1:{}", port + 4000))
            .collect();
        assert_eq!(settings.set_favorite("127.0.0.1:9000", true), Err(BrowserError::TooLarge));
        assert_eq!(settings.favorites.len(), MAX_FAVORITES);
    }

    #[test]
    fn settings_from_old_versions_have_backward_compatible_defaults() {
        let path = std::env::temp_dir().join(format!(
            "ddnet-rust-browser-settings-{}-compat.json", std::process::id(),
        ));
        fs::write(&path, r#"{"nickname":"ConoF","search":"race","hide_empty":true,
            "last_server":"127.0.0.1:8303"}"#).unwrap();
        let loaded = ClientSettings::load(&path).unwrap();
        assert_eq!(loaded.nickname, "ConoF");
        assert_eq!(loaded.connect_timeout_secs, 20);
        assert_eq!(loaded.reconnect_attempts, 3);
        assert!(loaded.favorites.is_empty());
        assert!(!loaded.show_favorites);
        fs::write(&path, r#"{"favorites":["127.0.0.1:8303","bad","127.0.0.1:8303",
            "[::1]:8305"],"connect_timeout_secs":999,"reconnect_attempts":99}"#).unwrap();
        let loaded = ClientSettings::load(&path).unwrap();
        assert_eq!(loaded.favorites, ["127.0.0.1:8303", "[::1]:8305"]);
        assert_eq!(loaded.connect_timeout_secs, 20);
        assert_eq!(loaded.reconnect_attempts, 3);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn rebind_swaps_conflicts_and_rejects_invalid_names() {
        let mut binds = KeyBindings::default();
        assert!(binds.validate());
        assert!(binds.set(0, "D"));
        assert_eq!(binds.slots[0], "D");
        assert_eq!(binds.slots[1], "A");
        assert!(!binds.set(2, "Escape"));
        assert!(!binds.set(99, "F1"));
        assert!(binds.validate());
        assert!(binds.uses("D"));
        assert!(!binds.uses("Key1"));
        assert!(binds.set(4, "F12"));
        assert_eq!(binds.slots[4], "F12");
        assert!(binds.validate());
    }

    #[test]
    fn bindings_survive_settings_roundtrip_and_invalid_lists_use_defaults() {
        let path=std::env::temp_dir().join(format!(
            "ddnet-rust-bindings-{}.json", std::process::id()));
        let mut options=ClientSettings::default();
        assert!(options.bindings.set(0,"Q"));
        options.save(&path).unwrap();
        assert_eq!(ClientSettings::load(&path).unwrap(), options);
        fs::write(&path,r#"{"bindings":["A","A"]}"#).unwrap();
        assert_eq!(ClientSettings::load(&path).unwrap().bindings,
            KeyBindings::default());
        let _=fs::remove_file(path);
    }

    #[test]
    fn muting_forces_mixer_gain_to_zero_without_forgetting_slider() {
        let mut s=ClientSettings {
            sound_volume: 40, ..ClientSettings::default()
        };
        assert_eq!(s.effective_volume(),40);
        s.sound_enabled=false;
        assert_eq!(s.effective_volume(),0);
        s.sound_enabled=true;
        assert_eq!(s.effective_volume(),40);
    }

    #[test]
    fn master_volume_is_validated_and_migrates_safely() {
        let path = std::env::temp_dir().join(
            format!("ddnet-rust-volume-{}.json",std::process::id()));
        let prefs = ClientSettings {
            sound_volume: 65, ..ClientSettings::default()
        };
        prefs.save(&path).unwrap();
        assert_eq!(ClientSettings::load(&path).unwrap().sound_volume,65);
        fs::write(&path,r#"{"sound_volume":999}"#).unwrap();
        assert_eq!(ClientSettings::load(&path).unwrap().sound_volume,100);
        let invalid = ClientSettings { sound_volume: 101, ..prefs };
        assert_eq!(invalid.save(&path),Err(BrowserError::InvalidSchema));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn game_menu_preferences_persist_and_invalid_values_fall_back() {
        let path = std::env::temp_dir().join(format!(
            "ddnet-rust-menu-prefs-{}.json", std::process::id(),
        ));
        let settings = ClientSettings {
            show_chat: false,
            show_nameplates: false,
            sound_enabled: false,
            fps_limit: 144,
            zoom_percent: 220,
            ..ClientSettings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(ClientSettings::load(&path), Ok(settings.clone()));
        let mut invalid = settings.clone();
        invalid.fps_limit = 999;
        assert_eq!(invalid.save(&path), Err(BrowserError::InvalidSchema));
        fs::write(&path,
            r#"{"nickname":"old","fps_limit":777,"zoom_percent":500,"sound_enabled":false}"#
        ).unwrap();
        let loaded = ClientSettings::load(&path).unwrap();
        assert_eq!(loaded.fps_limit, 60);
        assert_eq!(loaded.zoom_percent, 150);
        assert_eq!(loaded.nickname, "old");
        assert!(!loaded.sound_enabled);
        fs::remove_file(path).unwrap();
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
            favorites: vec!["127.0.0.1:8303".into()],
            show_favorites: true,
            connect_timeout_secs: 12,
            reconnect_attempts: 2,
            ..ClientSettings::default()
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
