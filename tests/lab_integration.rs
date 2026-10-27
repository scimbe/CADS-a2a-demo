//! Integration test with REAL processes: three `ct-agent channel` acceptors (Zentrale,
//! Werkstatt, Hafenbuero) running the real node handler `nordwind_node.py`, called through the
//! hub's own call plumbing (`nordwind::hops`) exactly as in production -- only the transport
//! is direct-address (no edge) and the two external data sources are local stubs
//! (`nordwind/node/lab_stubs.py`, test-only).
//!
//! Skipped (loudly) when no ct-agent binary or python3 is available:
//!   CT_AGENT_BIN=/path/to/ct-agent cargo test --test lab_integration -- --nocapture
use nordwind::hops::{initiator_env, run_call};
use nordwind::topology::{apply_mesh_status, Live};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn agent_bin() -> Option<String> {
    let cands = [std::env::var("CT_AGENT_BIN").ok(), Some("../ct-agent/target/release/ct-agent".to_string()), Some("ct-agent".to_string())];
    cands.into_iter().flatten().find(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_udp() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}
fn free_tcp() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn val(text: &str, key: &str) -> String {
    text.lines().find_map(|l| l.split_once(&format!("{key}=")).map(|(_, v)| v.trim().to_string())).unwrap_or_else(|| panic!("{key} missing in:\n{text}"))
}

struct Ident {
    noise_key: String,
    noise_pub: String,
}
fn ident(bin: &str) -> Ident {
    let out = Command::new(bin).args(["channel", "init"]).output().unwrap();
    let t = String::from_utf8_lossy(&out.stdout).to_string();
    Ident { noise_key: val(&t, "CT_CHANNEL_NOISE_KEY"), noise_pub: val(&t, "CT_CHANNEL_NOISE_PUBKEY") }
}

/// Start a (restarting) real acceptor via the lab supervisor; returns (process, cert FILE path).
fn acceptor(bin: &str, node: &str, ch: &str, port: u16, me: &Ident, peer: &Ident, extra: &[(String, String)], state: &PathBuf) -> (Proc, String) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("nordwind/node");
    let services = match node {
        "zentrale" => "toern_briefing,mesh_status,ping",
        "werkstatt" => "draft_text,ping",
        _ => "route_wetter,ping",
    };
    let mut cfg = serde_json::Map::new();
    let mut put = |k: &str, v: String| {
        cfg.insert(k.to_string(), Value::String(v));
    };
    put("CT_AGENT_STATE_DIR", state.join(format!("st-{node}-{ch}")).display().to_string());
    put("CT_CHANNEL_ROLE", "accept".into());
    put("CT_CHANNEL_SERVE", "1".into());
    put("CT_CHANNEL_ADDR", format!("127.0.0.1:{port}"));
    put("CT_CHANNEL_NOISE_KEY", me.noise_key.clone());
    put("CT_CHANNEL_PEER_NOISE_KEY", peer.noise_pub.clone());
    put("CT_AGENT_SERVICES", services.into());
    put("CT_AGENT_SERVICE_HANDLER_CMD", format!("python3 {}", root.join("nordwind_node.py").display()));
    put("CT_AGENT_BIN", bin.into());
    put("NORDWIND_NODE", node.into());
    put("NORDWIND_SERVING_CHANNEL", ch.into());
    put("NORDWIND_STATE_DIR", state.display().to_string());
    for (k, v) in extra {
        put(k, v.clone());
    }
    let cert = state.join(format!("{node}-{ch}.cert"));
    let child = Command::new("python3").arg(root.join("lab_acceptor.py")).arg(&cert)
        .env("LAB_ENV", Value::Object(cfg).to_string()).stderr(Stdio::null()).stdout(Stdio::null()).spawn().unwrap();
    for _ in 0..300 {
        if cert.exists() {
            return (Proc(child), cert.display().to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{node}: acceptor did not publish a certificate");
}

fn direct(prefix: &str, port: u16, peer: &Ident, cert: &str) -> Vec<(String, String)> {
    vec![
        (format!("{prefix}DIRECT_ADDR"), format!("127.0.0.1:{port}")),
        (format!("{prefix}PEER_NOISE_KEY"), peer.noise_pub.clone()),
        (format!("{prefix}PEER_CERT_FILE"), cert.to_string()),
    ]
}

#[test]
fn three_real_agents_run_the_whole_scenario() {
    let Some(bin) = agent_bin() else {
        eprintln!("SKIPPED: no ct-agent binary (set CT_AGENT_BIN)");
        return;
    };
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("SKIPPED: python3 missing");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("nw-lab-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let (cloud, zen, wer, haf) = (ident(&bin), ident(&bin), ident(&bin), ident(&bin));

    let (p_oll, p_wx) = (free_tcp(), free_tcp());
    let stubs = Command::new("python3").arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("nordwind/node/lab_stubs.py")).args([p_oll.to_string(), p_wx.to_string()])
        .stdout(Stdio::piped()).spawn().unwrap();
    let mut stubs = Proc(stubs);
    let mut line = String::new();
    BufReader::new(stubs.0.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains("stubs-ready"));

    let (pw, ph, pz) = (free_udp(), free_udp(), free_udp());
    let (_w, cw) = acceptor(&bin, "werkstatt", "zentrale-werkstatt", pw, &wer, &zen,
        &[("NORDWIND_OLLAMA_URL".into(), format!("http://127.0.0.1:{p_oll}"))], &tmp);
    let (_h, ch) = acceptor(&bin, "hafenbuero", "zentrale-hafenbuero", ph, &haf, &zen,
        &[("NORDWIND_WEATHER_URL".into(), format!("http://127.0.0.1:{p_wx}/v1/forecast"))], &tmp);
    // Zentrale knows how to reach its two neighbours (direct addresses), as initiator.
    let mut zextra = direct("NW_CH_ZENTRALE_WERKSTATT_", pw, &wer, &cw);
    zextra.extend(direct("NW_CH_ZENTRALE_HAFENBUERO_", ph, &haf, &ch));
    zextra.push(("NW_NOISE_KEY".into(), zen.noise_key.clone()));
    let (_z, cz) = acceptor(&bin, "zentrale", "cloud-zentrale", pz, &zen, &cloud, &zextra, &tmp);

    // The hub side: its own env, same plumbing as production.
    let mut base: HashMap<String, String> = std::env::vars().filter(|(k, _)| k == "PATH" || k == "HOME").collect();
    base.insert("CT_AGENT_STATE_DIR".into(), tmp.join("hub").display().to_string());
    base.insert("NW_NOISE_KEY".into(), cloud.noise_key.clone());
    for (k, v) in direct("NW_CH_CLOUD_ZENTRALE_", pz, &zen, &cz) {
        base.insert(k, v);
    }
    let call = |svc: &str, payload: Value, secs: u64| {
        let env = initiator_env("cloud-zentrale", svc, &base).unwrap();
        run_call(&bin, &env, &payload, Duration::from_secs(secs))
    };

    // 1) mesh status: Zentrale really pings both neighbours.
    let r = call("mesh_status", json!({}), 60);
    assert!(r.ok, "mesh_status failed: {:?}", r.error);
    let reply: Value = serde_json::from_str(r.output.as_deref().unwrap()).unwrap();
    assert_eq!(reply["peers"]["werkstatt"]["online"], true, "{reply}");
    assert_eq!(reply["peers"]["hafenbuero"]["online"], true, "{reply}");
    assert!(reply["peers"]["werkstatt"]["sent_bytes"].as_u64().unwrap() > 0, "ciphertext bytes must be measured");
    let mut live = Live::default();
    apply_mesh_status(&mut live, Some(&reply), &r, 1);
    assert!(live.nodes["werkstatt"].online && live.edges["zentrale-hafenbuero"].online);

    // 2) the scenario end to end.
    let r = call("toern_briefing", json!({"topic": "Törn-Briefing Samstag Bremen", "run": "t1"}), 120);
    assert!(r.ok, "briefing call failed: {:?}", r.error);
    assert!(r.sent_bytes.unwrap() > 0 && r.recv_bytes.unwrap() > 0);
    let v: Value = serde_json::from_str(r.output.as_deref().unwrap()).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    let hops = v["hops"].as_array().unwrap();
    assert_eq!(hops.len(), 2);
    assert_eq!((hops[0]["to"].as_str(), hops[1]["to"].as_str()), (Some("werkstatt"), Some("hafenbuero")));
    for h in hops {
        assert_eq!(h["state"], "ok");
        assert!(h["sent_bytes"].as_u64().unwrap() > 0 && h["recv_bytes"].as_u64().unwrap() > 0 && h["dur_ms"].as_u64().unwrap() > 0, "{h}");
    }
    assert!(v["briefing"]["text"].as_str().unwrap().contains("Test-Stub"));
    assert_eq!(v["briefing"]["wetter"]["wind_kn_max"], 11.5);
    assert_eq!(v["briefing"]["route"]["gesamt_km"], 88);
    eprintln!("scenario ok in {} ms; hop1 {}ms hop2 {}ms", v["total_ms"], hops[0]["dur_ms"], hops[1]["dur_ms"]);

    // 3) a dead LLM must fail the hop, not produce made-up text.
    drop(stubs);
    let r = call("toern_briefing", json!({"run": "t2"}), 120);
    assert!(r.ok);
    let v: Value = serde_json::from_str(r.output.as_deref().unwrap()).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("Werkstatt"), "{v}");
    assert_eq!(v["hops"][0]["state"], "error");
    let _ = std::fs::remove_dir_all(&tmp);
}
