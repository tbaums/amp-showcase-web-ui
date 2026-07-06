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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn initial_state_is_empty_and_versioned() {
        let s = ConsoleState::initial("org-123");
        assert_eq!(s.schema_version, 1);
        assert_eq!(s.org_id, "org-123");
        assert!(s.deployments.is_empty());
        assert!(s.commands.is_empty());
    }

    #[wasm_bindgen_test]
    fn deployment_scenario_key_is_sector_slash_slug() {
        let d = Deployment {
            sector: "financial-services".into(),
            slug: "test-drive".into(),
            name: "showcase_financial_services_test_drive".into(),
            status: "Online".into(),
            public_url: None,
            updated_at: None,
        };
        assert_eq!(d.scenario(), "financial-services/test-drive");
    }

    #[wasm_bindgen_test]
    fn state_json_round_trips() {
        let mut s = ConsoleState::initial("org-1");
        s.deployments.push(Deployment {
            sector: "pharma".into(),
            slug: "payload".into(),
            name: "showcase_pharma_payload".into(),
            status: "Online".into(),
            public_url: Some("https://x.crewai.com".into()),
            updated_at: Some("2026-07-06T00:00:00Z".into()),
        });
        let json = serde_json::to_string(&s).unwrap();
        let back: ConsoleState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.org_id, "org-1");
        assert_eq!(back.deployments, s.deployments);
    }

    #[wasm_bindgen_test]
    fn parses_a_minimal_document_via_serde_defaults() {
        // The executor (or a hand-created initial file) may omit optional
        // fields — #[serde(default)] must tolerate that, not error.
        let s: ConsoleState = serde_json::from_str(r#"{"schema_version":1}"#).unwrap();
        assert_eq!(s.schema_version, 1);
        assert!(s.deployments.is_empty());
        assert!(s.commands.is_empty());
        assert_eq!(s.org_id, "");
    }

    #[wasm_bindgen_test]
    fn parses_an_executor_shaped_result_document() {
        // A state.json the executor Action would write back: a finished command
        // and an updated deployment status. The browser must read it cleanly.
        let json = r#"{
          "schema_version": 1,
          "org_id": "9e9df64f",
          "deployments": [
            {"sector":"pharma","slug":"payload","name":"showcase_pharma_payload",
             "status":"Online","public_url":"https://p.crewai.com","updated_at":"t"}
          ],
          "commands": [
            {"id":"abc","action":"reset","scenario":"pharma/payload",
             "requested_at":"t","state":"done"}
          ]
        }"#;
        let s: ConsoleState = serde_json::from_str(json).unwrap();
        assert_eq!(s.deployments[0].status, "Online");
        assert_eq!(s.commands[0].state, "done");
        assert_eq!(s.commands[0].scenario.as_deref(), Some("pharma/payload"));
    }
}
