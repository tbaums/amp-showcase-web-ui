#!/usr/bin/env bash
# amp-executor.sh — the amp-showcase web UI executor.
#
# The browser console never calls AMP directly (CORS + secret safety). It appends
# Command objects to state.json (state="pending"). This script — run by the
# execute-commands.yml GitHub Action in the FDE's own state repo — drains that
# queue: for each pending command it runs the matching AMP control-plane action,
# then writes results (deployment status + public_url, command state) back into
# state.json. The workflow commits the file afterward.
#
# Self-contained by design: only curl + jq, no dependency on the (private)
# amp-showcase repo. It mirrors the exact wire contract the tested ops scripts use
# (scripts/lib/control_plane.py):
#
#   POST   {API}/crews                 {"deploy":{name,repo_clone_url,env}}  -> crew JSON
#   GET    {API}/crews                 -> [ {uuid,name,status,public_url}, ... ]
#   GET    {API}/crews/{uuid}/status   -> {status, public_url, ...}
#   DELETE {API}/crews/{uuid}          -> 200/202/204/404 (404 == already gone)
#   Headers: Authorization: Bearer $CREWAI_TOKEN + X-Crewai-Organization-Id: $SHOWCASE_ORG
#
# Auth: CREWAI_TOKEN is the AMP Settings -> Account bearer token (a DeploymentUserToken,
# ~60-day TTL). It is used directly as the bearer here — there is no CREWAI_PLATFORM_
# INTEGRATION_TOKEN indirection (that key is connections-only and 401s on this API).
set -euo pipefail

# --- config (all overridable via env / repo secrets+vars) --------------------
BASE_URL="${CREWAI_BASE_URL:-https://app.crewai.com}"
API="${BASE_URL%/}/crewai_plus/api/v1"
TOKEN="${CREWAI_TOKEN:-}"
ORG="${SHOWCASE_ORG:-}"
# Owner of the public per-scenario deploy repos (amp-showcase-<sector>-<slug>).
# Same central value for every FDE — override with the SHOWCASE_DEPLOY_OWNER repo variable.
DEPLOY_OWNER="${SHOWCASE_DEPLOY_OWNER:-tbaums}"
STATE="${STATE_FILE:-state.json}"
PREFIX="showcase_"
POLL_TRIES="${AMP_POLL_TRIES:-18}"     # provision poll: up to POLL_TRIES * POLL_SLEEP seconds
POLL_SLEEP="${AMP_POLL_SLEEP:-10}"

# Canonical scenario catalog (sector/slug), mirroring the six public deploy repos.
# "provision/reset/teardown ALL" (a command with scenario=null) iterates this list.
# Keep in sync when scenarios are added — see docs/walkthroughs/05-onboard-a-new-fde.md.
CATALOG=(
  "financial-services/no-code-trigger"
  "financial-services/your-own-data"
  "financial-services/execution-trace"
  "pharma/no-code-trigger"
  "pharma/your-own-data"
  "pharma/execution-trace"
)

now() { date -u +%Y-%m-%dT%H:%M:%S.000Z; }
log() { echo "[amp-executor] $*"; }

deployment_name() { # sector slug -> showcase_<sector>_<slug>
  printf '%s%s_%s' "$PREFIX" "$1" "$2" | tr '-' '_'
}
repo_url() { # sector slug -> public deploy repo clone url
  printf 'https://github.com/%s/amp-showcase-%s-%s.git' "$DEPLOY_OWNER" "$1" "$2"
}

# --- AMP control-plane calls (curl) ------------------------------------------
# amp_curl sets two globals: AMP_HTTP (status code) and the response body in the
# file $AMP_BODY. Callers MUST invoke it in their own shell (not inside $(...)),
# then read "$AMP_BODY" — a global set inside a command-substitution subshell
# would not survive back to the caller.
AMP_HTTP=""
AMP_BODY="$(mktemp)"
trap 'rm -f "$AMP_BODY" "$AMP_BODY".tmp 2>/dev/null || true' EXIT

