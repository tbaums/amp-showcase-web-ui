//! GitHub Contents API sync backend.
//!
//! A single `state.json` file in a private repo, written via PUT with the
//! previous blob sha for optimistic concurrency. On 409/422 the caller should
//! re-fetch and retry. Retargeted from ws-study's sync.rs onto `ConsoleState`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use gloo_net::http::Request;
use serde::{Deserialize, Serialize};

use crate::models::ConsoleState;

const API_BASE: &str = "https://api.github.com";

/// Persisted connection config (localStorage) plus the AMP org id.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    pub token: String,
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub org_id: String,
}

impl SyncConfig {
    pub fn is_configured(&self) -> bool {
        !self.token.is_empty() && !self.repo.is_empty()
    }

    pub fn to_github_config(&self) -> GitHubConfig {
        GitHubConfig {
            token: self.token.clone(),
            repo: self.repo.clone(),
            branch: if self.branch.is_empty() {
                "main".into()
            } else {
                self.branch.clone()
            },
            path: if self.path.is_empty() {
                "state.json".into()
            } else {
                self.path.clone()
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitHubConfig {
    pub token: String,
    pub repo: String,
    pub path: String,
    pub branch: String,
}

#[derive(Debug)]
pub struct RemoteState {
    pub state: ConsoleState,
    pub sha: String,
}

#[derive(Debug)]
pub enum SyncError {
    Network(String),
    Http { status: u16, body: String },
    NotFound,
    Conflict,
    Decode(String),
    Serde(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Network(s) => write!(f, "network error: {s}"),
            SyncError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            SyncError::NotFound => write!(f, "remote file not found"),
            SyncError::Conflict => write!(f, "conflict (sha mismatch)"),
            SyncError::Decode(s) => write!(f, "decode error: {s}"),
            SyncError::Serde(s) => write!(f, "serde error: {s}"),
        }
    }
}

impl std::error::Error for SyncError {}

fn contents_url(cfg: &GitHubConfig) -> String {
    format!("{API_BASE}/repos/{}/contents/{}", cfg.repo, cfg.path)
}

fn auth_header(cfg: &GitHubConfig) -> String {
    format!("Bearer {}", cfg.token)
}

#[derive(Deserialize)]
struct ContentsResp {
    content: String,
    encoding: String,
    sha: String,
}

#[derive(Deserialize)]
struct UpdateResp {
    content: UpdateContent,
}

#[derive(Deserialize)]
struct UpdateContent {
    sha: String,
}

pub async fn fetch_state(cfg: &GitHubConfig) -> Result<RemoteState, SyncError> {
    let url = format!("{}?ref={}", contents_url(cfg), cfg.branch);
    let resp = Request::get(&url)
        .header("Authorization", &auth_header(cfg))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| SyncError::Network(e.to_string()))?;

    let status = resp.status();
    if status == 404 {
        return Err(SyncError::NotFound);
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(SyncError::Http { status, body });
    }

    let parsed: ContentsResp = resp
        .json()
        .await
        .map_err(|e| SyncError::Decode(e.to_string()))?;

    if parsed.encoding != "base64" {
        return Err(SyncError::Decode(format!(
            "unexpected encoding: {}",
            parsed.encoding
        )));
    }

    // GitHub wraps base64 at 60 chars with newlines; strip whitespace first.
    let cleaned: String = parsed
        .content
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = BASE64
        .decode(&cleaned)
        .map_err(|e| SyncError::Decode(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|e| SyncError::Decode(e.to_string()))?;
    let state: ConsoleState =
        serde_json::from_str(&text).map_err(|e| SyncError::Serde(e.to_string()))?;

    Ok(RemoteState {
        state,
        sha: parsed.sha,
    })
}

pub async fn push_state(
    cfg: &GitHubConfig,
    state: &ConsoleState,
    prev_sha: Option<&str>,
) -> Result<String, SyncError> {
    let json = serde_json::to_string_pretty(state).map_err(|e| SyncError::Serde(e.to_string()))?;
    let content = BASE64.encode(json.as_bytes());

    let mut body = serde_json::json!({
        "message": format!(
            "Update state.json ({})",
            state.updated_at.clone().unwrap_or_else(|| "unknown".into())
        ),
        "content": content,
        "branch": cfg.branch,
    });
    if let Some(sha) = prev_sha {
        body["sha"] = serde_json::Value::String(sha.to_string());
    }

    let resp = Request::put(&contents_url(cfg))
        .header("Authorization", &auth_header(cfg))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .map_err(|e| SyncError::Network(e.to_string()))?
        .send()
        .await
        .map_err(|e| SyncError::Network(e.to_string()))?;

    let status = resp.status();
    if status == 409 || status == 422 {
        return Err(SyncError::Conflict);
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(SyncError::Http { status, body });
    }

    let parsed: UpdateResp = resp
        .json()
        .await
        .map_err(|e| SyncError::Decode(e.to_string()))?;
    Ok(parsed.content.sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use wasm_bindgen_test::*;

    fn cfg(token: &str, repo: &str) -> SyncConfig {
        SyncConfig {
            token: token.into(),
            repo: repo.into(),
            branch: String::new(),
            path: String::new(),
            org_id: "org-1".into(),
        }
    }

    #[wasm_bindgen_test]
    fn is_configured_requires_both_token_and_repo() {
        assert!(!cfg("", "").is_configured());
        assert!(!cfg("tok", "").is_configured());
        assert!(!cfg("", "owner/repo").is_configured());
        assert!(cfg("tok", "owner/repo").is_configured());
    }

    #[wasm_bindgen_test]
    fn github_config_defaults_branch_and_path() {
        let gh = cfg("tok", "owner/state").to_github_config();
        assert_eq!(gh.branch, "main");
        assert_eq!(gh.path, "state.json");
    }

    #[wasm_bindgen_test]
    fn github_config_respects_explicit_branch_and_path() {
        let mut c = cfg("tok", "owner/state");
        c.branch = "prod".into();
        c.path = "envs/demo.json".into();
        let gh = c.to_github_config();
        assert_eq!(gh.branch, "prod");
        assert_eq!(gh.path, "envs/demo.json");
    }

    #[wasm_bindgen_test]
    fn contents_url_and_auth_header_match_the_github_contract() {
        let gh = cfg("secret-token", "tbaums/my-state").to_github_config();
        assert_eq!(
            contents_url(&gh),
            "https://api.github.com/repos/tbaums/my-state/contents/state.json"
        );
        assert_eq!(auth_header(&gh), "Bearer secret-token");
    }

    #[wasm_bindgen_test]
    fn state_survives_the_base64_wire_round_trip() {
        // fetch_state base64-decodes the Contents API payload (which GitHub
        // wraps at 60 cols); push_state base64-encodes it. Prove a state
        // survives encode -> whitespace-wrap -> decode -> parse intact.
        let state = ConsoleState::initial("org-xyz");
        let encoded = B64.encode(serde_json::to_string(&state).unwrap());
        let wrapped = encoded
            .as_bytes()
            .chunks(60)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let cleaned: String = wrapped.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = B64.decode(&cleaned).unwrap();
        let back: ConsoleState = serde_json::from_str(&String::from_utf8(bytes).unwrap()).unwrap();
        assert_eq!(back.org_id, "org-xyz");
    }
}
