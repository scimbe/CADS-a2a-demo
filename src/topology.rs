//! Topology from the ACCOUNT (control plane `/me/topologies`) merged with live measurements.
//!
//! Truth hierarchy: the control-plane topology decides WHICH nodes/edges exist; the manifest
//! (public data written by provisioning) only adds display names and the holder->site
//! mapping; live state comes exclusively from real channel calls. If the control plane is
//! unreachable the graph falls back to the manifest and says so (`source.kind`).
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct NodeDef {
    pub id: String,
    pub label_de: String,
    pub label_en: String,
    pub holder_pubkey: String,
    #[serde(default)]
    pub os_hint: Option<String>,
    /// hub | site | gpu | mobile | cloud
    pub role: String,
    #[serde(default)]
    pub planned: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ChannelDef {
    pub name: String,
    pub a: String,
    pub b: String,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct Manifest {
    pub nodes: Vec<NodeDef>,
    pub channels: Vec<ChannelDef>,
}

impl Manifest {
    pub fn node_by_holder(&self, holder: &str) -> Option<&NodeDef> {
        self.nodes.iter().find(|n| n.holder_pubkey == holder)
    }
    pub fn node(&self, id: &str) -> Option<&NodeDef> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn channel_between(&self, a: &str, b: &str) -> Option<&ChannelDef> {
        self.channels.iter().find(|c| (c.a == a && c.b == b) || (c.a == b && c.b == a))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CpAgent {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub verified: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CpEdge {
    pub a: String,
    pub b: String,
    #[serde(default)]
    pub channel: Option<String>,
}

/// `GET /me/topologies/:id` (control-plane `TopologyView`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CpTopology {
    pub id: String,
    #[serde(default)]
    pub net_uuid: String,
    pub agents: Vec<CpAgent>,
    pub edges: Vec<CpEdge>,
    #[serde(default)]
    pub overlay_mode: String,
}

/// Choose the topology to show: the configured id, else the first whose agents include a
/// manifest holder (so a user with several topologies gets the Nordwind one).
pub fn pick_topology<'a>(views: &'a [CpTopology], manifest: &Manifest, wanted: Option<&str>) -> Option<&'a CpTopology> {
    if let Some(w) = wanted.filter(|w| !w.is_empty()) {
        return views.iter().find(|t| t.id == w);
    }
    views.iter().find(|t| t.agents.iter().any(|a| manifest.node_by_holder(&a.id).is_some()))
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NodeLive {
    pub online: bool,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub agent: Option<String>,
    pub rtt_ms: Option<u64>,
    pub detail: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct EdgeLive {
    pub online: bool,
    pub rtt_ms: Option<u64>,
    pub sent_bytes: Option<u64>,
    pub recv_bytes: Option<u64>,
    pub transport: Option<String>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Live {
    /// by manifest node id
    pub nodes: HashMap<String, NodeLive>,
    /// by channel name
    pub edges: HashMap<String, EdgeLive>,
    pub polled_at_ms: u64,
}

pub fn os_label(raw: &str) -> &'static str {
    match raw.to_ascii_lowercase().as_str() {
        "darwin" | "macos" => "macOS",
        "linux" => "Linux",
        "android" => "Android",
        "windows" => "Windows",
        _ => "?",
    }
}

/// Fold a `mesh_status` reply (Zentrale -> neighbours, all real calls) plus the hub's own call
/// to Zentrale into `Live`. `hub_call_*` describe the measured hub->Zentrale call.
pub fn apply_mesh_status(live: &mut Live, reply: Option<&Value>, hub_call: &crate::hops::CallResult, now_ms: u64) {
    live.polled_at_ms = now_ms;
    let hub_node = NodeLive { online: true, os: Some(os_label(std::env::consts::OS).into()), ..Default::default() };
    live.nodes.insert("cloud".into(), hub_node);
    let edge = |ok: bool, rtt: Option<u64>, s: Option<u64>, r: Option<u64>, t: Option<String>| EdgeLive {
        online: ok, rtt_ms: rtt, sent_bytes: s, recv_bytes: r, transport: t, at_ms: now_ms,
    };
    let Some(v) = reply.filter(|_| hub_call.ok) else {
        let err = hub_call.error.clone();
        for id in ["zentrale", "werkstatt", "hafenbuero", "aussendienst"] {
            live.nodes.insert(id.into(), NodeLive { online: false, error: err.clone(), ..Default::default() });
        }
        for c in ["cloud-zentrale", "zentrale-werkstatt", "zentrale-hafenbuero", "aussendienst-zentrale"] {
            live.edges.insert(c.into(), edge(false, None, None, None, None));
        }
        return;
    };
    live.nodes.insert("zentrale".into(), NodeLive {
        online: true,
        os: v.get("os").and_then(Value::as_str).map(|s| os_label(s).to_string()),
        arch: v.get("arch").and_then(Value::as_str).map(str::to_string),
        agent: v.get("agent").and_then(Value::as_str).map(str::to_string),
        rtt_ms: Some(hub_call.dur_ms),
        ..Default::default()
    });
    live.edges.insert("cloud-zentrale".into(), edge(true, Some(hub_call.dur_ms), hub_call.sent_bytes, hub_call.recv_bytes, hub_call.transport.map(str::to_string)));
    let peers = v.get("peers").cloned().unwrap_or(json!({}));
    for (id, chan) in [("werkstatt", "zentrale-werkstatt"), ("hafenbuero", "zentrale-hafenbuero")] {
        let p = &peers[id];
        let ok = p.get("online").and_then(Value::as_bool).unwrap_or(false);
        let info = p.get("info").filter(|i| i.is_object());
        live.nodes.insert(id.into(), NodeLive {
            online: ok,
            os: info.and_then(|i| i.get("os")).and_then(Value::as_str).map(|s| os_label(s).to_string()),
            arch: info.and_then(|i| i.get("arch")).and_then(Value::as_str).map(str::to_string),
            agent: info.and_then(|i| i.get("agent")).and_then(Value::as_str).map(str::to_string),
            rtt_ms: p.get("rtt_ms").and_then(Value::as_u64).filter(|_| ok),
            detail: info.and_then(|i| i.get("ollama")).cloned(),
            error: p.get("error").and_then(Value::as_str).map(str::to_string),
        });
        live.edges.insert(chan.into(), edge(ok, p.get("rtt_ms").and_then(Value::as_u64).filter(|_| ok),
            p.get("sent_bytes").and_then(Value::as_u64), p.get("recv_bytes").and_then(Value::as_u64),
            p.get("transport").and_then(Value::as_str).map(str::to_string)));
    }
    let ad = &peers["aussendienst"];
    let ad_ok = ad.get("online").and_then(Value::as_bool).unwrap_or(false);
    live.nodes.insert("aussendienst".into(), NodeLive { online: ad_ok, detail: ad.get("last_seen_s_ago").cloned(), ..Default::default() });
    live.edges.insert("aussendienst-zentrale".into(), edge(ad_ok, None, None, None, None));
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The JSON the dashboard (and the mobile apps) consume.
pub fn build_graph(cp: Result<&CpTopology, String>, manifest: &Manifest, live: &Live, fetched_at_ms: u64) -> Value {
    let (agents, cp_edges, source): (Vec<CpAgent>, Vec<CpEdge>, Value) = match cp {
        Ok(t) => (t.agents.clone(), t.edges.clone(), json!({"kind": "control-plane", "topology_id": t.id, "net_uuid": t.net_uuid, "overlay_mode": t.overlay_mode, "fetched_at_ms": fetched_at_ms})),
        Err(e) => (
            manifest.nodes.iter().map(|n| CpAgent { id: n.holder_pubkey.clone(), kind: "peer".into(), label: None, verified: false }).collect(),
            manifest.channels.iter().filter_map(|c| Some(CpEdge { a: manifest.node(&c.a)?.holder_pubkey.clone(), b: manifest.node(&c.b)?.holder_pubkey.clone(), channel: None })).collect(),
            json!({"kind": "manifest", "error": e, "fetched_at_ms": fetched_at_ms}),
        ),
    };
    let nodes: Vec<Value> = agents.iter().map(|a| {
        let def = manifest.node_by_holder(&a.id);
        let l = def.and_then(|d| live.nodes.get(&d.id));
        let planned = def.map(|d| d.planned).unwrap_or(false);
        let state = match (l, planned) {
            (Some(l), _) if l.online => "online",
            (_, true) => "planned",
            (Some(_), false) => "offline",
            (None, false) => "unknown",
        };
        let os = l.and_then(|l| l.os.clone()).or_else(|| def.and_then(|d| d.os_hint.clone()));
        json!({
            "id": a.id, "key": def.map(|d| d.id.clone()).unwrap_or_else(|| short(&a.id)),
            "label": {"de": def.map(|d| d.label_de.clone()).or(a.label.clone()).unwrap_or_else(|| short(&a.id)),
                      "en": def.map(|d| d.label_en.clone()).or(a.label.clone()).unwrap_or_else(|| short(&a.id))},
            "kind": a.kind, "role": def.map(|d| d.role.clone()).unwrap_or_else(|| "unknown".into()),
            "os": os, "state": state, "planned": planned,
            "rtt_ms": l.and_then(|l| l.rtt_ms), "agent": l.and_then(|l| l.agent.clone()),
            "detail": l.and_then(|l| l.detail.clone()), "error": l.and_then(|l| l.error.clone()),
            "verified": a.verified,
        })
    }).collect();
    let edges: Vec<Value> = cp_edges.iter().map(|e| {
        let (da, db) = (manifest.node_by_holder(&e.a), manifest.node_by_holder(&e.b));
        let chan = match (da, db) { (Some(x), Some(y)) => manifest.channel_between(&x.id, &y.id), _ => None };
        let l = chan.and_then(|c| live.edges.get(&c.name));
        json!({
            "a": e.a, "b": e.b, "channel": chan.map(|c| c.name.clone()),
            "channel_id": e.channel.clone().or_else(|| chan.map(|c| c.id.clone()).filter(|s| !s.is_empty())),
            "state": if l.map(|l| l.online).unwrap_or(false) { "connected" } else { "offline" },
            "rtt_ms": l.and_then(|l| l.rtt_ms), "sent_bytes": l.and_then(|l| l.sent_bytes),
            "recv_bytes": l.and_then(|l| l.recv_bytes), "transport": l.and_then(|l| l.transport.clone()),
        })
    }).collect();
    json!({"source": source, "nodes": nodes, "edges": edges, "polled_at_ms": live.polled_at_ms})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hops::CallResult;

    fn manifest() -> Manifest {
        let n = |id: &str, h: &str, role: &str, planned: bool| NodeDef {
            id: id.into(), label_de: id.to_uppercase(), label_en: id.to_uppercase(), holder_pubkey: h.into(),
            os_hint: Some("Linux".into()), role: role.into(), planned,
        };
        Manifest {
            nodes: vec![n("cloud", "h-cloud", "cloud", false), n("zentrale", "h-zen", "hub", false), n("werkstatt", "h-wer", "gpu", false),
                        n("hafenbuero", "h-haf", "site", false), n("aussendienst", "h-aus", "mobile", true)],
            channels: ["cloud-zentrale", "zentrale-werkstatt", "zentrale-hafenbuero", "aussendienst-zentrale"].iter().map(|c| {
                let (a, b) = c.split_once('-').unwrap();
                ChannelDef { name: c.to_string(), a: a.into(), b: b.into(), id: String::new() }
            }).collect(),
        }
    }

    fn cp() -> CpTopology {
        let a = |id: &str| CpAgent { id: id.into(), kind: "peer".into(), label: None, verified: true };
        let e = |x: &str, y: &str| CpEdge { a: x.into(), b: y.into(), channel: None };
        CpTopology { id: "t1".into(), net_uuid: "n".into(), overlay_mode: "baseline".into(),
            agents: vec![a("h-cloud"), a("h-zen"), a("h-wer"), a("h-haf"), a("h-aus"), a("h-stranger")],
            edges: vec![e("h-cloud", "h-zen"), e("h-zen", "h-wer"), e("h-zen", "h-haf"), e("h-aus", "h-zen")] }
    }

    fn hub_ok() -> CallResult { CallResult { ok: true, dur_ms: 120, sent_bytes: Some(10), recv_bytes: Some(20), transport: Some("quic"), ..Default::default() } }

    #[test]
    fn pick_prefers_configured_then_manifest_match() {
        let other = CpTopology { id: "t0".into(), net_uuid: "".into(), agents: vec![], edges: vec![], overlay_mode: "".into() };
        let views = vec![other, cp()];
        assert_eq!(pick_topology(&views, &manifest(), None).unwrap().id, "t1");
        assert_eq!(pick_topology(&views, &manifest(), Some("t0")).unwrap().id, "t0");
        assert!(pick_topology(&views, &manifest(), Some("nope")).is_none());
    }

    #[test]
    fn live_state_comes_only_from_replies() {
        let mut live = Live::default();
        let reply = json!({"os": "Darwin", "arch": "arm64", "agent": "ct-agent 0.7.41", "peers": {
            "werkstatt": {"online": true, "rtt_ms": 300, "sent_bytes": 5, "recv_bytes": 6, "info": {"os": "Linux", "ollama": {"ok": true}}},
            "hafenbuero": {"online": false, "rtt_ms": 25000, "error": "timeout"},
            "aussendienst": {"online": false, "last_seen_s_ago": null}}});
        apply_mesh_status(&mut live, Some(&reply), &hub_ok(), 1000);
        let g = build_graph(Ok(&cp()), &manifest(), &live, 1000);
        let n = |k: &str| g["nodes"].as_array().unwrap().iter().find(|n| n["key"] == k).unwrap().clone();
        assert_eq!(n("zentrale")["os"], "macOS");
        assert_eq!(n("werkstatt")["state"], "online");
        assert_eq!(n("hafenbuero")["state"], "offline");
        assert_eq!(n("aussendienst")["state"], "planned");
        assert_eq!(n("h-stranger".get(..8).unwrap())["state"], "unknown");
        let e = g["edges"].as_array().unwrap();
        assert_eq!(e[1]["state"], "connected");
        assert_eq!(e[1]["rtt_ms"], 300);
        assert_eq!(e[2]["state"], "offline");
        assert_eq!(g["source"]["kind"], "control-plane");
    }

    #[test]
    fn failed_hub_call_marks_everything_offline_not_stale_green() {
        let mut live = Live::default();
        apply_mesh_status(&mut live, None, &CallResult { error: Some("timeout".into()), ..Default::default() }, 5);
        let g = build_graph(Ok(&cp()), &manifest(), &live, 5);
        assert!(g["edges"].as_array().unwrap().iter().all(|e| e["state"] == "offline"));
        let z = g["nodes"].as_array().unwrap().iter().find(|n| n["key"] == "zentrale").unwrap().clone();
        assert_eq!(z["state"], "offline");
        assert_eq!(z["error"], "timeout");
    }

    #[test]
    fn control_plane_failure_falls_back_to_manifest_and_says_so() {
        let g = build_graph(Err("503".into()), &manifest(), &Live::default(), 1);
        assert_eq!(g["source"]["kind"], "manifest");
        assert_eq!(g["source"]["error"], "503");
        assert_eq!(g["nodes"].as_array().unwrap().len(), 5);
        assert_eq!(g["edges"].as_array().unwrap().len(), 4);
    }
}
