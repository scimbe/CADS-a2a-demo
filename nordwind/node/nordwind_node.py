#!/usr/bin/env python3
"""Nordwind node: the real service handler behind `CT_AGENT_SERVICE_HANDLER_CMD`.

One file, Python 3 stdlib only (runs on macOS, Linux containers, anything with python3).
`ct-agent channel` (serve mode) starts this script once per incoming service call: the
request is on stdin, `CT_SERVICE_TYPE` names the service, the single JSON line on stdout is
the reply that travels back over the end-to-end encrypted (Noise_IK) channel.

Services (which node offers what is decided by `NORDWIND_NODE`):

  ping          every node   {node, os, arch, agent, ts, ...}  -- who am I, really
  draft_text    werkstatt    local LLM (Ollama) drafts the briefing prose
  route_wetter  hafenbuero   route master data + live weather (Open-Meteo)
  mesh_status   zentrale     real `ping` calls to the neighbours, with RTT + wire bytes
  toern_briefing zentrale    the orchestration: werkstatt -> hafenbuero -> aussendienst -> result
  feld_abruf     zentrale    (called BY aussendienst) long-poll: hands out the pending field job
  feld_quittung  zentrale    (called BY aussendienst) returns the field result for that job

The Außendienst node is outbound-only like a phone: its channel grant is `initiate`, so it never
accepts calls. It keeps a long-poll `feld_abruf` open at Zentrale, derives its field sheet from the
job (rule based, no model) and hands it back with `feld_quittung`.

Everything the orchestrator reports about a hop (latency, ciphertext bytes, transport) is
measured from a REAL `ct-agent channel` initiator process it spawned for exactly that hop.
There is no fallback text: if the LLM or the weather source is down, the hop fails and
says so.
"""
from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

DEFAULT_TOPIC = "Törn-Briefing Samstag Bremen"
MAX_TOPIC_LEN = 120
HERE = os.path.dirname(os.path.abspath(__file__))

# Which services each node role serves. `ping` everywhere.
NODE_SERVICES = {
    "zentrale": ["toern_briefing", "mesh_status", "feld_abruf", "feld_quittung", "ping"],
    "hafenbuero": ["route_wetter", "ping"],
    "werkstatt": ["draft_text", "ping"],
    "aussendienst": ["ping"],
}

STATUS_RE = re.compile(r"status uptime=(\d+)s sent=(\d+)B recv=(\d+)B")


# --------------------------------------------------------------------------- helpers
def now_ms() -> int:
    return int(time.time() * 1000)


def clean_topic(raw) -> str:
    """The topic is the only caller-controlled string that reaches the LLM prompt: one
    line, bounded, no control characters."""
    if not isinstance(raw, str):
        return DEFAULT_TOPIC
    t = re.sub(r"[\x00-\x1f\x7f]+", " ", raw).strip()
    return t[:MAX_TOPIC_LEN] if t else DEFAULT_TOPIC


def parse_status_line(stderr_text: str):
    """Last `ct-agent channel: status uptime=..s sent=..B recv=..B` line -> (sent, recv).

    These counters are ciphertext bytes pumped over the network transport, counted below
    the Noise layer (ct-agent `CountingWriter`/`CountingReader`). A fresh initiator process
    per hop makes them per-hop numbers."""
    sent = recv = None
    for m in STATUS_RE.finditer(stderr_text or ""):
        sent, recv = int(m.group(2)), int(m.group(3))
    return sent, recv


def transport_from_stderr(stderr_text: str):
    t = stderr_text or ""
    if "relay leg via" in t:
        if "front door" in t:
            return "tcp_443"
        if "QUIC" in t:
            return "quic"
    if "direct" in t.lower() and "listening" not in t.lower():
        return "direct"
    return None


_agent_version_cache: dict = {}


def agent_version() -> str:
    if "v" not in _agent_version_cache:
        try:
            out = subprocess.run(
                [os.environ.get("CT_AGENT_BIN", "ct-agent"), "--version"],
                capture_output=True, text=True, timeout=3,
            )
            _agent_version_cache["v"] = (out.stdout or out.stderr).strip()[:60] or "unknown"
        except Exception:
            _agent_version_cache["v"] = "unknown"
    return _agent_version_cache["v"]


