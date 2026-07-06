use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use crate::models::{Command, ConsoleState, Deployment};
use crate::storage;
use crate::sync::{self, SyncConfig, SyncError};

// ── Navigation ──────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Debug)]
pub enum View {
    Setup,
    Dashboard,
}

// ── Global state ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct AppState {
    pub state: RwSignal<ConsoleState>,
    pub config: RwSignal<SyncConfig>,
    pub view: RwSignal<View>,
    pub toast: RwSignal<Option<String>>,
    pub sync_sha: RwSignal<Option<String>>,
    pub last_synced_at: RwSignal<Option<String>>,
    pub syncing: RwSignal<bool>,
}

impl AppState {
    pub fn navigate(&self, v: View) {
        self.view.set(v);
    }

    pub fn show_toast(&self, msg: impl Into<String>) {
        let toast = self.toast;
        toast.set(Some(msg.into()));
        let cb = Closure::once(move || toast.set(None));
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref::<js_sys::Function>(),
                2800,
            );
        }
        cb.forget();
    }
}

// ── App root ──────────────────────────────────────────────────────────────────

#[component]
pub fn App() -> impl IntoView {
    let cfg = storage::load_sync_config();
    let configured = cfg.is_configured();

    let state = AppState {
        state: RwSignal::new(ConsoleState::initial(&cfg.org_id)),
        config: RwSignal::new(cfg),
        view: RwSignal::new(if configured { View::Dashboard } else { View::Setup }),
        toast: RwSignal::new(None),
        sync_sha: RwSignal::new(None),
        last_synced_at: RwSignal::new(storage::load_last_synced_at()),
        syncing: RwSignal::new(false),
    };
    provide_context(state);

    // Boot: if we already have a config, pull the current state.json.
    if configured {
        spawn_local(async move {
            let gh = state.config.get_untracked().to_github_config();
            match sync::fetch_state(&gh).await {
                Ok(remote) => {
                    state.sync_sha.set(Some(remote.sha));
                    state.state.set(remote.state);
                    let ts = current_datetime();
                    storage::save_last_synced_at(&ts);
                    state.last_synced_at.set(Some(ts));
                }
                Err(SyncError::NotFound) => {
                    state.show_toast("No state.json yet — create one in Setup");
                }
                Err(e) => leptos::logging::warn!("Boot pull failed: {e}"),
            }
        });
    }

    view! {
        <div id="app">
            <CurrentView/>
            <Toast/>
        </div>
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

#[component]
fn CurrentView() -> impl IntoView {
    let state = expect_context::<AppState>();
    move || match state.view.get() {
        View::Setup => view! { <SetupView/> }.into_any(),
        View::Dashboard => view! { <DashboardView/> }.into_any(),
    }
}

// ── Toast ─────────────────────────────────────────────────────────────────────

#[component]
fn Toast() -> impl IntoView {
    let state = expect_context::<AppState>();
    move || {
        state
            .toast
            .get()
            .map(|msg| view! { <div class="toast">{msg}</div> })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

pub fn current_datetime() -> String {
    js_sys::Date::new_0()
        .to_iso_string()
        .as_string()
        .unwrap_or_default()
}

fn status_class(status: &str) -> &'static str {
    match status {
        "Online" => "badge badge-green",
        "Provisioning" => "badge badge-amber",
        "Failed" => "badge badge-red",
        _ => "badge badge-grey",
    }
}

fn cmd_state_class(state: &str) -> &'static str {
    match state {
        "done" => "badge badge-green",
        "error" => "badge badge-red",
        _ => "badge badge-amber",
    }
}

/// Optimistically fold a queued command into a ConsoleState: nudge the affected
/// deployment status(es) and append the command. The executor Action later
/// overwrites these with real results.
fn apply_command(cs: &mut ConsoleState, cmd: &Command) {
    let target_status: Option<&str> = match cmd.action.as_str() {
        "teardown" => Some("Not deployed"),
        "provision" | "reset" => Some("Provisioning"),
        _ => None,
    };
    if let Some(status) = target_status {
        for d in cs.deployments.iter_mut() {
            let matches = cmd
                .scenario
                .as_deref()
                .map_or(true, |sc| d.scenario() == sc);
            if matches {
                d.status = status.to_string();
                d.updated_at = Some(cmd.requested_at.clone());
            }
        }
    }
    cs.commands.push(cmd.clone());
    cs.updated_at = Some(cmd.requested_at.clone());
}

fn on_push_ok(state: AppState, new_sha: String) {
    state.sync_sha.set(Some(new_sha));
    let ts = current_datetime();
    storage::save_last_synced_at(&ts);
    state.last_synced_at.set(Some(ts));
    state.show_toast("Command queued ↑");
}

/// Enqueue a command into state.json: apply it locally, then push. On a sha
/// conflict, re-fetch the remote, re-apply the command onto the fresh copy
/// (so we don't clobber the executor's writes) and push again.
fn enqueue_command(state: AppState, action: &str, scenario: Option<String>) {
    let cmd = Command {
        id: uuid::Uuid::new_v4().to_string(),
        action: action.to_string(),
        scenario,
        requested_at: current_datetime(),
        state: "pending".to_string(),
    };
    state.state.update(|cs| apply_command(cs, &cmd));

    let gh = state.config.get_untracked().to_github_config();
    let snapshot = state.state.get_untracked();
    let sha = state.sync_sha.get_untracked();
    state.syncing.set(true);
    spawn_local(async move {
        match sync::push_state(&gh, &snapshot, sha.as_deref()).await {
            Ok(new_sha) => on_push_ok(state, new_sha),
            Err(SyncError::Conflict) => match sync::fetch_state(&gh).await {
                Ok(remote) => {
                    let mut merged = remote.state;
                    apply_command(&mut merged, &cmd);
                    match sync::push_state(&gh, &merged, Some(&remote.sha)).await {
                        Ok(new_sha) => {
                            state.state.set(merged);
                            on_push_ok(state, new_sha);
                        }
                        Err(e) => state.show_toast(format!("Push failed: {e}")),
                    }
                }
                Err(e) => state.show_toast(format!("Refetch failed: {e}")),
            },
            Err(e) => state.show_toast(format!("Push failed: {e}")),
        }
        state.syncing.set(false);
    });
}

fn refresh(state: AppState) {
    let gh = state.config.get_untracked().to_github_config();
    state.syncing.set(true);
    spawn_local(async move {
        match sync::fetch_state(&gh).await {
            Ok(remote) => {
                state.sync_sha.set(Some(remote.sha));
                state.state.set(remote.state);
                let ts = current_datetime();
                storage::save_last_synced_at(&ts);
                state.last_synced_at.set(Some(ts));
                state.show_toast("Refreshed ↓");
            }
            Err(SyncError::NotFound) => {
                state.show_toast("No state.json yet — create one in Setup")
            }
            Err(e) => state.show_toast(format!("Refresh failed: {e}")),
        }
        state.syncing.set(false);
    });
}

// ── Setup view ────────────────────────────────────────────────────────────────

#[component]
fn SetupView() -> impl IntoView {
    let state = expect_context::<AppState>();
    let saved = state.config.get_untracked();

    let token = RwSignal::new(saved.token);
    let repo = RwSignal::new(if saved.repo.is_empty() {
        "you/amp-showcase-state".to_string()
    } else {
        saved.repo
    });
    let branch = RwSignal::new(if saved.branch.is_empty() {
        "main".to_string()
    } else {
        saved.branch
    });
    let org_id = RwSignal::new(saved.org_id);

    let status: RwSignal<Option<String>> = RwSignal::new(None);
    let not_found = RwSignal::new(false);

    let build_cfg = move || SyncConfig {
        token: token.get_untracked(),
        repo: repo.get_untracked(),
        branch: branch.get_untracked(),
        path: "state.json".to_string(),
        org_id: org_id.get_untracked(),
    };

    let connect = move |_| {
        let cfg = build_cfg();
        storage::save_sync_config(&cfg);
        state.config.set(cfg.clone());
        status.set(Some("Connecting…".to_string()));
        not_found.set(false);
        let gh = cfg.to_github_config();
        spawn_local(async move {
            match sync::fetch_state(&gh).await {
                Ok(remote) => {
                    state.sync_sha.set(Some(remote.sha.clone()));
                    let short = remote.sha[..7.min(remote.sha.len())].to_string();
                    state.state.set(remote.state);
                    let ts = current_datetime();
                    storage::save_last_synced_at(&ts);
                    state.last_synced_at.set(Some(ts));
                    status.set(Some(format!("OK — connected (sha {short})")));
                    state.navigate(View::Dashboard);
                }
                Err(SyncError::NotFound) => {
                    not_found.set(true);
                    status.set(Some("state.json not found in this repo.".to_string()));
                }
                Err(e) => status.set(Some(format!("Error: {e}"))),
            }
        });
    };

    let create_initial = move |_| {
        let cfg = build_cfg();
        storage::save_sync_config(&cfg);
        state.config.set(cfg.clone());
        let gh = cfg.to_github_config();
        let mut init = ConsoleState::initial(&cfg.org_id);
        init.updated_at = Some(current_datetime());
        spawn_local(async move {
            match sync::push_state(&gh, &init, None).await {
                Ok(new_sha) => {
                    state.sync_sha.set(Some(new_sha));
                    state.state.set(init);
                    let ts = current_datetime();
                    storage::save_last_synced_at(&ts);
                    state.last_synced_at.set(Some(ts));
                    state.show_toast("Created state.json ✓");
                    state.navigate(View::Dashboard);
                }
                Err(e) => status.set(Some(format!("Create failed: {e}"))),
            }
        });
    };

    let is_configured = move || state.config.get().is_configured();

    view! {
        <div class="page">
            <header class="topbar">
                <div class="wordmark">
                    <span class="wm-crew">"crewai"</span>
                    <span class="wm-sep">"/"</span>
                    <span class="wm-app">"amp-showcase console"</span>
                </div>
            </header>

            <div class="section-label">"Setup"</div>
            <p class="muted mb">
                "Connect a private state repo. All state lives in a single "
                <code>"state.json"</code>" the console reads and writes via the GitHub Contents API."
            </p>

            <div class="card">
                <div class="form-group">
                    <label>"GitHub token (fine-grained, single-repo Contents R/W)"</label>
                    <input
                        type="password"
                        placeholder="github_pat_…"
                        prop:value=move || token.get()
                        on:input=move |e| token.set(event_target_value(&e))
                    />
                </div>
                <div class="form-group">
                    <label>"State repo (owner/repo)"</label>
                    <input
                        type="text"
                        placeholder="you/amp-showcase-state"
                        prop:value=move || repo.get()
                        on:input=move |e| repo.set(event_target_value(&e))
                    />
                </div>
                <div class="row">
                    <div class="form-group flex-1">
                        <label>"Branch"</label>
                        <input
                            type="text"
                            prop:value=move || branch.get()
                            on:input=move |e| branch.set(event_target_value(&e))
                        />
                    </div>
                    <div class="form-group flex-1">
                        <label>"AMP org id"</label>
                        <input
                            type="text"
                            placeholder="org_…"
                            prop:value=move || org_id.get()
                            on:input=move |e| org_id.set(event_target_value(&e))
                        />
                    </div>
                </div>

                <div class="row">
                    <button class="btn btn-primary" on:click=connect>"Connect"</button>
                    {move || {
                        if is_configured() {
                            view! {
                                <button
                                    class="btn btn-ghost"
                                    on:click=move |_| state.navigate(View::Dashboard)
                                >
                                    "Back to dashboard"
                                </button>
                            }
                            .into_any()
                        } else {
                            ().into_any()
                        }
                    }}
                </div>

                {move || {
                    status
                        .get()
                        .map(|s| view! { <div class="status-line">{s}</div> })
                }}

                {move || {
                    if not_found.get() {
                        view! {
                            <div class="notice">
                                <span>"This repo has no "<code>"state.json"</code>" yet."</span>
                                <button class="btn btn-sm btn-secondary" on:click=create_initial>
                                    "Create initial state.json"
                                </button>
                            </div>
                        }
                        .into_any()
                    } else {
                        ().into_any()
                    }
                }}
            </div>

            <div class="section-label mt">"Token scope"</div>
            <p class="muted">
                "Use a fine-grained PAT limited to the single private state repo with "
                <strong>"Contents: Read and write"</strong>". The console never calls the AMP API "
                "directly (CORS) — it enqueues commands that a GitHub Action in the repo executes."
            </p>
        </div>
    }
}

// ── Dashboard view ──────────────────────────────────────────────────────────────

#[component]
fn DashboardView() -> impl IntoView {
    let state = expect_context::<AppState>();

    let org_id = move || {
        let o = state.state.get().org_id;
        if o.is_empty() {
            state.config.get().org_id
        } else {
            o
        }
    };
    let repo = move || state.config.get().repo;
    let last_synced = move || {
        state
            .last_synced_at
            .get()
            .unwrap_or_else(|| "never".to_string())
    };
    let syncing = move || state.syncing.get();

    let deployments = move || state.state.get().deployments;
    let commands = move || state.state.get().commands;

    view! {
        <div class="page">
            <header class="topbar">
                <div class="wordmark">
                    <span class="wm-crew">"crewai"</span>
                    <span class="wm-sep">"/"</span>
                    <span class="wm-app">"amp-showcase console"</span>
                </div>
                <button class="btn btn-ghost btn-sm" on:click=move |_| state.navigate(View::Setup)>
                    "Setup"
                </button>
            </header>

            <div class="meta-bar">
                <div class="meta-item">
                    <span class="meta-k">"org"</span>
                    <span class="meta-v">{org_id}</span>
                </div>
                <div class="meta-item">
                    <span class="meta-k">"repo"</span>
                    <span class="meta-v">{repo}</span>
                </div>
                <div class="meta-item">
                    <span class="meta-k">"synced"</span>
                    <span class="meta-v">{last_synced}</span>
                </div>
                <div class="meta-item meta-right">
                    <span class="sync-dot" class:on=syncing></span>
                    <button
                        class="btn btn-secondary btn-sm"
                        prop:disabled=syncing
                        on:click=move |_| refresh(state)
                    >
                        {move || if syncing() { "Syncing…" } else { "Refresh" }}
                    </button>
                </div>
            </div>

            <div class="toolbar">
                <div class="section-label">"Deployments"</div>
                <div class="toolbar-actions">
                    <button
                        class="btn btn-sm btn-secondary"
                        on:click=move |_| enqueue_command(state, "provision", None)
                    >
                        "Provision all"
                    </button>
                    <button
                        class="btn btn-sm btn-secondary"
                        on:click=move |_| enqueue_command(state, "reset", None)
                    >
                        "Reset all"
                    </button>
                </div>
            </div>

            {move || {
                if deployments().is_empty() {
                    view! {
                        <div class="empty">
                            <div class="empty-title">"No deployments"</div>
                            <div class="muted">
                                "Nothing here yet. Enqueue "<code>"Provision all"</code>
                                " or add scenarios to "<code>"state.json"</code>", then Refresh."
                            </div>
                        </div>
                    }
                        .into_any()
                } else {
                    view! {
                        <div class="table-wrap">
                            <table class="grid">
                                <thead>
                                    <tr>
                                        <th>"Scenario"</th>
                                        <th>"Name"</th>
                                        <th>"Status"</th>
                                        <th>"Public URL"</th>
                                        <th>"Updated"</th>
                                        <th class="ta-right">"Actions"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    <For
                                        each=deployments
                                        key=|d| d.name.clone()
                                        children=move |d: Deployment| { deployment_row(state, d) }
                                    />
                                </tbody>
                            </table>
                        </div>
                    }
                        .into_any()
                }
            }}

            <div class="section-label mt">"Pending commands"</div>
            {move || {
                let cmds = commands();
                if cmds.is_empty() {
                    view! { <div class="muted mb">"No commands queued."</div> }.into_any()
                } else {
                    view! {
                        <div class="table-wrap">
                            <table class="grid">
                                <thead>
                                    <tr>
                                        <th>"Action"</th>
                                        <th>"Scenario"</th>
                                        <th>"State"</th>
                                        <th>"Requested"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    <For
                                        each=commands
                                        key=|c| c.id.clone()
                                        children=move |c: Command| {
                                            let scope = c
                                                .scenario
                                                .clone()
                                                .unwrap_or_else(|| "all".to_string());
                                            view! {
                                                <tr>
                                                    <td class="mono-strong">{c.action.clone()}</td>
                                                    <td>{scope}</td>
                                                    <td>
                                                        <span class=cmd_state_class(&c.state)>
                                                            {c.state.clone()}
                                                        </span>
                                                    </td>
                                                    <td class="muted">{c.requested_at.clone()}</td>
                                                </tr>
                                            }
                                        }
                                    />
                                </tbody>
                            </table>
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

/// One deployment table row with per-row action buttons.
fn deployment_row(state: AppState, d: Deployment) -> impl IntoView {
    let scenario = d.scenario();
    let not_deployed = d.status == "Not deployed";

    let url_cell = match d.public_url.clone() {
        Some(url) if !url.is_empty() => {
            let href = url.clone();
            view! {
                <a class="link" href=href target="_blank" rel="noreferrer">
                    {url}
                </a>
            }
            .into_any()
        }
        _ => view! { <span class="muted">"—"</span> }.into_any(),
    };

    let updated = d.updated_at.clone().unwrap_or_else(|| "—".to_string());

    let sc_provision = scenario.clone();
    let sc_reset = scenario.clone();
    let sc_teardown = scenario.clone();

    view! {
        <tr>
            <td class="mono-strong">{scenario.clone()}</td>
            <td>{d.name.clone()}</td>
            <td>
                <span class=status_class(&d.status)>{d.status.clone()}</span>
            </td>
            <td>{url_cell}</td>
            <td class="muted">{updated}</td>
            <td class="ta-right">
                <div class="row-actions">
                    {move || {
                        if not_deployed {
                            let sc = sc_provision.clone();
                            view! {
                                <button
                                    class="btn btn-xs btn-accent"
                                    on:click=move |_| {
                                        enqueue_command(state, "provision", Some(sc.clone()))
                                    }
                                >
                                    "Provision"
                                </button>
                            }
                                .into_any()
                        } else {
                            ().into_any()
                        }
                    }}
                    <button
                        class="btn btn-xs btn-secondary"
                        on:click=move |_| enqueue_command(state, "reset", Some(sc_reset.clone()))
                    >
                        "Reset"
                    </button>
                    <button
                        class="btn btn-xs btn-danger"
                        on:click=move |_| enqueue_command(state, "teardown", Some(sc_teardown.clone()))
                    >
                        "Teardown"
                    </button>
                </div>
            </td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::apply_command;
    use crate::models::{Command, ConsoleState, Deployment};
    use wasm_bindgen_test::*;

    fn dep(sector: &str, slug: &str, status: &str) -> Deployment {
        Deployment {
            sector: sector.into(),
            slug: slug.into(),
            name: format!("showcase_{}_{}", sector.replace('-', "_"), slug.replace('-', "_")),
            status: status.into(),
            public_url: None,
            updated_at: None,
        }
    }

    fn cmd(action: &str, scenario: Option<&str>) -> Command {
        Command {
            id: "id-1".into(),
            action: action.into(),
            scenario: scenario.map(str::to_string),
            requested_at: "2026-07-06T00:00:00Z".into(),
            state: "pending".into(),
        }
    }

    #[wasm_bindgen_test]
    fn reset_nudges_only_the_targeted_scenario_to_provisioning() {
        let mut cs = ConsoleState::initial("org");
        cs.deployments = vec![dep("pharma", "payload", "Online"), dep("pharma", "test-drive", "Online")];
        apply_command(&mut cs, &cmd("reset", Some("pharma/payload")));
        assert_eq!(cs.deployments[0].status, "Provisioning"); // targeted
        assert_eq!(cs.deployments[1].status, "Online"); // untouched
        assert_eq!(cs.commands.len(), 1);
        assert_eq!(cs.commands[0].action, "reset");
    }

    #[wasm_bindgen_test]
    fn teardown_marks_targeted_not_deployed() {
        let mut cs = ConsoleState::initial("org");
        cs.deployments = vec![dep("pharma", "payload", "Online")];
        apply_command(&mut cs, &cmd("teardown", Some("pharma/payload")));
        assert_eq!(cs.deployments[0].status, "Not deployed");
    }

    #[wasm_bindgen_test]
    fn a_null_scenario_applies_to_every_deployment() {
        let mut cs = ConsoleState::initial("org");
        cs.deployments = vec![dep("pharma", "payload", "Online"), dep("finserv", "test-drive", "Failed")];
        apply_command(&mut cs, &cmd("provision", None));
        assert!(cs.deployments.iter().all(|d| d.status == "Provisioning"));
    }

    #[wasm_bindgen_test]
    fn an_unknown_action_appends_the_command_but_touches_no_status() {
        let mut cs = ConsoleState::initial("org");
        cs.deployments = vec![dep("pharma", "payload", "Online")];
        apply_command(&mut cs, &cmd("frobnicate", Some("pharma/payload")));
        assert_eq!(cs.deployments[0].status, "Online"); // unchanged
        assert_eq!(cs.commands.len(), 1); // still queued
    }
}
