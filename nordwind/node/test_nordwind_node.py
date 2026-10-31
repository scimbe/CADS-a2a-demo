import json
import io
import datetime as dt, io, json, os, unittest
import nordwind_node as n

OK_ENV = json.dumps({"ok": True, "output": json.dumps({"x": 1})})
ERR_LOG = ("ct-agent channel: relay leg via QUIC (e:4436) (#106)\n"
           "ct-agent channel: status uptime=0s sent=98B recv=50B\n"
           "ct-agent channel: status uptime=1s sent=224B recv=304B\n")


class Pure(unittest.TestCase):
    def test_status_takes_last_line(self):
        self.assertEqual(n.parse_status_line(ERR_LOG), (224, 304))
        self.assertEqual(n.parse_status_line(""), (None, None))

    def test_transport(self):
        self.assertEqual(n.transport_from_stderr(ERR_LOG), "quic")
        self.assertEqual(n.transport_from_stderr("relay leg via the :443 front door"), "tcp_443")

    def test_envelope(self):
        self.assertEqual(n.unwrap_envelope(OK_ENV), ('{"x": 1}', None))
        self.assertIsNone(n.unwrap_envelope('{"ok":false,"error":"boom"}')[0])
        self.assertIn("boom", n.unwrap_envelope('{"ok":false,"error":"boom"}')[1])
        self.assertIsNone(n.unwrap_envelope("garbage")[0])

    def test_topic_is_sanitised(self):
        self.assertEqual(n.clean_topic(None), n.DEFAULT_TOPIC)
        self.assertEqual(n.clean_topic("a\nb\x00c"), "a b c")
        self.assertEqual(len(n.clean_topic("x" * 999)), n.MAX_TOPIC_LEN)

    def test_next_saturday(self):
        self.assertEqual(n.next_saturday(dt.date(2026, 10, 27)), dt.date(2026, 10, 31))
        self.assertEqual(n.next_saturday(dt.date(2026, 10, 31)), dt.date(2026, 11, 7))

    def test_dispatch_refuses_foreign_service(self):
        with self.assertRaises(n.ServiceError):
            n.dispatch("draft_text", "{}", node="hafenbuero")


class CallPeer(unittest.TestCase):
    def setUp(self):
        os.environ.update({"NW_NOISE_KEY": "11" * 32, "NW_HOLDER_KEY": "22" * 32,
                           "NW_CH_ZENTRALE_WERKSTATT_GRANT": "g"})

    def test_success_measures_real_fields(self):
        r = n.call_peer("zentrale-werkstatt", "ping", {}, runner=lambda e, i, t: (0, OK_ENV + "\n", ERR_LOG))
        self.assertTrue(r["ok"])
        self.assertEqual((r["sent_bytes"], r["recv_bytes"], r["transport"]), (224, 304, "quic"))
        self.assertEqual(json.loads(r["reply"]), {"x": 1})

    def test_failure_is_reported_not_hidden(self):
        r = n.call_peer("zentrale-werkstatt", "ping", {}, runner=lambda e, i, t: (1, "", "refused\n"))
        self.assertFalse(r["ok"]); self.assertIn("refused", r["error"])

    def test_unprovisioned_channel(self):
        r = n.call_peer("zentrale-hafenbuero", "ping", {})
        self.assertFalse(r["ok"]); self.assertIn("not provisioned", r["error"])

    def test_env_plane_vs_lab(self):
        e = n.initiator_env("zentrale-werkstatt", "ping")
        self.assertEqual(e["CT_CHANNEL_GRANT"], "g"); self.assertNotIn("CT_CHANNEL_ADDR", e)
        os.environ["NW_CH_ZENTRALE_WERKSTATT_DIRECT_ADDR"] = "127.0.0.1:1"
        try:
            self.assertEqual(n.initiator_env("zentrale-werkstatt", "ping")["CT_CHANNEL_ADDR"], "127.0.0.1:1")
        finally:
            del os.environ["NW_CH_ZENTRALE_WERKSTATT_DIRECT_ADDR"]