def node_name() -> str:
    return os.environ.get("NORDWIND_NODE", "unknown")


# ---------------------------------------------------------------- calling a neighbour
def channel_prefix(channel: str) -> str:
    return "NW_CH_" + channel.upper().replace("-", "_") + "_"


def read_cert_file(path, wait_s: float = 15.0) -> str:
    """Lab mode only: the cert a restarting direct-address acceptor published (lab_acceptor.py)."""
    if not path:
        return ""
    end = time.monotonic() + wait_s
    while time.monotonic() < end:
        try:
            v = open(path).read().strip()
            if v:
                return v
        except OSError:
            pass
        time.sleep(0.1)
    raise RuntimeError(f"no peer certificate published at {path}")


def initiator_env(channel: str, service: str, base_env=None) -> dict:
    """Environment for one real `ct-agent channel` initiator process on `channel`.

    Plane mode (production): broker/relay/front door come from the process environment,
    the per-channel operator-signed grant from `NW_CH_<CHANNEL>_GRANT`.
    Lab mode (`NW_CH_<CHANNEL>_DIRECT_ADDR` set): direct address, no broker (used by the
    local integration test with real ct-agent processes)."""
    env = dict(os.environ if base_env is None else base_env)
    p = channel_prefix(channel)
    holder = env.get(p + "HOLDER_KEY") or env.get("NW_HOLDER_KEY")
    noise = env.get(p + "NOISE_KEY") or env.get("NW_NOISE_KEY")
    if not noise:
        raise RuntimeError(f"no Noise key for channel {channel} (NW_NOISE_KEY / {p}NOISE_KEY)")
    env["CT_CHANNEL_ROLE"] = "initiate"
    env["CT_CHANNEL_CALL_SERVICE"] = service
    env["CT_CHANNEL_NOISE_KEY"] = noise
    env.pop("CT_CHANNEL_SERVE", None)
    direct = env.get(p + "DIRECT_ADDR")
    if direct:
        env["CT_CHANNEL_ADDR"] = direct
        env["CT_CHANNEL_PEER_NOISE_KEY"] = env.get(p + "PEER_NOISE_KEY", "")
        env["CT_CHANNEL_PEER_CERT"] = env.get(p + "PEER_CERT", "") or read_cert_file(env.get(p + "PEER_CERT_FILE"))
    else:
        grant = env.get(p + "GRANT")
        if not (holder and grant):
            raise RuntimeError(f"channel {channel} not provisioned ({p}GRANT / holder key missing)")
        env["CT_CHANNEL_HOLDER_KEY"] = holder
        env["CT_CHANNEL_GRANT"] = grant
        env["CT_CHANNEL_RELAY_ONLY"] = "1"
        env.setdefault("CT_CHANNEL_DIRECT_UPGRADE", "1")
    return env


def call_peer(channel: str, service: str, payload: dict, timeout: float = 90.0, runner=None) -> dict:
    """One real channel call = one fresh `ct-agent channel` initiator process.

    Returns {ok, reply, dur_ms, sent_bytes, recv_bytes, transport, error, plain_out, plain_in}.
    `runner` is injectable for unit tests; production uses subprocess."""
    line = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
    t0 = time.monotonic()
    res = {"ok": False, "reply": None, "dur_ms": 0, "sent_bytes": None, "recv_bytes": None,
           "transport": None, "error": None, "plain_out": len(line.encode()), "plain_in": 0}
    try:
        env = initiator_env(channel, service)
        run = runner or _run_agent
        rc, out, err = run(env, line + "\n", timeout)
    except subprocess.TimeoutExpired:
        res["error"] = f"timeout after {int(timeout)}s"
        res["dur_ms"] = int((time.monotonic() - t0) * 1000)
        return res
    except Exception as e:  # provisioning errors, missing binary, ...
        res["error"] = str(e)[:300]
        res["dur_ms"] = int((time.monotonic() - t0) * 1000)
        return res
    res["dur_ms"] = int((time.monotonic() - t0) * 1000)
    res["sent_bytes"], res["recv_bytes"] = parse_status_line(err)
    res["transport"] = transport_from_stderr(err)
    reply = ""
    for ln in (out or "").splitlines():
        if ln.strip():
            reply = ln.strip()
    inner, env_err = unwrap_envelope(reply)
    if rc == 0 and inner is not None:
        res["ok"] = True
        res["reply"] = inner
        res["plain_in"] = len(inner.encode())
    elif rc == 0 and reply:
        res["error"] = env_err or "service call failed"
    else:
        tail = [l for l in (err or "").splitlines() if l.strip()][-2:]
        res["error"] = f"exit {rc}: " + " | ".join(tail)[:300]
    return res


