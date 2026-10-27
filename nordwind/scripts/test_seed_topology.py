#!/usr/bin/env python3
"""nordwind-seed-topology.sh against an in-memory mock of the control-plane /me/topologies API.
Proves: creates once, second run changes nothing (idempotent), shares by e-mail, links channels,
and refuses loudly when an agent already belongs to someone else's topology."""
import json, os, re, subprocess, tempfile, threading, unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

SCRIPT = os.path.join(os.path.dirname(__file__), "nordwind-seed-topology.sh")


class CP(BaseHTTPRequestHandler):
    topos: dict = {}
    taken: set = set()
    shares: list = []
    calls: list = []

    def log_message(self, *a): pass

    def _body(self):
        n = int(self.headers.get("content-length", 0))
        return json.loads(self.rfile.read(n) or b"{}") if n else {}

    def _out(self, code, obj=None):
        b = json.dumps(obj if obj is not None else {}).encode()
        self.send_response(code); self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)

    def _route(self, method):
        CP.calls.append((method, self.path))
        if self.headers.get("authorization") != "Bearer tok":
            return self._out(401)
        parts = self.path.strip("/").split("/")
        body = self._body() if method in ("POST", "PUT") else {}
        if parts == ["me", "topologies"]:
            if method == "GET":
                return self._out(200, [{"id": i, "net_uuid": "n"} for i in CP.topos])
            tid = "t%d" % (len(CP.topos) + 1)
            CP.topos[tid] = dict(id=tid, net_uuid="n", agents=[], edges=[], overlay_mode="baseline")
            return self._out(200, {"id": tid, "net_uuid": "n"})
        t = CP.topos.get(parts[2])
        if len(parts) == 3:
            return self._out(200, t)
        if parts[3] == "agents" and len(parts) == 4:
            if body["agent"] in CP.taken:
                return self._out(409, "already assigned")
            CP.taken.add(body["agent"]); t["agents"].append(dict(id=body["agent"], kind=body.get("kind", "peer"), label=body.get("label"), verified=True))
            return self._out(200, {"verified": True})
        if parts[3] == "agents":
            for a in t["agents"]:
                if a["id"] == parts[4]: a["label"] = body["label"]
            return self._out(200)
        if parts[3] == "edges" and len(parts) == 4:
            t["edges"].append(dict(a=body["a"], b=body["b"])); return self._out(200)
        if parts[3] == "edges":
            for e in t["edges"]:
                if {e["a"], e["b"]} == {body["a"], body["b"]}: e["channel"] = body["channel"]
            return self._out(200)
        if parts[3] == "share":
            CP.shares.append(body["email"]); return self._out(200)
        return self._out(404)

    do_GET = lambda s: s._route("GET")
    do_POST = lambda s: s._route("POST")
    do_PUT = lambda s: s._route("PUT")


def manifest():
    ids = ["cloud", "zentrale", "hafenbuero", "werkstatt", "aussendienst"]
    nodes = [dict(id=i, label_de=i.title(), label_en=i, holder_pubkey=(i[0] * 64), os_hint="Linux", role="hub" if i == "zentrale" else "site", planned=i == "aussendienst") for i in ids]
    chans = [dict(name=f"{a}-{b}", a=a, b=b, id=(a[0] + b[0]) * 32) for a, b in [("cloud", "zentrale"), ("zentrale", "werkstatt"), ("zentrale", "hafenbuero"), ("aussendienst", "zentrale")]]
    return dict(nodes=nodes, channels=chans)


class Seed(unittest.TestCase):
    def setUp(self):
        CP.topos, CP.taken, CP.shares, CP.calls = {}, set(), [], []
        self.srv = HTTPServer(("127.0.0.1", 0), CP)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.gen = tempfile.mkdtemp()
        json.dump(manifest(), open(os.path.join(self.gen, "nodes.json"), "w"))

    def tearDown(self):
        self.srv.shutdown()

    def run_seed(self, shares=""):
        env = dict(os.environ, CT_OIDC_TOKEN="tok", NW_CP_URL=f"http://127.0.0.1:{self.srv.server_port}", NW_GEN_DIR=self.gen, NW_SHARE_EMAILS=shares)
        return subprocess.run(["bash", SCRIPT], env=env, capture_output=True, text=True)

    def test_creates_then_is_idempotent(self):
        r = self.run_seed("a@example.org,b@example.org")
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
        t = CP.topos["t1"]
        self.assertEqual((len(t["agents"]), len(t["edges"])), (5, 4))
        self.assertTrue(all(e.get("channel") for e in t["edges"]))
        self.assertEqual(sorted(CP.shares), ["a@example.org", "b@example.org"])
        zen = next(a for a in t["agents"] if a["id"] == "z" * 64)
        self.assertEqual(zen["kind"], "super-peer")
        self.assertTrue(next(a for a in t["agents"] if a["id"] == "a" * 64)["label"].endswith("(geplant)"))
        CP.calls.clear()
        r2 = self.run_seed()
        self.assertEqual(r2.returncode, 0, r2.stderr)
        writes = [c for c in CP.calls if c[0] in ("POST", "PUT") and not c[1].endswith("/share")]
        self.assertEqual(writes, [], "second run must not change anything")
        self.assertEqual(len(CP.topos), 1)

    def test_agent_owned_elsewhere_fails_loudly(self):
        CP.taken.add("z" * 64)
        r = self.run_seed()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("zentrale", r.stderr + r.stdout)

    def test_rejects_bad_token(self):
        env = dict(os.environ, CT_OIDC_TOKEN="wrong", NW_CP_URL=f"http://127.0.0.1:{self.srv.server_port}", NW_GEN_DIR=self.gen)
        self.assertNotEqual(subprocess.run(["bash", SCRIPT], env=env, capture_output=True, text=True).returncode, 0)


if __name__ == "__main__":
    unittest.main()
