"""A tiny stateful in-memory mock of the AMP control plane, for testing
amp-executor.sh offline (no network, no creds, no real deploys).

Mirrors the four endpoints the executor uses:
  POST   /crewai_plus/api/v1/crews                {"deploy":{name,repo_clone_url,env}}
  GET    /crewai_plus/api/v1/crews                 -> [ {uuid,name,status,public_url} ]
  GET    /crewai_plus/api/v1/crews/{uuid}/status   -> {status, public_url}
  DELETE /crewai_plus/api/v1/crews/{uuid}          -> 204

Test hooks:
  * a create whose name contains "boom" returns HTTP 500 (create-failure path).
  * a crew reports status "completed" on its first status poll (so provision
    reaches Online fast; run the executor with AMP_POLL_SLEEP=0).
  * every request must carry Authorization: Bearer <token> and
    X-Crewai-Organization-Id: <org>, else 401 — this asserts the token wiring.
"""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer


class _State:
    def __init__(self):
        self.crews: dict[str, dict] = {}
        self.n = 0
        self.lock = threading.Lock()


def make_server(host="127.0.0.1", port=0):
    state = _State()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):  # keep test output quiet
            pass

        def _auth_ok(self) -> bool:
            return (self.headers.get("Authorization", "").startswith("Bearer ")
                    and bool(self.headers.get("X-Crewai-Organization-Id")))

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

        # crews/<uuid>[/status]
        def _crew_path(self):
            p = self.path.split("?", 1)[0].rstrip("/")
            prefix = "/crewai_plus/api/v1/crews"
            if not p.startswith(prefix):
                return None
            return p[len(prefix):].strip("/")  # "" | "<uuid>" | "<uuid>/status"

        def do_GET(self):
            if not self._auth_ok():
                return self._send(401, {"error": "unauthorized"})
            rest = self._crew_path()
            if rest is None:
                return self._send(404)
            with state.lock:
                if rest == "":  # list
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
                    # first poll flips provisioning -> completed
                    if c["status"] == "provisioning":
                        c["status"] = "completed"
                        c["public_url"] = f"https://{uuid}.crewai.com"
                    return self._send(200, {"status": c["status"], "public_url": c["public_url"]})
            return self._send(404)

        def do_POST(self):
            if not self._auth_ok():
                return self._send(401, {"error": "unauthorized"})
            if self._crew_path() != "":
                return self._send(404)
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length) or "{}")
            name = body.get("deploy", {}).get("name", "")
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
            rest = self._crew_path()
            if not rest or "/" in rest:
                return self._send(404)
            with state.lock:
                state.crews.pop(rest, None)  # idempotent: gone is fine
            return self._send(204)

    httpd = HTTPServer((host, port), Handler)
    return httpd, state