def unwrap_envelope(line: str):
    """Persistent call mode prints one NDJSON envelope per call:
    `{"ok":true,"output":"<handler stdout>"}` or `{"ok":false,"error":...}`.
    Returns (output, None) on success, (None, error_text) otherwise."""
    if not line:
        return None, None
    try:
        env = json.loads(line)
    except ValueError:
        return None, "unparseable reply envelope"
    if isinstance(env, dict) and env.get("ok") is True and isinstance(env.get("output"), str):
        return env["output"], None
    err = env.get("error") if isinstance(env, dict) else None
    return None, ("service error: " + (json.dumps(err) if not isinstance(err, str) else err))[:300]


def _run_agent(env: dict, stdin_text: str, timeout: float):
    p = subprocess.run(
        [env.get("CT_AGENT_BIN", "ct-agent"), "channel"],
        input=stdin_text, capture_output=True, text=True, timeout=timeout, env=env,
    )
    return p.returncode, p.stdout, p.stderr


# ----------------------------------------------------------------------- telemetry
class Reporter:
    """Optional live hop reports to the hub (capability URL minted per run by the hub).

    Pure side channel for *liveness of the display*: the authoritative record is the
    trace returned in the (encrypted) reply. Failures never affect the run."""

    def __init__(self, url, token, poster=None):
        self.url, self.token = url, token
        self.poster = poster or self._post
        self.sent = 0
        self.failed = 0
        self._lock = threading.Lock()

    def _post(self, body: bytes):
        req = urllib.request.Request(
            self.url, data=body, method="POST",
            headers={"content-type": "application/json", "x-run-token": self.token},
        )
        with urllib.request.urlopen(req, timeout=3) as r:  # noqa: S310 (https URL from hub)
            r.read(64)

    def hop(self, hop: dict):
        if not self.url or not self.token:
            return
        body = json.dumps(hop, ensure_ascii=False).encode()
        try:
            self.poster(body)
            with self._lock:
                self.sent += 1
        except Exception:
            with self._lock:
                self.failed += 1


# --------------------------------------------------------------------- the services
def svc_ping(_req: dict) -> dict:
    info = {
        "node": node_name(),
        "os": platform.system(),
        "arch": platform.machine(),
        "agent": agent_version(),
        "ts": int(time.time()),
    }
    if node_name() == "werkstatt":
        info["ollama"] = ollama_status()
    return info


def ollama_url() -> str:
    return os.environ.get("NORDWIND_OLLAMA_URL", "http://127.0.0.1:11434").rstrip("/")


def ollama_status(opener=None) -> dict:
    try:
        r = (opener or urllib.request.urlopen)(ollama_url() + "/api/tags", timeout=2)
        data = json.loads(r.read().decode())
        return {"ok": True, "models": [m.get("name") for m in data.get("models", [])][:5]}
    except Exception as e:
        return {"ok": False, "error": str(e)[:120]}


def build_prompt(topic: str) -> str:
    return (
        "Du bist die Disposition der Nordwind Bootsservice GmbH. Schreibe ein kurzes "
        f"Crew-Briefing auf Deutsch (maximal 90 Woerter) zum Thema: {topic}. "
        "Struktur: Begruessung, Ablauf in drei Schritten, ein Sicherheitshinweis. "
        "Nenne KEINE konkreten Wetterwerte, Uhrzeiten oder Entfernungen - diese Fakten "
        "liefert das Hafenbuero separat."
    )


