//! Data model for the amp-showcase console.
//!
//! The single source of truth is `state.json` in the FDE's private repo. The
//! browser reads/writes it via the GitHub Contents API (see `sync.rs`), and a
//! GitHub Action in that repo executes queued commands and writes results back.

use serde::{Deserialize, Serialize};

/// Root document persisted as `state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConsoleState {
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub org_id: String,
    #[serde(default)]
    pub deployments: Vec<Deployment>,
    #[serde(default)]
    pub commands: Vec<Command>,
}

impl ConsoleState {
    /// A fresh, empty state for a repo that has no `state.json` yet.
    pub fn initial(org_id: &str) -> Self {
        ConsoleState {
            schema_version: 1,
            updated_at: None,
            org_id: org_id.to_string(),
            deployments: Vec::new(),
            commands: Vec::new(),
        }
    }
}

/// One showcase deployment (AMP demo/workshop environment).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Deployment {
    pub sector: String,
    pub slug: String,
    /// Canonical name: `showcase_<sector>_<slug>`.
    pub name: String,
    /// "Online" | "Provisioning" | "Failed" | "Not deployed"
    pub status: String,
    #[serde(default)]
    pub public_url: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl Deployment {
    /// `"<sector>/<slug>"` — the scenario key used by commands.
    pub fn scenario(&self) -> String {
        format!("{}/{}", self.sector, self.slug)
    }
}

/// A queued instruction for the executor GitHub Action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Command {
    /// uuid v4.
    pub id: String,
    /// "provision" | "reset" | "teardown"
    pub action: String,
    /// "<sector>/<slug>" for a single scenario, or None for all.
    #[serde(default)]
    pub scenario: Option<String>,
    pub requested_at: String,
    /// "pending" | "done" | "error"
    pub state: String,
}
