//! One real channel call = one fresh `ct-agent channel` initiator process.
//! Everything reported per hop (latency, ciphertext bytes, transport) is measured from that
//! process; nothing is estimated.
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallResult {
    pub ok: bool,
    pub output: Option<String>,
    pub dur_ms: u64,
    pub sent_bytes: Option<u64>,
    pub recv_bytes: Option<u64>,
    pub transport: Option<&'static str>,
    pub error: Option<String>,
    pub plain_out: usize,
    pub plain_in: usize,
}

/// Last `status uptime=Ns sent=XB recv=YB` line -> (sent, recv). Ciphertext bytes counted by
/// ct-agent below the Noise layer, per process (so per hop for a fresh initiator).
pub fn parse_status(stderr: &str) -> (Option<u64>, Option<u64>) {
    let mut out = (None, None);
    for l in stderr.lines() {
        if let Some(i) = l.find("status uptime=") {
            let rest = &l[i..];
            let num = |key: &str| -> Option<u64> {
                let j = rest.find(key)? + key.len();
                rest[j..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
            };
            if let (Some(s), Some(r)) = (num("sent="), num("recv=")) {
                out = (Some(s), Some(r));
            }
        }
    }
    out
}

pub fn parse_transport(stderr: &str) -> Option<&'static str> {
    if stderr.contains("relay leg via") {
        if stderr.contains("front door") {
            return Some("tcp_443");
        }
        if stderr.contains("QUIC") {
            return Some("quic");
        }
    }
    if stderr.contains("connected to") && stderr.contains("(initiator)") {
        return Some("direct");
    }
    None
}

/// Persistent call mode prints `{"ok":true,"output":"..."}` per call.
pub fn unwrap_envelope(line: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(line).map_err(|_| "unparseable reply envelope".to_string())?;
    match (v.get("ok").and_then(Value::as_bool), v.get("output").and_then(Value::as_str)) {
        (Some(true), Some(o)) => Ok(o.to_string()),
        _ => {
            let e = v.get("error").map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string()));
            Err(format!("service error: {}", e.unwrap_or_default()).chars().take(300).collect())
        }
    }
}

pub fn channel_prefix(channel: &str) -> String {
    format!("NW_CH_{}_", channel.to_uppercase().replace('-', "_"))
}