def svc_draft_text(req: dict, opener=None) -> dict:
    topic = clean_topic(req.get("topic"))
    model = os.environ.get("NORDWIND_OLLAMA_MODEL", "llama3.2")
    body = json.dumps({"model": model, "prompt": build_prompt(topic), "stream": False, "think": False,
                       "options": {"num_predict": 220, "temperature": 0.4}}).encode()
    http = urllib.request.Request(ollama_url() + "/api/generate", data=body, method="POST",
                                  headers={"content-type": "application/json"})
    t0 = time.monotonic()
    try:
        r = (opener or urllib.request.urlopen)(http, timeout=100)
        data = json.loads(r.read().decode())
    except Exception as e:
        raise ServiceError(f"Ollama nicht erreichbar oder Modell fehlt ({model}): {str(e)[:160]}")
    text = (data.get("response") or "").strip()
    if not text:
        raise ServiceError("Ollama lieferte keinen Text")
    return {
        "text": text, "model": data.get("model", model), "llm_ms": int((time.monotonic() - t0) * 1000),
        "tokens": data.get("eval_count"), "node": "werkstatt",
        "gpu": os.environ.get("NORDWIND_GPU_LABEL") or None,
    }


def next_saturday(today: _dt.date) -> _dt.date:
    # weekday(): Mon=0 .. Sat=5. If today is Saturday the *next* one (a briefing is for the future).
    return today + _dt.timedelta(days=(5 - today.weekday()) % 7 or 7)


def weather_url(lat: float, lon: float) -> str:
    base = os.environ.get("NORDWIND_WEATHER_URL", "https://api.open-meteo.com/v1/forecast")
    return (f"{base}?latitude={lat}&longitude={lon}&timezone=Europe%2FBerlin&wind_speed_unit=kn&forecast_days=10"
            "&daily=wind_speed_10m_max,wind_gusts_10m_max,wind_direction_10m_dominant,"
            "precipitation_sum,temperature_2m_max,temperature_2m_min")


def pick_day(daily: dict, day: _dt.date):
    try:
        i = daily["time"].index(day.isoformat())
    except (KeyError, ValueError):
        return None
    g = lambda k: (daily.get(k) or [None] * (i + 1))[i]  # noqa: E731
    return {
        "datum": day.isoformat(), "wind_kn_max": g("wind_speed_10m_max"), "boeen_kn_max": g("wind_gusts_10m_max"),
        "windrichtung_grad": g("wind_direction_10m_dominant"), "regen_mm": g("precipitation_sum"),
        "temp_min_c": g("temperature_2m_min"), "temp_max_c": g("temperature_2m_max"),
    }


def svc_route_wetter(req: dict, opener=None, today=None) -> dict:
    with open(os.environ.get("NORDWIND_ROUTE_FILE", os.path.join(HERE, "data", "route-bremen.json")), encoding="utf-8") as f:
        route = json.load(f)
    day = next_saturday(today or _dt.date.today())
    loc = route["wetter_ort"]
    try:
        r = (opener or urllib.request.urlopen)(weather_url(loc["lat"], loc["lon"]), timeout=8)
        daily = json.loads(r.read().decode()).get("daily") or {}
    except Exception as e:
        raise ServiceError(f"Wetterquelle nicht erreichbar: {str(e)[:160]}")
    w = pick_day(daily, day)
    if w is None:
        raise ServiceError(f"Wetterquelle liefert keinen Tag {day.isoformat()}")
    total = sum(l["km"] for l in route["legs"])
    return {
        "node": "hafenbuero", "topic": clean_topic(req.get("topic")),
        "route": {"name": route["name"], "legs": route["legs"], "gesamt_km": total,
                  "hinweise": route["hinweise"], "quelle": route["quelle"]},
        "wetter": {**w, "ort": loc["name"], "quelle": "Open-Meteo (live abgefragt vom Hafenbüro)"},
    }


def lastseen_path(channel: str) -> str:
    return os.path.join(os.environ.get("NORDWIND_STATE_DIR", "/tmp"), f"lastseen-{channel}")


def touch_lastseen(channel: str):
    try:
        with open(lastseen_path(channel), "w") as f:
            f.write(str(int(time.time())))
    except OSError:
        pass


def read_lastseen(channel: str, now=None):
    try:
        with open(lastseen_path(channel)) as f:
            return int(f.read().strip())
    except (OSError, ValueError):
        return None


