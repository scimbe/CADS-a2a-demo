#!/usr/bin/env python3
"""TEST-ONLY stand-ins for the two external data sources, used by the local lab / integration
test so it runs without a GPU or internet: an Ollama-compatible endpoint and a forecast JSON.
Their output is labelled as stub text. Never deployed, never part of the production path.

usage: lab_stubs.py OLLAMA_PORT WEATHER_PORT"""
import datetime as dt, json, sys, threading
from http.server import BaseHTTPRequestHandler, HTTPServer


class Quiet(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)


class Ollama(Quiet):
    def do_GET(self):
        self._send({"models": [{"name": "stub-llm"}]})

    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        self._send({"model": "stub-llm", "response": "Moin Crew, (Test-Stub) 1. Ablegen 2. Weser abwaerts 3. Anlegen.", "eval_count": 12})


class Weather(Quiet):
    def do_GET(self):
        d0 = dt.date.today()
        days = [(d0 + dt.timedelta(days=i)).isoformat() for i in range(10)]
        n = len(days)
        self._send({"daily": {"time": days, "wind_speed_10m_max": [11.5] * n, "wind_gusts_10m_max": [19.0] * n,
                              "wind_direction_10m_dominant": [250] * n, "precipitation_sum": [0.0] * n,
                              "temperature_2m_max": [13.0] * n, "temperature_2m_min": [8.0] * n}})


if __name__ == "__main__":
    for port, h in ((int(sys.argv[1]), Ollama), (int(sys.argv[2]), Weather)):
        threading.Thread(target=HTTPServer(("127.0.0.1", port), h).serve_forever, daemon=True).start()
    print("stubs-ready", flush=True)
    threading.Event().wait()