/// Lab mode only: wait for the cert a restarting direct-address acceptor published.
fn read_cert_file(path: &str) -> Result<String, String> {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if let Ok(v) = std::fs::read_to_string(path) {
            if !v.trim().is_empty() {
                return Ok(v.trim().to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("no peer certificate published at {path}"))
}

/// Environment for a `ct-agent channel` initiator on `channel` (same convention as
/// `nordwind/node/nordwind_node.py::initiator_env`). Lab mode = `..._DIRECT_ADDR` set.
pub fn initiator_env(channel: &str, service: &str, base: &HashMap<String, String>) -> Result<HashMap<String, String>, String> {
    let mut env = base.clone();
    // The control-plane credentials are the hub's, not the child's.
    env.retain(|k, _| !k.starts_with("NW_CP_CLIENT") && k != "NW_CP_TOKEN" && !k.starts_with("CT_OIDC"));
    let p = channel_prefix(channel);
    let get = |k: &str| base.get(k).filter(|v| !v.is_empty()).cloned();
    let noise = get(&format!("{p}NOISE_KEY")).or_else(|| get("NW_NOISE_KEY")).ok_or(format!("no Noise key for channel {channel}"))?;
    env.insert("CT_CHANNEL_ROLE".into(), "initiate".into());
    env.insert("CT_CHANNEL_CALL_SERVICE".into(), service.into());
    env.insert("CT_CHANNEL_NOISE_KEY".into(), noise);
    env.remove("CT_CHANNEL_SERVE");
    if let Some(addr) = get(&format!("{p}DIRECT_ADDR")) {
        env.insert("CT_CHANNEL_ADDR".into(), addr);
        env.insert("CT_CHANNEL_PEER_NOISE_KEY".into(), get(&format!("{p}PEER_NOISE_KEY")).unwrap_or_default());
        let cert = match (get(&format!("{p}PEER_CERT")), get(&format!("{p}PEER_CERT_FILE"))) {
            (Some(c), _) => c,
            (None, Some(f)) => read_cert_file(&f)?,
            _ => String::new(),
        };
        env.insert("CT_CHANNEL_PEER_CERT".into(), cert);
    } else {
        let holder = get(&format!("{p}HOLDER_KEY")).or_else(|| get("NW_HOLDER_KEY"));
        let grant = get(&format!("{p}GRANT"));
        let (Some(h), Some(g)) = (holder, grant) else {
            return Err(format!("channel {channel} not provisioned ({p}GRANT / holder key missing)"));
        };
        env.insert("CT_CHANNEL_HOLDER_KEY".into(), h);
        env.insert("CT_CHANNEL_GRANT".into(), g);
        env.insert("CT_CHANNEL_RELAY_ONLY".into(), "1".into());
        env.entry("CT_CHANNEL_DIRECT_UPGRADE".into()).or_insert_with(|| "1".into());
    }
    Ok(env)
}

/// Blocking; call from `spawn_blocking`.
pub fn run_call(bin: &str, env: &HashMap<String, String>, payload: &Value, timeout: Duration) -> CallResult {
    let line = payload.to_string();
    let mut res = CallResult { plain_out: line.len(), ..Default::default() };
    let t0 = Instant::now();
    let mut child = match Command::new(bin)
        .arg("channel")
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            res.error = Some(format!("spawning {bin}: {e}"));
            return res;
        }
    };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(line.as_bytes());
        let _ = si.write_all(b"\n");
    }
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let h_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut so, &mut s);
        s
    });
    let h_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut se, &mut s);
        s
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if t0.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    res.dur_ms = t0.elapsed().as_millis() as u64;
    let out = h_out.join().unwrap_or_default();
    let err = h_err.join().unwrap_or_default();
    (res.sent_bytes, res.recv_bytes) = parse_status(&err);
    res.transport = parse_transport(&err);
    let Some(status) = status else {
        res.error = Some(format!("timeout after {}s", timeout.as_secs()));
        return res;
    };
    let reply = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    if status.success() && !reply.is_empty() {
        match unwrap_envelope(&reply) {
            Ok(o) => {
                res.plain_in = o.len();
                res.output = Some(o);
                res.ok = true;
            }
            Err(e) => res.error = Some(e),
        }
    } else {
        let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).rev().take(2).collect();
        res.error = Some(format!("exit {:?}: {}", status.code(), tail.into_iter().rev().collect::<Vec<_>>().join(" | ")).chars().take(300).collect());
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> HashMap<String, String> {
        [("NW_NOISE_KEY", "aa"), ("NW_HOLDER_KEY", "bb"), ("NW_CH_CLOUD_ZENTRALE_GRANT", "gg")]
            .iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn status_last_line_wins() {
        let e = "ct-agent channel: status uptime=0s sent=98B recv=50B\nx\nct-agent channel: status uptime=1s sent=224B recv=304B\n";
        assert_eq!(parse_status(e), (Some(224), Some(304)));
        assert_eq!(parse_status("nothing"), (None, None));
    }

    #[test]
    fn transports() {
        assert_eq!(parse_transport("ct-agent channel: relay leg via QUIC (x) (#106)"), Some("quic"));
        assert_eq!(parse_transport("relay leg via the :443 front door"), Some("tcp_443"));
        assert_eq!(parse_transport("ct-agent channel: connected to 127.0.0.1:1 (initiator)"), Some("direct"));
        assert_eq!(parse_transport(""), None);
    }

    #[test]
    fn envelope() {
        assert_eq!(unwrap_envelope(r#"{"ok":true,"output":"hi"}"#).unwrap(), "hi");
        assert!(unwrap_envelope(r#"{"ok":false,"error":"boom"}"#).unwrap_err().contains("boom"));
        assert!(unwrap_envelope("zzz").is_err());
    }

    #[test]
    fn plane_env_requires_provisioning() {
        let e = initiator_env("cloud-zentrale", "ping", &base()).unwrap();
        assert_eq!(e["CT_CHANNEL_GRANT"], "gg");
        assert_eq!(e["CT_CHANNEL_RELAY_ONLY"], "1");
        assert!(initiator_env("zentrale-werkstatt", "ping", &base()).unwrap_err().contains("not provisioned"));
    }

    #[test]
    fn lab_env_uses_direct_address() {
        let mut b = base();
        b.insert("NW_CH_CLOUD_ZENTRALE_DIRECT_ADDR".into(), "127.0.0.1:9".into());
        let e = initiator_env("cloud-zentrale", "ping", &b).unwrap();
        assert_eq!(e["CT_CHANNEL_ADDR"], "127.0.0.1:9");
        assert!(!e.contains_key("CT_CHANNEL_GRANT"));
    }

    #[test]
    fn missing_binary_is_an_error_not_a_panic() {
        let r = run_call("/nonexistent/ct-agent", &HashMap::new(), &serde_json::json!({}), Duration::from_secs(1));
        assert!(!r.ok && r.error.unwrap().contains("spawning"));
    }
}