PEERS = {"werkstatt": "zentrale-werkstatt", "hafenbuero": "zentrale-hafenbuero"}
AUSSENDIENST_ONLINE_WINDOW_S = 300


def svc_mesh_status(_req: dict, caller=None) -> dict:
    call = caller or call_peer
    results: dict = {}

    def probe(peer, channel):
        r = call(channel, "ping", {}, 25.0)
        info = None
        if r["ok"]:
            try:
                info = json.loads(r["reply"])
            except ValueError:
                info = None
        results[peer] = {"online": bool(r["ok"] and info), "rtt_ms": r["dur_ms"], "sent_bytes": r["sent_bytes"],
                         "recv_bytes": r["recv_bytes"], "transport": r["transport"], "info": info,
                         "error": None if r["ok"] else r["error"]}

    threads = [threading.Thread(target=probe, args=(p, c)) for p, c in PEERS.items()]
    [t.start() for t in threads]
    [t.join() for t in threads]
    seen = read_lastseen("aussendienst-zentrale")
    ago = None if seen is None else max(0, int(time.time()) - seen)
    results["aussendienst"] = {"online": ago is not None and ago < AUSSENDIENST_ONLINE_WINDOW_S,
                               "last_seen_s_ago": ago, "info": None, "rtt_ms": None}
    return {"node": "zentrale", "os": platform.system(), "arch": platform.machine(), "agent": agent_version(),
            "ts": int(time.time()), "peers": results}


def svc_toern_briefing(req: dict, caller=None, reporter=None) -> dict:
    """The orchestration. Zentrale -> Werkstatt (LLM draft) -> Hafenbuero (facts) -> result."""
    call = caller or call_peer
    topic = clean_topic(req.get("topic"))
    rep = reporter or Reporter(req.get("report"), req.get("token"))
    t_run = time.monotonic()
    hops: list = []
    out = {"ok": False, "topic": topic, "hops": hops, "telemetry": None}

    def hop(seq, peer, channel, service, payload):
        base = {"run": req.get("run"), "seq": seq, "from": "zentrale", "to": peer, "channel": channel,
                "service": service, "t0_ms": int((time.monotonic() - t_run) * 1000)}
        rep.hop({**base, "state": "start"})
        r = call(channel, service, payload, 100.0)
        done = {**base, "state": "ok" if r["ok"] else "error", "dur_ms": r["dur_ms"],
                "sent_bytes": r["sent_bytes"], "recv_bytes": r["recv_bytes"], "transport": r["transport"],
                "plain_out": r["plain_out"], "plain_in": r["plain_in"], "error": r["error"]}
        hops.append(done)
        rep.hop(done)
        return r

    d = hop(2, "werkstatt", PEERS["werkstatt"], "draft_text", {"topic": topic})
    if not d["ok"]:
        out["error"] = f"Werkstatt: {d['error']}"
        return _finish(out, rep, t_run)
    f = hop(3, "hafenbuero", PEERS["hafenbuero"], "route_wetter", {"topic": topic})
    if not f["ok"]:
        out["error"] = f"Hafenbüro: {f['error']}"
        return _finish(out, rep, t_run)
    try:
        draft, facts = json.loads(d["reply"]), json.loads(f["reply"])
    except ValueError as e:
        out["error"] = f"Antwort nicht lesbar: {e}"
        return _finish(out, rep, t_run)
    briefing = {"titel": topic, "text": draft.get("text"), "modell": draft.get("model"),
                "llm_ms": draft.get("llm_ms"), "gpu": draft.get("gpu"),
                "route": facts.get("route"), "wetter": facts.get("wetter")}
    # Hop 4 (only while the Außendienst node is actually connected; otherwise the run honestly
    # covers three sites and says so - never a made-up field result).
    seen = read_lastseen(FIELD_CHANNEL)
    if seen is not None and time.time() - seen < FIELD_ONLINE_WINDOW_S:
        fr = field_hop(4, req.get("run"), briefing, rep, t_run, hops)
        if not fr["ok"]:
            out["error"] = f"Außendienst: {fr['error']}"
            return _finish(out, rep, t_run)
        briefing["aussendienst"] = fr["result"]
    else:
        briefing["aussendienst"] = None
    out["ok"] = True
    out["briefing"] = briefing
    return _finish(out, rep, t_run)