amp_curl() { # method path [json-body] -> sets AMP_HTTP, writes body to $AMP_BODY
  local method="$1" path="$2" body="${3:-}"
  local -a args=(-sS -o "$AMP_BODY" -w '%{http_code}'
    -X "$method" "$API/$path"
    -H "Authorization: Bearer $TOKEN"
    -H "X-Crewai-Organization-Id: $ORG")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  AMP_HTTP="$(curl "${args[@]}")"
}

amp_destroy() {
  amp_curl DELETE "crews/$1"
  case "${AMP_HTTP:-}" in 200|202|204|404) return 0 ;; *) return 1 ;; esac
}
amp_create() { # name repo_url env-json  (body -> $AMP_BODY, status -> AMP_HTTP)
  amp_curl POST crews "$(jq -nc --arg n "$1" --arg r "$2" --argjson e "$3" \
    '{deploy:{name:$n, repo_clone_url:$r, env:$e}}')"
}

# env passed to the deployed crew. ANTHROPIC_API_KEY rides along only if provided
# (matches scripts/lib/config.deploy_env); provisioning itself needs no model key.
deploy_env_json() {
  if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    jq -nc --arg k "$ANTHROPIC_API_KEY" '{ANTHROPIC_API_KEY:$k}'
  else
    echo '{}'
  fi
}

uuid_by_name() { # name -> uuid of first matching crew, or "" (reads a fresh live list)
  local name="$1"
  amp_curl GET crews
  [ "${AMP_HTTP:-}" = 200 ] || { log "list failed (HTTP ${AMP_HTTP:-none})"; return 1; }
  jq -r --arg n "$name" 'map(select(.name == $n)) | (.[0].uuid // "")' "$AMP_BODY"
}

# --- state.json patching -----------------------------------------------------
upsert_deployment() { # sector slug name status public_url
  local n; n="$(now)"
  jq --arg sector "$1" --arg slug "$2" --arg name "$3" --arg status "$4" \
     --arg url "$5" --arg now "$n" '
    (.deployments // []) as $ds
    | .deployments = (
        if any($ds[]?; .sector == $sector and .slug == $slug)
        then ($ds | map(if .sector == $sector and .slug == $slug
              then .name = $name | .status = $status
                   | .public_url = (if $url == "" then null else $url end)
                   | .updated_at = $now
              else . end))
        else $ds + [{sector:$sector, slug:$slug, name:$name, status:$status,
                     public_url:(if $url == "" then null else $url end), updated_at:$now}]
        end)
    | .updated_at = $now
  ' "$STATE" > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

set_command_state() { # id state
  local n; n="$(now)"
  jq --arg id "$1" --arg st "$2" --arg now "$n" \
    '.commands = (.commands | map(if .id == $id then .state = $st else . end)) | .updated_at = $now' \
    "$STATE" > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

# --- actions (each returns 0 on success, sets DEPLOY_STATUS + PUBLIC_URL) -----
do_teardown() { # sector slug  -> removes the scenario's deployment, verifies gone
  local name uuid; name="$(deployment_name "$1" "$2")"
  DEPLOY_STATUS="Not deployed"; PUBLIC_URL=""
  while true; do
    uuid="$(uuid_by_name "$name")" || return 1
    [ -n "$uuid" ] || break
    log "teardown: destroying $name ($uuid)"
    amp_destroy "$uuid" || { log "destroy failed for $uuid"; return 1; }
  done
  # verify zero remain
  uuid="$(uuid_by_name "$name")" || return 1
  [ -z "$uuid" ] || { log "teardown incomplete: $name still present ($uuid)"; return 1; }
  return 0
}

do_provision() { # sector slug  -> creates if absent, polls to Online
  local name uuid; name="$(deployment_name "$1" "$2")"
  uuid="$(uuid_by_name "$name")" || return 1
  if [ -n "$uuid" ]; then
    log "provision: $name already exists ($uuid) — skipping (idempotent)"
  else
    amp_create "$name" "$(repo_url "$1" "$2")" "$(deploy_env_json)"
    case "${AMP_HTTP:-}" in
      200|201|202) uuid="$(jq -r '.uuid // ""' "$AMP_BODY")" ;;
      *) log "create failed for $name (HTTP ${AMP_HTTP:-none}): $(cat "$AMP_BODY")"; return 1 ;;
    esac
    log "provision: created $name ($uuid)"
  fi
  # Poll status until Online (or leave Provisioning for a later tick to finalize).
  DEPLOY_STATUS="Provisioning"; PUBLIC_URL=""
  local i st
  for ((i = 0; i < POLL_TRIES; i++)); do
    amp_curl GET "crews/$uuid/status"
    [ "${AMP_HTTP:-}" = 200 ] || { sleep "$POLL_SLEEP"; continue; }
    st="$(jq -r '.status // ""' "$AMP_BODY")"
    PUBLIC_URL="$(jq -r '.public_url // ""' "$AMP_BODY")"
    case "$st" in
      completed|Completed|online|Online|running|Running|ready|Ready)
        DEPLOY_STATUS="Online"; return 0 ;;
      failed|Failed|error|Error|*_error|provisioning_failed)
        DEPLOY_STATUS="Failed"; log "provision: $name reported status=$st"; return 1 ;;
    esac
    sleep "$POLL_SLEEP"
  done
  log "provision: $name still provisioning after poll window — leaving Provisioning"
  return 0
}

