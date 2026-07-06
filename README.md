# amp-showcase console

A dark-mode, monospace, single-page dashboard for CrewAI field engineers (FDEs)
to see and control their **AMP** demo/workshop deployments.

Built with **Leptos 0.7 (Rust → WASM, CSR)**. Front-end only: there is no
backend server. A private GitHub repo is both the state store and the command
channel.

---

## Architecture: FE → repo → Action

The browser can't call the AMP API directly (CORS, and you don't want a
long-lived AMP token in a browser). So the console treats a single
`state.json` file in a private repo as the source of truth, and a GitHub Action
in that repo is the privileged executor.

```
  ┌─────────────────────────┐        GitHub Contents API        ┌──────────────────────┐
  │  amp-showcase console    │  ── fetch state.json (GET) ──▶    │  FDE private repo     │
  │  (Leptos/WASM in browser)│  ◀─ push state.json (PUT, sha) ─  │  └ state.json         │
  │                          │                                   │  └ .github/workflows/ │
  │  • render deployments    │                                   │      execute-commands │
  │  • enqueue Commands       │                                  └───────────┬──────────┘
  │    (provision/reset/       │                                             │
  │     teardown)              │                                  schedule (~5m) / dispatch
  └─────────────────────────┘                                                │
              ▲                                                               ▼
              │                                             ┌──────────────────────────────┐
              │  poll / Refresh (re-fetch state.json)       │  execute-commands.yml          │
              └─────────────────────────────────────────────│  • read pending Commands       │
                 deployment status + command.state="done"   │  • call AMP API (CREWAI_TOKEN) │
                 written back by the Action                  │  • teardown-by-prefix+redeploy │
                                                             │  • write results → state.json  │
                                                             │  • commit                      │
                                                             └──────────────────────────────┘
```

1. The console renders `deployments[]` from `state.json`.
2. Action buttons (Provision / Reset / Teardown, per-row or "all") **enqueue** a
   `Command { action, scenario, state:"pending" }` into `state.json` and push it
   (base64 Contents API PUT with the previous blob sha for optimistic concurrency).
3. The **`execute-commands` GitHub Action** in the FDE's repo runs on a ~5-minute
   schedule (and on manual dispatch), drains pending commands by calling the AMP
   API with repo secrets, writes results back into `state.json`
   (`deployments[].status`, `command.state="done"`), and commits.
4. The console polls by re-fetching (Refresh button) and re-renders.

### `state.json` schema

```jsonc
{
  "schema_version": 1,
  "updated_at": "2026-07-05T12:00:00.000Z",
  "org_id": "org_...",
  "deployments": [
    {
      "sector": "retail",
      "slug": "checkout-copilot",
      "name": "showcase_retail_checkout-copilot",
      "status": "Online",              // Online | Provisioning | Failed | Not deployed
      "public_url": "https://...",     // or null
      "updated_at": "2026-07-05T11:59:00.000Z"
    }
  ],
  "commands": [
    {
      "id": "uuid-v4",
      "action": "reset",               // provision | reset | teardown
      "scenario": "retail/checkout-copilot",  // "<sector>/<slug>" or null for all
      "requested_at": "2026-07-05T12:00:00.000Z",
      "state": "pending"               // pending | done | error
    }
  ]
}
```

---

## Setup for an FDE

### 1. Create a private state repo
e.g. `you/amp-showcase-state`. Copy `.github/workflows/execute-commands.yml`
from this project into that repo.

### 2. Add repo secrets
In the state repo → **Settings → Secrets and variables → Actions**:

| Secret          | Purpose                                        |
| --------------- | ---------------------------------------------- |
| `CREWAI_TOKEN`  | AMP/CrewAI API token used by the executor      |
| `SHOWCASE_ORG`  | Your AMP org id                                |

The workflow already has `permissions: contents: write` so the default
`GITHUB_TOKEN` can commit `state.json` back.

### 3. Create a scoped GitHub token for the browser
Create a **fine-grained personal access token** scoped to **only** the single
private state repo, with **Repository permissions → Contents: Read and write**.
Nothing else. This is the token you paste into the console's Setup screen. It
can read/write `state.json` and nothing more.

### 4. Configure the console
Open the console and fill the **Setup** screen:
- **GitHub token** — the fine-grained PAT from step 3
- **State repo** — `owner/repo`
- **Branch** — usually `main`
- **AMP org id**

Click **Connect**. If the repo has no `state.json` yet, use **Create initial
state.json**. Config is stored in `localStorage` (per browser).

---

## Build & serve

Requires the `wasm32-unknown-unknown` target and [Trunk](https://trunkrs.dev/).

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk

# type-check (no trunk needed)
cargo check --target wasm32-unknown-unknown

# dev server with hot reload  → http://127.0.0.1:8082
trunk serve

# production build → ./dist (static; host anywhere / GitHub Pages)
trunk build --release
```

All CSS is inlined in `index.html` (dark + monospace, CrewAI coral accent), and
the app makes no external font/CDN requests (CSP-safe).

## Tests

`.github/workflows/ci.yml` runs on every push/PR: clippy against the wasm
target, then the unit suite via the Node wasm runner (these tests are
DOM-free). Locally:

```bash
cargo install wasm-pack
wasm-pack test --node --lib
```

Coverage is the load-bearing, backend-touching logic: the `state.json` schema
(serde round-trips + forward-compatible defaults + an executor-written result
document), `sync`'s GitHub Contents-API contract (config defaults, URL/auth
shape, the base64 wire round-trip), and `apply_command`'s optimistic
state-transitions (targeted vs all-scenario, each action's status nudge,
unknown-action safety). A browser-driven Playwright tier (drive the setup
screen → dashboard → queue a command) is the next step — see the TODO in
`ci.yml`.

---

## Project layout

```
amp-console/
├─ Cargo.toml                         # leptos 0.7 csr, gloo-net, base64, uuid, web-sys
├─ Trunk.toml
├─ index.html                         # inline dark/mono theme + trunk data-links
├─ src/
│  ├─ main.rs                         # mount_to_body(App)
│  ├─ lib.rs                          # module exports (lib target)
│  ├─ app.rs                          # AppState, View enum, Setup + Dashboard views
│  ├─ models.rs                       # ConsoleState / Deployment / Command
│  ├─ sync.rs                         # GitHub Contents API fetch/push (optimistic sha)
│  └─ storage.rs                      # localStorage config + last-synced
└─ .github/workflows/
   └─ execute-commands.yml            # the executor (schedule + dispatch)
```

> The AMP API calls in `execute-commands.yml` are intentionally **stubbed**
> (echo "WOULD …") with `TODO(AMP)` comments. The FE → repo → Action pipeline
> shape is real; wire the real `curl`/CLI calls into those steps.