# ------------------------------------------------------------------- Außendienst (field node)
FIELD_CHANNEL = "aussendienst-zentrale"
FIELD_ONLINE_WINDOW_S = 90      # a connected field node polls every <= ~30 s
FIELD_WAIT_S = 45.0             # job handed over + result back
FIELD_POLL_S = 20.0             # long-poll window of one feld_abruf
RUN_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,32}$")
CRUISE_KMH = 9.0                # assumption of the field sheet (about 5 kn through the water)


def field_dir(sub: str) -> str:
    d = os.path.join(os.environ.get("NORDWIND_STATE_DIR", "/tmp"), "field", sub)
    os.makedirs(d, exist_ok=True)
    return d


def _write_json_atomic(path: str, obj) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, separators=(",", ":"))
    os.replace(tmp, path)


def field_job_for(briefing: dict, run: str) -> dict:
    """What Zentrale hands to the field node: the facts it needs, not the whole briefing."""
    w, r = briefing.get("wetter") or {}, briefing.get("route") or {}
    return {"run": run, "titel": briefing.get("titel"), "route": {"name": r.get("name"), "legs": r.get("legs"), "gesamt_km": r.get("gesamt_km")},
            "wetter": {k: w.get(k) for k in ("datum", "ort", "wind_kn_max", "boeen_kn_max", "regen_mm", "temp_min_c", "temp_max_c")}}


def svc_feld_abruf(req: dict, sleep=time.sleep, clock=time.monotonic) -> dict:
    """Long-poll by the field node: hand out the oldest pending job (claimed atomically) or time out empty."""
    wait = min(max(float(req.get("wait") or 0), 0.0), 25.0) if isinstance(req.get("wait"), (int, float)) else 0.0
    inbox, claimed = field_dir("inbox"), field_dir("claimed")
    end = clock() + wait
    while True:
        names = sorted((f for f in os.listdir(inbox) if f.endswith(".json")), key=lambda f: os.path.getmtime(os.path.join(inbox, f)))
        for name in names:
            src = os.path.join(inbox, name)
            try:
                os.replace(src, os.path.join(claimed, name))
                with open(os.path.join(claimed, name), encoding="utf-8") as f:
                    return {"job": json.load(f)}
            except (OSError, ValueError):
                continue
        if clock() >= end:
            return {"job": None}
        sleep(0.4)


def svc_feld_quittung(req: dict) -> dict:
    run = req.get("run")
    if not isinstance(run, str) or not RUN_ID_RE.match(run):
        raise ServiceError("quittung without valid run id")
    body = json.dumps(req, ensure_ascii=False)
    if len(body) > 8000:
        raise ServiceError("quittung too large")
    _write_json_atomic(os.path.join(field_dir("outbox"), run + ".json"), req)
    return {"ok": True}


def field_hop(seq: int, run, briefing: dict, rep: Reporter, t_run: float, hops: list, poll_s: float = 0.4) -> dict:
    """Hop 4: hand the job to the field node's pending poll and wait for its signed-off result."""
    run = run if isinstance(run, str) and RUN_ID_RE.match(run) else "r" + str(now_ms())
    inbox, outbox = field_dir("inbox"), field_dir("outbox")
    try:
        os.remove(os.path.join(outbox, run + ".json"))
    except OSError:
        pass
    base = {"run": run, "seq": seq, "from": "zentrale", "to": "aussendienst", "channel": FIELD_CHANNEL,
            "service": "feld_auftrag", "t0_ms": int((time.monotonic() - t_run) * 1000)}
    rep.hop({**base, "state": "start"})
    t0 = time.monotonic()
    job = field_job_for(briefing, run)
    job_path = os.path.join(inbox, run + ".json")
    _write_json_atomic(job_path, job)
    res_path = os.path.join(outbox, run + ".json")
    got, err = None, None
    while time.monotonic() - t0 < FIELD_WAIT_S:
        if os.path.exists(res_path):
            try:
                with open(res_path, encoding="utf-8") as f:
                    got = json.load(f)
            except (OSError, ValueError) as e:
                err = f"Quittung nicht lesbar: {e}"
            break
        time.sleep(poll_s)
    else:
        err = f"keine Quittung innerhalb {int(FIELD_WAIT_S)}s (Auftrag nicht abgeholt oder nicht bearbeitet)"
    for leftover in (job_path, res_path):
        try:
            os.remove(leftover)
        except OSError:
            pass
    dur = int((time.monotonic() - t0) * 1000)
    wire = (got or {}).get("wire") if isinstance(got, dict) else None
    wire = wire if isinstance(wire, dict) else {}
    num = lambda k: wire.get(k) if isinstance(wire.get(k), int) else None  # noqa: E731
    result = clean_field_result((got or {}).get("ergebnis")) if got else None
    if got and result is None:
        err = "Quittung ohne gueltiges Ergebnis"
    done = {**base, "state": "ok" if not err else "error", "dur_ms": dur,
            "sent_bytes": num("sent_bytes"), "recv_bytes": num("recv_bytes"),
            "transport": wire.get("transport") if isinstance(wire.get("transport"), str) else None,
            "plain_out": len(json.dumps(job, ensure_ascii=False).encode()),
            "plain_in": len(json.dumps(got, ensure_ascii=False).encode()) if got else 0, "error": err}
    hops.append(done)
    rep.hop(done)
    return {"ok": not err, "error": err, "result": result}


