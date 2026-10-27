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


if __name__ == "__main__":
    unittest.main()