class Services(unittest.TestCase):
    def test_route_wetter_uses_live_source_and_fails_loudly(self):
        daily = {"time": ["2026-10-31"], "wind_speed_10m_max": [14.2], "wind_gusts_10m_max": [22],
                 "wind_direction_10m_dominant": [240], "precipitation_sum": [0.4],
                 "temperature_2m_max": [12], "temperature_2m_min": [7]}
        op = lambda url, timeout: io.BytesIO(json.dumps({"daily": daily}).encode())
        r = n.svc_route_wetter({}, opener=op, today=dt.date(2026, 10, 27))
        self.assertEqual(r["wetter"]["wind_kn_max"], 14.2); self.assertEqual(r["route"]["gesamt_km"], 88)
        def down(url, timeout): raise OSError("net down")
        with self.assertRaises(n.ServiceError):
            n.svc_route_wetter({}, opener=down, today=dt.date(2026, 10, 27))

    def test_draft_text_no_fallback_when_llm_down(self):
        def down(req, timeout): raise OSError("refused")
        with self.assertRaises(n.ServiceError):
            n.svc_draft_text({"topic": "t"}, opener=down)
        ok = lambda req, timeout: io.BytesIO(json.dumps({"response": " Hallo ", "model": "m", "eval_count": 5}).encode())
        self.assertEqual(n.svc_draft_text({}, opener=ok)["text"], "Hallo")

    def test_draft_text_disables_thinking(self):
        seen = {}
        def op(req, timeout=None):
            seen.update(json.loads(req.data.decode()))
            return io.BytesIO(json.dumps({"response": "Hallo"}).encode())
        n.svc_draft_text({}, opener=op)
        self.assertIs(seen["think"], False)

    def test_orchestration_order_and_telemetry(self):
        calls, posted = [], []
        def caller(ch, svc, payload, to):
            calls.append((ch, svc))
            body = {"text": "T", "model": "m"} if svc == "draft_text" else {"route": {"name": "r"}, "wetter": {"wind_kn_max": 9}}
            return {"ok": True, "reply": json.dumps(body), "dur_ms": 5, "sent_bytes": 10, "recv_bytes": 20,
                    "transport": "quic", "error": None, "plain_out": 3, "plain_in": 4}
        rep = n.Reporter("https://x/y", "tok", poster=lambda b: posted.append(json.loads(b)))
        out = n.svc_toern_briefing({"run": "r1"}, caller=caller, reporter=rep)
        self.assertEqual(calls, [("zentrale-werkstatt", "draft_text"), ("zentrale-hafenbuero", "route_wetter")])
        self.assertTrue(out["ok"]); self.assertEqual(out["briefing"]["wetter"]["wind_kn_max"], 9)
        self.assertEqual([p["state"] for p in posted], ["start", "ok", "start", "ok"])
        self.assertEqual([h["to"] for h in out["hops"]], ["werkstatt", "hafenbuero"])

    def test_orchestration_stops_on_failed_hop(self):
        fail = lambda ch, s, p, t: {"ok": False, "reply": None, "dur_ms": 1, "sent_bytes": None, "recv_bytes": None,
                                    "transport": None, "error": "x", "plain_out": 0, "plain_in": 0}
        out = n.svc_toern_briefing({}, caller=fail, reporter=n.Reporter(None, None))
        self.assertFalse(out["ok"]); self.assertEqual(len(out["hops"]), 1); self.assertIn("Werkstatt", out["error"])

    def test_mesh_status(self):
        def caller(ch, svc, p, to):
            ok = ch.endswith("werkstatt")
            return {"ok": ok, "reply": json.dumps({"os": "Linux"}) if ok else None, "dur_ms": 7, "sent_bytes": 1,
                    "recv_bytes": 2, "transport": None, "error": None if ok else "down"}
        m = n.svc_mesh_status({}, caller=caller)["peers"]
        self.assertTrue(m["werkstatt"]["online"]); self.assertFalse(m["hafenbuero"]["online"])
        self.assertEqual(m["werkstatt"]["info"]["os"], "Linux")


JOB = {"run": "r9", "titel": "Toern-Briefing Samstag Bremen",
       "route": {"name": "Weser", "gesamt_km": 88, "legs": [{"von": "Bremen Schlachte", "nach": "Vegesack", "km": 21},
                                                           {"von": "Vegesack", "nach": "Brake", "km": 26}, {"von": "Brake", "nach": "Bremerhaven", "km": 41}]},
       "wetter": {"datum": "2026-11-07", "ort": "Weser / Bremerhaven", "wind_kn_max": 12.0, "boeen_kn_max": 20.0, "regen_mm": 0.4}}