def clean_field_result(r):
    """Bound what the field node may put into the briefing (it is untrusted input for Zentrale)."""
    if not isinstance(r, dict):
        return None
    txt = lambda v, n: v[:n] if isinstance(v, str) else None  # noqa: E731
    legs = [{"von": txt(l.get("von"), 40), "nach": txt(l.get("nach"), 40), "km": l.get("km") if isinstance(l.get("km"), (int, float)) else None,
             "fahrzeit_min": l.get("fahrzeit_min") if isinstance(l.get("fahrzeit_min"), int) else None}
            for l in (r.get("etappen") or [])[:8] if isinstance(l, dict)]
    return {"geraet": txt(r.get("geraet"), 120), "titel": txt(r.get("titel"), MAX_TOPIC_LEN), "freigabe": txt(r.get("freigabe"), 40),
            "begruendung": txt(r.get("begruendung"), 300), "etappen": legs, "fahrzeit_min": r.get("fahrzeit_min") if isinstance(r.get("fahrzeit_min"), int) else None,
            "checkliste": [txt(c, 160) for c in (r.get("checkliste") or [])[:8] if isinstance(c, str)], "zettel": txt(r.get("zettel"), 900)}


def field_sheet(job: dict, device: str) -> dict:
    """The field node's own work: a rule-based sheet derived from the job (route + weather of THIS run).
    No model, no invented data: every figure comes from the job or from the stated assumptions."""
    w, route = job.get("wetter") or {}, job.get("route") or {}
    wind, boe, rain = w.get("wind_kn_max"), w.get("boeen_kn_max"), w.get("regen_mm")
    if not isinstance(wind, (int, float)) or not isinstance(boe, (int, float)):
        raise ServiceError("Auftrag ohne Windwerte")
    if boe >= 34 or wind >= 28:
        freigabe, why = "abgeraten", "Sturm: Boeen ab 34 kn bzw. Wind ab 28 kn"
    elif boe >= 28 or wind >= 22:
        freigabe, why = "mit Einschraenkung", "Starkwind: Reff vorbereiten, Etappen verkuerzen"
    else:
        freigabe, why = "freigegeben", "Wind und Boeen im Rahmen der Toernplanung"
    legs = []
    for l in (route.get("legs") or [])[:8]:
        km = l.get("km")
        if isinstance(km, (int, float)):
            legs.append({"von": l.get("von"), "nach": l.get("nach"), "km": km, "fahrzeit_min": round(km / CRUISE_KMH * 60)})
    total_min = sum(l["fahrzeit_min"] for l in legs)
    check = ["Funkkanal 16 abhoeren, Berufsschiffahrt hat Vorrang", "Tidenfahrplan gegen die Etappen pruefen"]
    if freigabe != "freigegeben":
        check.insert(0, "Reff einbinden, Rettungswesten auch fuer die Crew bereithalten")
    if isinstance(rain, (int, float)) and rain >= 2:
        check.append(f"Regenzeug an Bord ({rain} mm erwartet)")
    h, m = divmod(total_min, 60)
    zettel = (f"Feldzettel {job.get('titel')}: {w.get('datum')} {w.get('ort')}, Wind {wind} kn, Boeen {boe} kn -> {freigabe}. "
              + " | ".join(f"{l['von']}-{l['nach']} {l['km']} km (~{l['fahrzeit_min']} min)" for l in legs)
              + f" | gesamt {route.get('gesamt_km')} km, ~{h} h {m:02d} min bei {CRUISE_KMH:g} km/h.")
    return {"geraet": device, "titel": job.get("titel"), "freigabe": freigabe, "begruendung": f"{why} ({wind} kn / {boe} kn am {w.get('datum')})",
            "etappen": legs, "fahrzeit_min": total_min, "checkliste": check, "zettel": zettel}