do_reset() { # sector slug -> teardown then provision
  do_teardown "$1" "$2" || return 1
  do_provision "$1" "$2"
}

# run one action against one scenario; upsert its resulting deployment row.
run_one() { # action sector slug -> 0/1 (records status regardless)
  local action="$1" sector="$2" slug="$3" rc=0
  DEPLOY_STATUS=""; PUBLIC_URL=""
  case "$action" in
    provision) do_provision "$sector" "$slug" || rc=1 ;;
    reset)     do_reset     "$sector" "$slug" || rc=1 ;;
    teardown)  do_teardown  "$sector" "$slug" || rc=1 ;;
    *) log "unknown action '$action'"; return 2 ;;
  esac
  [ "$rc" -eq 0 ] || { [ -n "$DEPLOY_STATUS" ] || DEPLOY_STATUS="Failed"; }
  upsert_deployment "$sector" "$slug" "$(deployment_name "$sector" "$slug")" \
    "$DEPLOY_STATUS" "$PUBLIC_URL"
  return "$rc"
}

# --- main --------------------------------------------------------------------
main() {
  [ -f "$STATE" ] || { log "no $STATE — nothing to do."; exit 0; }
  # Secret gate: never fire metered AMP calls without credentials. No-op cleanly
  # so the console still works as a read-only status view until secrets are set.
  if [ -z "$TOKEN" ] || [ -z "$ORG" ]; then
    log "CREWAI_TOKEN / SHOWCASE_ORG not set — skipping execution (no-op)."
    exit 0
  fi

  local pending; pending="$(jq -r '.commands[]? | select(.state == "pending") | .id' "$STATE")"
  [ -n "$pending" ] || { log "no pending commands."; exit 0; }

  local id action scenario rc targets sector slug
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    action="$(jq -r --arg id "$id" '.commands[] | select(.id==$id) | .action' "$STATE")"
    scenario="$(jq -r --arg id "$id" '.commands[] | select(.id==$id) | (.scenario // "")' "$STATE")"
    log "== command $id: action=$action scenario=${scenario:-ALL} =="

    if [ "$action" != "provision" ] && [ "$action" != "reset" ] && [ "$action" != "teardown" ]; then
      log "unknown action '$action' — marking command error."
      set_command_state "$id" "error"; continue
    fi

    if [ -n "$scenario" ]; then targets=("$scenario"); else targets=("${CATALOG[@]}"); fi
    rc=0
    for t in "${targets[@]}"; do
      sector="${t%%/*}"; slug="${t#*/}"
      run_one "$action" "$sector" "$slug" || rc=1
    done
    if [ "$rc" -eq 0 ]; then set_command_state "$id" "done"; else set_command_state "$id" "error"; fi
  done <<<"$pending"

  log "done."
}

main "$@"
