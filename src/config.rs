// Runtime configuration.
//
// Source of truth is ~/.config/trident/config.toml, so re-pointing the hub or
// flipping a default never needs a reinstall - the `.mcp.json` entry just runs
// `trident serve-mcp` and reads this file. Environment variables
// (TRIDENT_HUB / TRIDENT_HUB_PORT / TRIDENT_NAME) still override the file for
// one-off runs and tests.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Hub URL to connect to. None => local hub (this machine is the hub).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<String>,
    /// Session name. None => derived from a friendly random name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Add --dangerously-skip-permissions when launching Claude Code.
    pub skip_perms: bool,
    /// Add --rc (remote control) when launching Claude Code.
    pub rc: bool,
    /// Per-peer settings (extra project roots, last chosen working dir).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub peers: BTreeMap<String, PeerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self { hub: None, name: None, skip_perms: false, rc: true, peers: BTreeMap::new() }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerConfig {
    /// SSH username learned at first enlist, used as the default next time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Extra project roots to scan for git repos on this peer (in addition to $HOME).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    /// Working directory chosen last time, used as the default next time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_dir: Option<String>,
}

pub fn peer(name: &str) -> PeerConfig {
    load().peers.get(name).cloned().unwrap_or_default()
}

pub fn set_peer(name: &str, pc: PeerConfig) -> anyhow::Result<()> {
    let mut c = load();
    c.peers.insert(name.to_string(), pc);
    save(&c)
}

pub fn home() -> PathBuf {
    #[cfg(windows)]
    if let Ok(h) = std::env::var("USERPROFILE") {
        return PathBuf::from(h);
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h);
    }
    PathBuf::from(".")
}

pub fn config_path() -> PathBuf {
    home().join(".config").join("trident").join("config.toml")
}

pub fn load() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(c: &Config) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, toml::to_string_pretty(c)?)?;
    Ok(())
}

// --- resolved accessors (env overrides file) -------------------------------

pub fn hub_port() -> u16 {
    std::env::var("TRIDENT_HUB_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8790)
}

/// Hub URL to connect to. Trailing slashes trimmed.
pub fn hub_url() -> String {
    let raw = std::env::var("TRIDENT_HUB")
        .ok()
        .or_else(|| load().hub)
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", hub_port()));
    raw.trim_end_matches('/').to_string()
}

/// Friendly name this session asks the hub for. The hub de-duplicates
/// collisions and tells us the final name in the `registered` event.
pub fn requested_name() -> String {
    std::env::var("TRIDENT_NAME")
        .ok()
        .or_else(|| load().name)
        .unwrap_or_else(random_name)
}

pub fn is_local(hub: &str) -> bool {
    hub.contains("127.0.0.1") || hub.contains("localhost")
}

fn random_name() -> String {
    const ADJ: &[&str] = &[
        "calm", "bold", "swift", "wise", "keen", "lucid", "brave", "sly", "quiet", "sharp",
    ];
    const NOUN: &[&str] = &[
        "otter", "hawk", "fox", "wren", "lynx", "orca", "heron", "ibex", "marten", "crane",
    ];
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let pid = std::process::id() as usize;
    format!("{}-{}", ADJ[nanos % ADJ.len()], NOUN[pid % NOUN.len()])
}