def device_label() -> str:
    # Honest self-description: what this node really is, never a pretended phone.
    return f"{platform.system()} {platform.machine()} (Feldknoten, regelbasiert, ohne Modell)"


def field_loop(caller=None, sleep=time.sleep, once: bool = False) -> int:
    """Run forever as the Außendienst node: poll Zentrale, work the job, hand the result back."""
    call = caller or call_peer
    backoff = 2.0
    print(f"nordwind[aussendienst]: polling {FIELD_CHANNEL} every <= {int(FIELD_POLL_S)}s", flush=True)
    while True:
        r = call(FIELD_CHANNEL, "feld_abruf", {"wait": FIELD_POLL_S, "os": platform.system(), "arch": platform.machine()}, FIELD_POLL_S + 25.0)
        if not r["ok"]:
            print(f"nordwind[aussendienst]: poll failed: {r['error']}", flush=True)
            if once:
                return 1
            sleep(backoff)
            backoff = min(backoff * 2, 30.0)
            continue
        backoff = 2.0
        try:
            job = json.loads(r["reply"]).get("job")
        except ValueError:
            job = None
        if job:
            try:
                sheet = field_sheet(job, device_label())
                q = {"run": job.get("run"), "ergebnis": sheet,
                     "wire": {"sent_bytes": r["sent_bytes"], "recv_bytes": r["recv_bytes"], "transport": r["transport"], "poll_ms": r["dur_ms"]}}
            except ServiceError as e:
                q = {"run": job.get("run"), "ergebnis": None, "fehler": str(e)}
            qr = call(FIELD_CHANNEL, "feld_quittung", q, 30.0)
            print(f"nordwind[aussendienst]: job {job.get('run')} -> {'ok' if qr['ok'] else 'quittung failed: ' + str(qr['error'])}", flush=True)
        if once:
            return 0


def _finish(out: dict, rep: Reporter, t_run: float) -> dict:
    out["total_ms"] = int((time.monotonic() - t_run) * 1000)
    out["telemetry"] = {"sent": rep.sent, "failed": rep.failed} if rep.url else None
    return out


class ServiceError(Exception):
    pass


HANDLERS = {
    "ping": svc_ping, "draft_text": svc_draft_text, "route_wetter": svc_route_wetter,
    "mesh_status": svc_mesh_status, "toern_briefing": svc_toern_briefing,
    "feld_abruf": svc_feld_abruf, "feld_quittung": svc_feld_quittung,
}


def dispatch(service: str, raw: str, node=None) -> dict:
    node = node or node_name()
    if service not in NODE_SERVICES.get(node, []):
        raise ServiceError(f"service '{service}' is not offered by node '{node}'")
    raw = (raw or "").strip()
    req: dict = {}
    if raw:
        try:
            v = json.loads(raw)
            req = v if isinstance(v, dict) else {"topic": str(v)}
        except ValueError:
            req = {"topic": raw}
    return HANDLERS[service](req)


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "field":
        return field_loop()
    service = os.environ.get("CT_SERVICE_TYPE", "")
    chan = os.environ.get("NORDWIND_SERVING_CHANNEL")
    if chan:
        touch_lastseen(chan)
    raw = sys.stdin.read()
    try:
        res = dispatch(service, raw)
    except ServiceError as e:
        print(f"nordwind[{node_name()}/{service}]: {e}", file=sys.stderr)
        return 3
    print(json.dumps(res, ensure_ascii=False, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
