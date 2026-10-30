#!/usr/bin/env python3
"""LAB-ONLY acceptor supervisor. In direct-address mode a `ct-agent channel` acceptor serves ONE
session and uses an ephemeral certificate; the persistent multi-session serve loop exists on the
broker path only (production). To exercise many calls locally without an edge, this wrapper
restarts the real acceptor after every session and publishes the fresh cert to a file, which lab
initiators read (`NW_CH_<CHANNEL>_PEER_CERT_FILE`). Everything else is the real ct-agent.

usage: LAB_ENV='{"CT_...": "..."}' lab_acceptor.py CERT_FILE"""
import json, os, re, subprocess, sys

cert_file, env = sys.argv[1], dict(os.environ, **json.loads(os.environ["LAB_ENV"]))
bin_ = env.get("CT_AGENT_BIN", "ct-agent")


def drop():
    try:
        os.remove(cert_file)
    except OSError:
        pass


while True:
    p = subprocess.Popen([bin_, "channel"], env=env, stderr=subprocess.PIPE, stdout=subprocess.DEVNULL, text=True)
    for line in p.stderr:
        m = re.search(r"CT_CHANNEL_PEER_CERT=([0-9a-f]+)", line)
        if m:
            tmp = cert_file + ".tmp"
            open(tmp, "w").write(m.group(1))
            os.replace(tmp, cert_file)
        if "channel_session" in line and "state=close" in line:
            drop()
            sys.stderr.write(line); sys.stderr.flush()
            p.terminate()  # one session per direct-address acceptor: restart for a fresh cert
            break
        sys.stderr.write(line); sys.stderr.flush()
    drop()
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()
