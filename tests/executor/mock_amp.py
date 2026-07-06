"""A tiny stateful in-memory mock of the AMP control plane + deployment
runtime, for testing amp-executor.sh offline (no network, no creds, no real
deploys or model calls).

Control-plane endpoints (org-bearer authed):
  POST   /crewai_plus/api/v1/crews                {"deploy":{name,repo_clone_url,env}}
  GET    /crewai_plus/api/v1/crews                 -> [ {uuid,name,status,public_url} ]
  GET    /crewai_plus/api/v1/crews/{uuid}/status   -> {status, public_url, token}
  DELETE /crewai_plus/api/v1/crews/{uuid}          -> 204

Deployment-runtime endpoints, served under the crew's own public_url (which
points back at this mock so the executor's kickoff calls stay offline):
  POST   /pub/{uuid}/kickoff                        -> {"kickoff_id": ...}
  GET    /pub/{uuid}/status/{kickoff_id}            -> {"state":"SUCCESS","result":{...}}

Test hooks (keyed off the crew NAME):
  * "boom"     in name -> POST /crews returns 500 (create-failure path).
  * "flop"     in name -> the crew reports a failed *provision* status on poll.
  * "flopkick" in name -> the crew comes Online but its *run* returns FAILED.
  * everything else       -> Online, and a kickoff returns SUCCESS with output.
Every control-plane request must carry Authorization: Bearer <token> and
X-Crewai-Organization-Id: <org>, else 401 — this asserts the token wiring.
The env sent on the last create is captured on state.last_env for key-passing
assertions.
"""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer


class _State:
    def __init__(self):
        self.crews: dict[str, dict] = {}
        self.runs: dict[str, dict] = {}
        self.last_env: dict | None = None
        self.base = ""
        self.n = 0
        self.lock = threading.Lock()


def make_server(host="127.0.0.1", port=0):
    state = _State()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _auth_ok(self) -> bool:
            return (self.headers.get("Authorization", "").startswith("Bearer ")
                    and bool(self.headers.get("X-Crewai-Organization-Id")))

        def _bearer_only(self) -> bool:  # deployment endpoints: crew token, no org header
            return self.headers.get("Authorization", "").startswith("Bearer ")

        def _send(self, code, obj=None):
            self.send_response(code)
            if obj is not None:
                body = json.dumps(obj).encode()
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            else:
                self.send_header("Content-Length", "0")
                self.end_headers()

        def _body(self):
            length = int(self.headers.get("Content-Length", "0"))
            return json.loads(self.rfile.read(length) or "{}")

        def _cp_path(self):  # control-plane crews path -> "" | uuid | uuid/status
            p = self.path.split("?", 1)[0].rstrip("/")
            prefix = "/crewai_plus/api/v1/crews"
            return p[len(prefix):].strip("/") if p.startswith(prefix) else None

        def _pub_parts(self):  # /pub/{uuid}/... -> [uuid, ...] or None
            p = self.path.split("?", 1)[0].strip("/").split("/")
            return p[1:] if len(p) >= 2 and p[0] == "pub" else None

        def do_GET(self):
            pub = self._pub_parts()
            if pub is not None:  # deployment runtime: GET /pub/{uuid}/status/{kid}
                if not self._bearer_only():
                    return self._send(401, {"error": "unauthorized"})
                if len(pub) == 3 and pub[1] == "status":
                    run = state.runs.get(pub[2])
                    if not run:
                        return self._send(404)
                    return self._send(200, run)
                return self._send(404)

            if not self._auth_ok():
                return self._send(401, {"error": "unauthorized"})
            rest = self._cp_path()
            if rest is None:
                return self._send(404)
            with state.lock:
                if rest == "":
                    return self._send(200, [
                        {"uuid": u, "name": c["name"], "status": c["status"],
                         "public_url": c["public_url"]}
                        for u, c in state.crews.items()
                    ])
                if rest.endswith("/status"):
                    uuid = rest[: -len("/status")]
                    c = state.crews.get(uuid)
                    if not c:
                        return self._send(404)
                    if c["status"] == "provisioning":  # first poll flips to terminal
                        if "flop" in c["name"] and "flopkick" not in c["name"]:
                            c["status"] = "Provisioning Failed, try again."
                        else:
                            c["status"] = "Crew is Online"
                            c["public_url"] = f"{state.base}/pub/{uuid}"
                    return self._send(200, {"status": c["status"],
                                            "public_url": c["public_url"], "token": f"tok-{uuid}"})
            return self._send(404)

        def do_POST(self):
            pub = self._pub_parts()
            if pub is not None:  # deployment runtime: POST /pub/{uuid}/kickoff
                if not self._bearer_only():
                    return self._send(401, {"error": "unauthorized"})
                if len(pub) == 2 and pub[1] == "kickoff":
                    uuid = pub[0]
                    c = state.crews.get(uuid)
                    if not c:
                        return self._send(404)
                    with state.lock:
                        state.n += 1
                        kid = f"kickoff-{state.n}"
                        if "flopkick" in c["name"]:
                            state.runs[kid] = {"state": "FAILED", "status": None,
                                               "result": "synthetic run failure"}
                        else:
                            state.runs[kid] = {"state": "SUCCESS", "status": None,
                                               "result": {"ok": True, "counterparty": "Acme",
                                                          "result": "synthetic KYC summary"}}
                    return self._send(200, {"kickoff_id": kid})
                return self._send(404)

            if not self._auth_ok():
                return self._send(401, {"error": "unauthorized"})
            if self._cp_path() != "":
                return self._send(404)
            body = self._body()
            deploy = body.get("deploy", {})
            name = deploy.get("name", "")
            state.last_env = deploy.get("env", {})
            if "boom" in name:
                return self._send(500, {"error": "synthetic create failure"})
            with state.lock:
                state.n += 1
                uuid = f"uuid-{state.n}"
                state.crews[uuid] = {"name": name, "status": "provisioning", "public_url": ""}
            return self._send(201, {"uuid": uuid, "name": name,
                                    "status": "provisioning", "public_url": ""})

        def do_DELETE(self):
            if not self._auth_ok():
                return self._send(401, {"error": "unauthorized"})
            rest = self._cp_path()
            if not rest or "/" in rest:
                return self._send(404)
            with state.lock:
                state.crews.pop(rest, None)
            return self._send(204)

    httpd = HTTPServer((host, port), Handler)
    state.base = f"http://{host}:{httpd.server_address[1]}"
    return httpd, state
