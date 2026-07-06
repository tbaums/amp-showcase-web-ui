# Release runbook — the amp-showcase web UI (Leptos/WASM console)

This is the runbook for cutting a release of the **console** — the browser
dashboard at **https://tbaums.github.io/amp-showcase-web-ui/**. It's a
front-end-only Leptos/WASM SPA (no backend), deployed to GitHub Pages by
`.github/workflows/deploy.yml` on every push to `main`.

The one thing that makes a console release *actually reach returning users* is
**cache-busting** — a returning visitor's browser must not keep serving the old
WASM. Read "How clients get the new build" below before cutting a release.

## What a release is

A tagged version of the console app. Semver on the app itself:

- **Patch** (`0.1.x`) — a fix or copy change, no new capability.
- **Minor** (`0.x.0`) — new console capability (e.g. the Run/kickoff action).
- **Major** (`x.0.0`) — a breaking change to the state.json contract it shares
  with the executor (`amp-executor.sh` / `models.rs`).

The version lives in **one place**: `Cargo.toml` `[package].version`. Everything
else (the visible version badge, the cache-bust) derives from it.

## How clients get the new build (the cache-bust mechanism — verified)

This is the important part, and it is a real mechanism, not an assumption:

1. **Content-hashed asset filenames.** `trunk build --release` emits the bundle
   with a content hash in the filename — e.g.
   `amp-showcase-web-ui-<hash>_bg.wasm` and `amp-showcase-web-ui-<hash>.js` — and
   rewrites `index.html` to point at those exact names. Different bytes → different
   hash → different filename → a URL the browser has never cached. (Verify on the
   live site: `curl -s https://tbaums.github.io/amp-showcase-web-ui/ | grep -oE
   '[^"]*\.(wasm|js)"'`.)

2. **The version is compiled into the WASM.** The UI shows `v<version>` in the
   header, read from `env!("CARGO_PKG_VERSION")` (`src/lib.rs::VERSION`). Because
   that literal is baked into the binary, **bumping the version changes the WASM
   bytes even with no other code change** — so a version-only release still
   changes the hash and still busts the cache. Without this, a pure version bump
   would emit byte-identical WASM (same filename) and returning clients would see
   nothing new. This is the gap this wiring closes.

3. **The HTML entry point is short-lived.** GitHub Pages serves `index.html` with
   `Cache-Control: max-age=600` (10 min) plus an ETag. So a returning visitor
   revalidates/re-fetches `index.html` within ~10 minutes, receives the rewritten
   references to the new hashed bundle, and the browser then fetches that new
   bundle (a never-seen URL, guaranteed fresh).

**Net:** a returning client is on the new build within ~10 minutes of a deploy,
with no manual hard-refresh. The visible `v<version>` badge is how anyone can
confirm which build they're actually running.

Deliberately **not** used: a service worker. GitHub Pages doesn't allow custom
`Cache-Control`, and a SW is a common source of *stickier* staleness bugs; the
hashed-asset + short-HTML-cache pattern is the standard, sufficient mechanism
here. The only cost is the ≤10-min revalidation window on the HTML, which is an
acceptable bound for an internal tool. If instant refresh is ever required,
that's the point to revisit (SW with skipWaiting, or a versioned query string).

## Cutting a release

```bash
# 1. Bump the version (single source of truth).
#    Edit Cargo.toml [package].version, then sync the lockfile:
cargo update -p amp-showcase-web-ui   # or `cargo build` once; commits the new Cargo.lock version

# 2. Commit to main. That's the release — deploy.yml builds + deploys.
git commit -am "release: v0.1.1"
git push origin main
```

`deploy.yml` runs `trunk build --release --public-url "/amp-showcase-web-ui/"`,
uploads `dist/`, and deploys to Pages. No manual build step.

## Verify the release reached clients (do this, don't assume)

```bash
# a) the deploy workflow is green
gh run list -R tbaums/amp-showcase-web-ui --workflow deploy.yml --limit 1

# b) the hashed asset filename CHANGED vs the previous release
curl -s https://tbaums.github.io/amp-showcase-web-ui/ | grep -oE '[^"/]*\.(wasm|js)"'
#    -> the <hash> in amp-showcase-web-ui-<hash>_bg.wasm must differ from last release

# c) the visible version bumped (what a returning client sees in the header)
curl -s https://tbaums.github.io/amp-showcase-web-ui/ | grep -oE 'wm-ver[^<]*<[^>]*>[^<]*'
#    -> or just open the site; the header reads "crewai / amp-showcase web UI v0.1.1"
```

If the hash didn't change, the cache-bust didn't happen — stop and find out why
(did the version actually change? did the build run?). RED if a release ships a
version bump but the asset hash is unchanged, because returning clients would
keep the stale WASM.

## Relationship to the executor

The console and the executor (`amp-executor.sh` + `execute-commands.yml`, which
FDEs install into their state repos — see the amp-showcase repo) share the
`state.json` contract (`src/models.rs`). A **major** console release means that
contract changed; coordinate an executor update at the same time so an installed
executor and a freshly-loaded console still agree on the schema.