class Field(unittest.TestCase):
    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()
        os.environ["NORDWIND_STATE_DIR"] = self.tmp

    def test_sheet_is_derived_from_the_job(self):
        s = n.field_sheet(JOB, "Linux x86_64")
        self.assertEqual(s["freigabe"], "freigegeben")
        self.assertEqual([l["fahrzeit_min"] for l in s["etappen"]], [140, 173, 273])
        self.assertEqual(s["fahrzeit_min"], 586)
        for needle in ("Samstag Bremen", "2026-11-07", "12.0 kn", "Vegesack-Brake 26 km", "88 km"):
            self.assertIn(needle, s["zettel"])

    def test_sheet_reacts_to_weather(self):
        storm = {**JOB, "wetter": {**JOB["wetter"], "wind_kn_max": 30.0, "boeen_kn_max": 41.0}}
        s = n.field_sheet(storm, "x")
        self.assertEqual(s["freigabe"], "abgeraten"); self.assertIn("Reff", s["checkliste"][0])
        with self.assertRaises(n.ServiceError):
            n.field_sheet({**JOB, "wetter": {}}, "x")

    def test_poll_claims_job_once_and_times_out_empty(self):
        self.assertEqual(n.svc_feld_abruf({"wait": 0})["job"], None)
        n._write_json_atomic(os.path.join(n.field_dir("inbox"), "r9.json"), JOB)
        self.assertEqual(n.svc_feld_abruf({"wait": 0})["job"]["run"], "r9")
        self.assertEqual(n.svc_feld_abruf({"wait": 0})["job"], None)

    def test_quittung_rejects_bad_run_id(self):
        with self.assertRaises(n.ServiceError):
            n.svc_feld_quittung({"run": "../x", "ergebnis": {}})

    def test_orchestration_has_field_hop_when_node_is_connected(self):
        import threading
        n.touch_lastseen(n.FIELD_CHANNEL)
        def caller(ch, svc, payload, to):
            body = {"text": "T", "model": "m"} if svc == "draft_text" else {"route": JOB["route"], "wetter": JOB["wetter"]}
            return {"ok": True, "reply": json.dumps(body), "dur_ms": 5, "sent_bytes": 10, "recv_bytes": 20, "transport": "tcp_443", "error": None, "plain_out": 3, "plain_in": 4}
        def field_node():  # what field_loop does against the real handlers
            job = n.svc_feld_abruf({"wait": 5}, sleep=lambda _: __import__("time").sleep(0.05))["job"]
            n.svc_feld_quittung({"run": job["run"], "ergebnis": n.field_sheet(job, "Linux test"), "wire": {"sent_bytes": 111, "recv_bytes": 222, "transport": "tcp_443"}})
        t = threading.Thread(target=field_node); t.start()
        posted = []
        out = n.svc_toern_briefing({"run": "r9"}, caller=caller, reporter=n.Reporter("https://x/y", "tok", poster=lambda b: posted.append(json.loads(b))))
        t.join()
        self.assertTrue(out["ok"], out)
        self.assertEqual([h["to"] for h in out["hops"]], ["werkstatt", "hafenbuero", "aussendienst"])
        h4 = out["hops"][2]
        self.assertEqual((h4["seq"], h4["state"], h4["sent_bytes"], h4["recv_bytes"]), (4, "ok", 111, 222))
        self.assertIn("Vegesack-Brake 26 km", out["briefing"]["aussendienst"]["zettel"])
        self.assertEqual(posted[-2]["service"], "feld_auftrag")

    def test_field_hop_fails_loudly_without_receipt(self):
        n.touch_lastseen(n.FIELD_CHANNEL)
        old = n.FIELD_WAIT_S; n.FIELD_WAIT_S = 0.3
        try:
            hops = []
            r = n.field_hop(4, "r8", {"titel": "t"}, n.Reporter(None, None), 0.0, hops, poll_s=0.05)
        finally:
            n.FIELD_WAIT_S = old
        self.assertFalse(r["ok"]); self.assertEqual(hops[0]["state"], "error")

    def test_orchestration_skips_field_hop_when_node_never_seen(self):
        caller = lambda ch, svc, p, to: {"ok": True, "reply": json.dumps({"text": "T"} if svc == "draft_text" else {"route": {}, "wetter": {}}),
                                          "dur_ms": 1, "sent_bytes": 1, "recv_bytes": 1, "transport": None, "error": None, "plain_out": 1, "plain_in": 1}
        out = n.svc_toern_briefing({"run": "r7"}, caller=caller, reporter=n.Reporter(None, None))
        self.assertTrue(out["ok"]); self.assertEqual(len(out["hops"]), 2); self.assertIsNone(out["briefing"]["aussendienst"])

    def test_loop_polls_then_hands_back_result(self):
        calls = []
        def caller(ch, svc, payload, to):
            calls.append(svc)
            rep = {"job": JOB} if svc == "feld_abruf" else {"ok": True}
            return {"ok": True, "reply": json.dumps(rep), "dur_ms": 4, "sent_bytes": 5, "recv_bytes": 6, "transport": "tcp_443", "error": None}
        self.assertEqual(n.field_loop(caller=caller, once=True), 0)
        self.assertEqual(calls, ["feld_abruf", "feld_quittung"])


if __name__ == "__main__":
    unittest.main()
