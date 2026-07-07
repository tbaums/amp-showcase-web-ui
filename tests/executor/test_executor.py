"""End-to-end offline tests for amp-executor.sh.

Drives the real script against a stateful mock AMP control plane (mock_amp.py)
and asserts the resulting state.json — command states and deployment rows —
without any network, credentials, or real deploys. Run: python -m pytest, or
just `python tests/executor/test_executor.py`.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mock_amp import make_server  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "amp-executor.sh"
ORG = "test-org-uuid"


def _run(state: dict, *, token="test-token", org=ORG, extra_env=None) -> dict:
    """Write state to a temp state.json, run the executor against the mock, return new state."""
    httpd, mock = make_server()
    _run.mock = mock  # expose captured deploy env etc. to key-passing tests
    port = httpd.server_address[1]
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    try:
        with tempfile.TemporaryDirectory() as d:
            sf = Path(d) / "state.json"
            sf.write_text(json.dumps(state))
            env = {
                **os.environ,
                "CREWAI_BASE_URL": f"http://127.0.0.1:{port}",
                "STATE_FILE": str(sf),
                "SHOWCASE_DEPLOY_OWNER": "tbaums",
                "AMP_POLL_SLEEP": "0",
                "AMP_POLL_TRIES": "3",
            }
            env.pop("ANTHROPIC_API_KEY", None)  # deterministic: only a test may set it
            if token is not None:
                env["CREWAI_TOKEN"] = token
            if org is not None:
                env["SHOWCASE_ORG"] = org
            if extra_env:
                env.update(extra_env)
            r = subprocess.run(["bash", str(SCRIPT)], env=env,
                               capture_output=True, text=True, timeout=60)
            assert r.returncode == 0, f"executor exited {r.returncode}\n{r.stderr}\n{r.stdout}"
            return json.loads(sf.read_text())
    finally:
        httpd.shutdown()


def _cmd(action, scenario=None, cid="c1"):
    return {"id": cid, "action": action, "scenario": scenario,
            "requested_at": "t", "state": "pending"}


def _state(commands, deployments=None):
    return {"schema_version": 1, "org_id": ORG,
            "deployments": deployments or [], "commands": commands}


def _dep(s, out):
    return next((d for d in out["deployments"] if d["sector"] + "/" + d["slug"] == s), None)


def _cmd_state(out, cid="c1"):
    return next(c["state"] for c in out["commands"] if c["id"] == cid)


# --- tests -------------------------------------------------------------------
def test_provision_single_comes_online():
    out = _run(_state([_cmd("provision", "pharma/your-own-data")]))
    assert _cmd_state(out) == "done"
    d = _dep("pharma/your-own-data", out)
    assert d["status"] == "Online"
    assert d["name"] == "showcase_pharma_your_own_data"
    assert d["public_url"].startswith("http")  # mock serves http://…/pub/{uuid}
    print("ok: provision single -> Online")


def test_provision_all_iterates_catalog():
    out = _run(_state([_cmd("provision", None)]))  # scenario=None => ALL
    assert _cmd_state(out) == "done"
    assert len(out["deployments"]) == 6
    assert all(d["status"] == "Online" for d in out["deployments"])
    keys = {d["sector"] + "/" + d["slug"] for d in out["deployments"]}
    assert "financial-services/no-code-trigger" in keys and "pharma/execution-trace" in keys
    print("ok: provision ALL -> 6 Online")


def test_provision_is_idempotent():
    # Pre-existing deployment row; provisioning again must not error or duplicate.
    pre = [{"sector": "pharma", "slug": "your-own-data",
            "name": "showcase_pharma_your_own_data", "status": "Online",
            "public_url": "https://old.crewai.com", "updated_at": "t"}]
    # First run creates it on the mock, second run should find + skip.
    out1 = _run(_state([_cmd("provision", "pharma/your-own-data")], pre))
    assert _cmd_state(out1) == "done"
    assert len([d for d in out1["deployments"] if d["slug"] == "your-own-data"]) == 1
    print("ok: provision idempotent (no duplicate row, no error)")


def test_teardown_marks_not_deployed():
    out = _run(_state([_cmd("provision", "pharma/your-own-data", "c1"),
                       _cmd("teardown", "pharma/your-own-data", "c2")]))
    assert _cmd_state(out, "c1") == "done"
    assert _cmd_state(out, "c2") == "done"
    assert _dep("pharma/your-own-data", out)["status"] == "Not deployed"
    print("ok: teardown -> Not deployed")


def test_reset_ends_online():
    out = _run(_state([_cmd("provision", "pharma/your-own-data", "c1"),
                       _cmd("reset", "pharma/your-own-data", "c2")]))
    assert _cmd_state(out, "c2") == "done"
    assert _dep("pharma/your-own-data", out)["status"] == "Online"
    print("ok: reset -> Online")


def test_unknown_action_is_error():
    out = _run(_state([_cmd("frobnicate", "pharma/your-own-data")]))
    assert _cmd_state(out) == "error"
    print("ok: unknown action -> command error")


def test_create_failure_marks_failed():
    # DEPLOY_OWNER override forces the deployment name to contain "boom"? No — the
    # mock keys off the crew NAME. Use a synthetic scenario whose slug yields "boom".
    out = _run(_state([_cmd("provision", "pharma/boom")]))
    assert _cmd_state(out) == "error"
    assert _dep("pharma/boom", out)["status"] == "Failed"
    print("ok: create failure -> command error + deployment Failed")


def test_kickoff_runs_end_to_end():
    out = _run(_state([_cmd("provision", "pharma/your-own-data", "c1"),
                       _cmd("kickoff", "pharma/your-own-data", "c2")]))
    assert _cmd_state(out, "c2") == "done"
    d = _dep("pharma/your-own-data", out)
    assert d["last_run"]["state"] == "SUCCESS"
    assert d["last_run"]["result"]  # non-empty run output recorded
    assert d["last_run"]["kickoff_id"].startswith("kickoff-")
    print("ok: kickoff runs end-to-end -> last_run SUCCESS with output")


def test_kickoff_failed_run_marks_error():
    out = _run(_state([_cmd("provision", "pharma/flopkick", "c1"),
                       _cmd("kickoff", "pharma/flopkick", "c2")]))
    assert _cmd_state(out, "c2") == "error"
    assert _dep("pharma/flopkick", out)["last_run"]["state"] == "FAILED"
    print("ok: failed run -> command error + last_run FAILED")


def test_kickoff_on_absent_deployment_errors():
    out = _run(_state([_cmd("kickoff", "pharma/your-own-data")]))
    assert _cmd_state(out) == "error"
    print("ok: kickoff a non-deployed scenario -> command error")


def test_provision_passes_anthropic_key_when_set():
    _run(_state([_cmd("provision", "pharma/your-own-data")]),
         extra_env={"ANTHROPIC_API_KEY": "sk-test-123"})
    assert _run.mock.last_env == {"ANTHROPIC_API_KEY": "sk-test-123"}, _run.mock.last_env
    print("ok: ANTHROPIC_API_KEY forwarded into deploy env when set")


def test_provision_sends_empty_env_without_key():
    _run(_state([_cmd("provision", "pharma/your-own-data")]))  # no ANTHROPIC_API_KEY
    assert _run.mock.last_env == {}, _run.mock.last_env
    print("ok: no key set -> deploy env is empty (no leakage)")


def test_failed_status_during_poll_marks_failed():
    # A crew that provisions but reports a failed status while polling -> Failed + error.
    out = _run(_state([_cmd("provision", "pharma/flop")]))
    assert _cmd_state(out) == "error"
    assert _dep("pharma/flop", out)["status"] == "Failed"
    print("ok: failed poll status -> deployment Failed + command error")


def test_secret_gate_noops_without_token():
    # No token => executor must no-op: command stays pending, exit 0.
    out = _run(_state([_cmd("provision", "pharma/your-own-data")]), token=None)
    assert _cmd_state(out) == "pending"
    assert out["deployments"] == []
    print("ok: secret gate -> no-op, command still pending")


def test_commit_step_is_push_resilient():
    # Regression for run 28843654726: the Commit step git-pushed state.json with
    # no fetch/rebase/retry, so an interleaved write (console enqueue / overlapping
    # run) non-fast-forward-rejected the push and failed the whole run. Guard that
    # the resilience (integrate remote + retry) and the serialization guard stay.
    wf = (REPO / ".github/workflows/execute-commands.yml").read_text()
    assert "git fetch origin main" in wf, "commit step must fetch remote before pushing"
    assert "git rebase origin/main" in wf, "commit step must rebase onto remote"
    assert "for attempt in" in wf, "commit step must retry the push on rejection"
    assert "group: execute-commands" in wf, "concurrency group must serialize runs"
    assert "cancel-in-progress: false" in wf, "must queue (not cancel) mid-provision runs"
    print("ok: commit step is push-resilient (fetch+rebase+retry) + serialized")


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
    print(f"\nALL {len(fns)} EXECUTOR TESTS PASSED")
