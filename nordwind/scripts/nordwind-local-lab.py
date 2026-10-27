#!/usr/bin/env python3
"""Local lab: the whole Nordwind mesh on one machine with REAL processes, no account, no edge.

  Zentrale / Werkstatt / Hafenbuero = real `ct-agent channel` acceptors running the real node
  handler; the hub = the real `nordwind-hub` binary (NW_LAB=1: login gate off, control plane
  replaced by the local manifest, and the page says so). Transport is direct-address Noise
  instead of the edge. The LLM and the weather source are the test stubs in lab_stubs.py
  (their output says "Test-Stub").

  cargo build --bin nordwind-hub
  CT_AGENT_BIN=/path/to/ct-agent python3 nordwind/scripts/nordwind-local-lab.py   # then open the URL
"""
import json, os, re, signal, socket, subprocess, sys, tempfile, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
BIN = os.environ.get("CT_AGENT_BIN", "ct-agent")
HUB = os.environ.get("NW_HUB_BIN", os.path.join(ROOT, "target", "debug", "nordwind-hub"))
PORT = int(os.environ.get("NW_LAB_PORT", "8791"))
procs = []


def free(kind):
    s = socket.socket(socket.AF_INET, kind)
    s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


def ident():
    t = subprocess.run([BIN, "channel", "init"], capture_output=True, text=True, check=True).stdout
    g = lambda k: re.search(k + r"=([0-9a-f]{64})", t).group(1)
    return dict(hk=g("CT_CHANNEL_HOLDER_KEY"), hp=g("CT_CHANNEL_HOLDER_PUBKEY"), nk=g("CT_CHANNEL_NOISE_KEY"), np=g("CT_CHANNEL_NOISE_PUBKEY"))


def acceptor(tmp, node, ch, port, me, peer, extra):
    """Start a (restarting) real acceptor; returns the cert FILE initiators read."""
    env = dict(CT_AGENT_STATE_DIR=f"{tmp}/st-{node}-{ch}", CT_CHANNEL_ROLE="accept", CT_CHANNEL_SERVE="1",
               CT_CHANNEL_ADDR=f"127.0.0.1:{port}", CT_CHANNEL_NOISE_KEY=me["nk"], CT_CHANNEL_PEER_NOISE_KEY=peer["np"],
               CT_AGENT_SERVICES={"zentrale": "toern_briefing,mesh_status,ping", "werkstatt": "draft_text,ping"}.get(node, "route_wetter,ping"),
               CT_AGENT_SERVICE_HANDLER_CMD=f"python3 {ROOT}/nordwind/node/nordwind_node.py", CT_AGENT_BIN=BIN, NORDWIND_NODE=node,
               NORDWIND_SERVING_CHANNEL=ch, NORDWIND_STATE_DIR=tmp, **extra)
    cert = f"{tmp}/{node}-{ch}.cert"
    log = open(f"{tmp}/{node}-{ch}.log", "w")
    procs.append(subprocess.Popen([sys.executable, f"{ROOT}/nordwind/node/lab_acceptor.py", cert], env=dict(os.environ, LAB_ENV=json.dumps(env)), stderr=log))
    for _ in range(150):
        if os.path.exists(cert):
            return cert
        time.sleep(0.1)
    raise SystemExit(f"{node}: acceptor did not publish a certificate")


def main():
    tmp = tempfile.mkdtemp(prefix="nw-lab-")
    cloud, zen, wer, haf, aus = (ident() for _ in range(5))
    p_oll, p_wx = free(socket.SOCK_STREAM), free(socket.SOCK_STREAM)
    procs.append(subprocess.Popen([sys.executable, f"{ROOT}/nordwind/node/lab_stubs.py", str(p_oll), str(p_wx)], stdout=subprocess.DEVNULL))
    pw, ph, pz = (free(socket.SOCK_DGRAM) for _ in range(3))
    cw = acceptor(tmp, "werkstatt", "zentrale-werkstatt", pw, wer, zen, dict(NORDWIND_OLLAMA_URL=f"http://127.0.0.1:{p_oll}"))
    ch = acceptor(tmp, "hafenbuero", "zentrale-hafenbuero", ph, haf, zen, dict(NORDWIND_WEATHER_URL=f"http://127.0.0.1:{p_wx}/v1/forecast"))
    d = lambda pre, port, peer, cert: {pre + "DIRECT_ADDR": f"127.0.0.1:{port}", pre + "PEER_NOISE_KEY": peer["np"], pre + "PEER_CERT_FILE": cert}
    zx = {**d("NW_CH_ZENTRALE_WERKSTATT_", pw, wer, cw), **d("NW_CH_ZENTRALE_HAFENBUERO_", ph, haf, ch), "NW_NOISE_KEY": zen["nk"]}
    cz = acceptor(tmp, "zentrale", "cloud-zentrale", pz, zen, cloud, zx)
    nd = lambda i, de, en, o, r, p, idn: dict(id=i, label_de=de, label_en=en, holder_pubkey=idn["hp"], os_hint=o, role=r, planned=p)
    manifest = dict(
        nodes=[nd("cloud", "Cloud-Kern", "Cloud core", "Linux", "cloud", False, cloud), nd("zentrale", "Zentrale Hamburg", "HQ Hamburg", "macOS", "hub", False, zen),
               nd("hafenbuero", "Hafenbüro Bremen", "Harbour office Bremen", "Linux", "site", False, haf), nd("werkstatt", "Werkstatt (GPU)", "Workshop (GPU)", "Linux", "gpu", False, wer),
               nd("aussendienst", "Außendienst (Android)", "Field service (Android)", "Android", "mobile", True, aus)],
        channels=[dict(name=n, a=a, b=b, id="") for n, a, b in [("cloud-zentrale", "cloud", "zentrale"), ("zentrale-werkstatt", "zentrale", "werkstatt"),
                                                                  ("zentrale-hafenbuero", "zentrale", "hafenbuero"), ("aussendienst-zentrale", "aussendienst", "zentrale")]])
    mf = f"{tmp}/nodes.json"
    json.dump(manifest, open(mf, "w"))
    env = dict(os.environ, NW_LAB="1", NW_NODES_FILE=mf, NW_LISTEN=f"127.0.0.1:{PORT}", CT_AGENT_BIN=BIN, CT_AGENT_STATE_DIR=f"{tmp}/hub",
               NW_NOISE_KEY=cloud["nk"], NW_PUBLIC_URL=f"http://127.0.0.1:{PORT}", **d("NW_CH_CLOUD_ZENTRALE_", pz, zen, cz))
    procs.append(subprocess.Popen([HUB], env=env))
    print(f"logs: {tmp}/*.log")
    print(f"\nLAB READY: http://127.0.0.1:{PORT}/   (Ctrl-C to stop)", flush=True)
    signal.pause()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
    finally:
        for p in procs:
            p.terminate()
