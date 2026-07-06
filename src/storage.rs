//! localStorage persistence for the sync connection config + last-synced time.
//! Mirrors ws-study/src/storage.rs (web-sys Storage + serde_json).

use crate::sync::SyncConfig;

const SYNC_CONFIG_KEY: &str = "amp_gh_sync";
const LAST_SYNCED_KEY: &str = "amp_last_synced_at";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn load_sync_config() -> SyncConfig {
    local_storage()
        .and_then(|s| s.get_item(SYNC_CONFIG_KEY).ok().flatten())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save_sync_config(cfg: &SyncConfig) {
    if let (Some(storage), Ok(json)) = (local_storage(), serde_json::to_string(cfg)) {
        let _ = storage.set_item(SYNC_CONFIG_KEY, &json);
    }
}

pub fn load_last_synced_at() -> Option<String> {
    local_storage().and_then(|s| s.get_item(LAST_SYNCED_KEY).ok().flatten())
}

pub fn save_last_synced_at(ts: &str) {
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(LAST_SYNCED_KEY, ts);
    }
}
