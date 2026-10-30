//! nordwind-hub: dashboard server of the Nordwind flagship demo. It is the "Cloud-Kern" node:
//! it holds its own channel identity (cloud -> zentrale), reads the topology from the control
//! plane, polls the mesh with real channel calls and runs the scenario.
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use nordwind::topology::{apply_mesh_status, build_graph, pick_topology, CpTopology, Live, Manifest};
use nordwind::{gate, hops};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, Mutex};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

const TOPIC: &str = "Törn-Briefing Samstag Bremen";
const KNOWN_NODES: [&str; 5] = ["cloud", "zentrale", "werkstatt", "hafenbuero", "aussendienst"];
const KNOWN_SERVICES: [&str; 5] = ["toern_briefing", "draft_text", "route_wetter", "ping", "mesh_status"];
const MAX_REPORTS_PER_RUN: usize = 20;
const POLL_EVERY: Duration = Duration::from_secs(30);
const CP_CACHE: Duration = Duration::from_secs(15);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).expect("urandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct ActiveRun {
    id: String,
    token: String,
    seen: HashSet<u64>,
    reports: usize,
}

struct Cfg {
    ct_agent_bin: String,
    env: HashMap<String, String>,
    cp_url: String,
    oidc_issuer: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    static_token: Option<String>,
    topology_id: Option<String>,
    public_url: String,
    lab: bool,
}

struct Hub {
    cfg: Cfg,
    manifest: Manifest,
    live: Mutex<Live>,
    cp_cache: Mutex<Option<(Instant, Result<CpTopology, String>)>>,
    token_cache: Mutex<Option<(Instant, String)>>,
    run: Mutex<Option<ActiveRun>>,
    last_run: Mutex<Vec<Value>>,
    last_start: Mutex<Option<Instant>>,
    tx: broadcast::Sender<String>,
}

type S = Arc<Hub>;

impl Hub {
    async fn emit(&self, ev: Value) {
        if ev["run"].is_string() && ev["type"] != "graph" {
            let mut l = self.last_run.lock().await;
            if ev["type"] == "run_start" {
                l.clear();
            }
            if l.len() < 80 {
                l.push(ev.clone());
            }
        }
        let _ = self.tx.send(ev.to_string());
    }

    /// Call a service through a real channel. Blocking work happens off the async runtime.
    async fn call(&self, channel: &str, service: &str, payload: Value, timeout: Duration) -> hops::CallResult {
        let env = match hops::initiator_env(channel, service, &self.cfg.env) {
            Ok(e) => e,
            Err(e) => return hops::CallResult { error: Some(e), ..Default::default() },
        };
        let bin = self.cfg.ct_agent_bin.clone();
        tokio::task::spawn_blocking(move || hops::run_call(&bin, &env, &payload, timeout))
            .await
            .unwrap_or_else(|e| hops::CallResult { error: Some(format!("join: {e}")), ..Default::default() })
    }

    async fn cp_token(&self) -> Result<String, String> {
        if let Some(t) = &self.cfg.static_token {
            return Ok(t.clone());
        }
        if let Some((at, t)) = &*self.token_cache.lock().await {
            if at.elapsed() < Duration::from_secs(120) {
                return Ok(t.clone());
            }
        }
        let (Some(iss), Some(id), Some(sec)) = (&self.cfg.oidc_issuer, &self.cfg.client_id, &self.cfg.client_secret) else {
            return Err("no control-plane credentials configured (NW_CP_CLIENT_ID/SECRET or NW_CP_TOKEN)".into());
        };
        let url = format!("{}/protocol/openid-connect/token", iss.trim_end_matches('/'));
        let (id, sec) = (id.clone(), sec.clone());
        let tok = tokio::task::spawn_blocking(move || {
            let r: Value = ureq::post(&url)
                .timeout(Duration::from_secs(8))
                .send_form(&[("grant_type", "client_credentials"), ("client_id", &id), ("client_secret", &sec)])
                .map_err(|e| format!("token endpoint: {e}"))?
                .into_json()
                .map_err(|e| format!("token json: {e}"))?;
            r["access_token"].as_str().map(str::to_string).ok_or("token endpoint returned no access_token".to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        *self.token_cache.lock().await = Some((Instant::now(), tok.clone()));
        Ok(tok)
    }

    async fn topology(&self) -> Result<CpTopology, String> {
        if let Some((at, r)) = &*self.cp_cache.lock().await {
            if at.elapsed() < CP_CACHE {
                return r.clone();
            }
        }
        let r = self.fetch_topology().await;
        *self.cp_cache.lock().await = Some((Instant::now(), r.clone()));
        r
    }

    async fn fetch_topology(&self) -> Result<CpTopology, String> {
        let token = self.cp_token().await?;
        let base = self.cfg.cp_url.trim_end_matches('/').to_string();
        let manifest = self.manifest.clone();
        let wanted = self.cfg.topology_id.clone();
        tokio::task::spawn_blocking(move || {
            let get = |path: &str| -> Result<Value, String> {
                ureq::get(&format!("{base}{path}"))
                    .set("Authorization", &format!("Bearer {token}"))
                    .timeout(Duration::from_secs(8))
                    .call()
                    .map_err(|e| format!("GET {path}: {e}"))?
                    .into_json()
                    .map_err(|e| format!("GET {path} json: {e}"))
            };
            let list = get("/me/topologies")?;
            let ids: Vec<String> = list.as_array().ok_or("topology list is not an array")?.iter().filter_map(|t| t["id"].as_str().map(str::to_string)).collect();
            if ids.is_empty() {
                return Err("account has no topology yet - run scripts/nordwind-seed-topology.sh".to_string());
            }
            let mut views = vec![];
            for id in ids.iter().take(10) {
                views.push(serde_json::from_value::<CpTopology>(get(&format!("/me/topologies/{id}"))?).map_err(|e| format!("topology json: {e}"))?);
            }
            pick_topology(&views, &manifest, wanted.as_deref()).cloned().ok_or("no topology in this account contains the Nordwind agents - run scripts/nordwind-seed-topology.sh".to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn graph(&self) -> Value {
        let t = self.topology().await;
        let live = self.live.lock().await.clone();
        build_graph(t.as_ref().map_err(|e| e.clone()), &self.manifest, &live, now_ms())
    }

    async fn poll(&self) {
        if self.run.lock().await.is_some() {
            return;
        }
        let r = self.call("cloud-zentrale", "mesh_status", json!({}), Duration::from_secs(60)).await;
        let reply: Option<Value> = r.output.as_deref().and_then(|o| serde_json::from_str(o).ok());
        apply_mesh_status(&mut *self.live.lock().await, reply.as_ref(), &r, now_ms());
        self.emit(json!({"type": "graph", "t": now_ms()})).await;
    }
}

// ---------------------------------------------------------------- gate + headers
async fn gate_mw(State(s): State<S>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let public = path == "/healthz" || path.starts_with("/node/hop/");
    if !public {
        let who = if s.cfg.lab {
            Some("lab@local".to_string())
        } else {
            gate::viewer(req.headers().get("x-gate-email").and_then(|v| v.to_str().ok()), false)
        };
        match who {
            Some(w) => {
                req.extensions_mut().insert(Viewer(w));
            }
            None => return (StatusCode::FORBIDDEN, Json(json!({"error": "login_required"}))).into_response(),
        }
    }
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

#[derive(Clone)]
struct Viewer(String);

// -------------------------------------------------------------------- handlers
async fn me(axum::Extension(Viewer(v)): axum::Extension<Viewer>, State(s): State<S>) -> Json<Value> {
    Json(json!({"email": v, "lab": s.cfg.lab, "topic": TOPIC}))
}

async fn api_state(axum::Extension(Viewer(v)): axum::Extension<Viewer>, State(s): State<S>) -> Json<Value> {
    let graph = s.graph().await;
    let last = s.last_run.lock().await.clone();
    Json(json!({"me": {"email": v, "lab": s.cfg.lab}, "topic": TOPIC, "graph": graph, "last_run": last, "running": s.run.lock().await.is_some()}))
}

async fn api_topology(State(s): State<S>) -> Json<Value> {
    Json(s.graph().await)
}

async fn api_events(State(s): State<S>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = s.tx.subscribe();
    Sse::new(BroadcastStream::new(rx).filter_map(|m| m.ok().map(|l| Ok(Event::default().data(l))))).keep_alive(KeepAlive::default())
}

async fn api_scenario(State(s): State<S>) -> Response {
    {
        let mut ls = s.last_start.lock().await;
        if ls.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error": "too_fast"}))).into_response();
        }
        *ls = Some(Instant::now());
    }
    let mut run = s.run.lock().await;
    if run.is_some() {
        return (StatusCode::CONFLICT, Json(json!({"error": "already_running"}))).into_response();
    }
    let id = rand_hex(6);
    let token = rand_hex(16);
    *run = Some(ActiveRun { id: id.clone(), token: token.clone(), seen: HashSet::new(), reports: 0 });
    drop(run);
    let s2 = s.clone();
    let id2 = id.clone();
    tokio::spawn(async move { run_scenario(s2, id2, token).await });
    (StatusCode::ACCEPTED, Json(json!({"run": id}))).into_response()
}

fn hop_event(run: &str, h: &Value, source: &str) -> Value {
    let mut e = json!({"type": "hop", "run": run, "source": source, "t": now_ms(), "edge_sees": "ciphertext"});
    for k in ["seq", "from", "to", "channel", "service", "state", "t0_ms", "dur_ms", "sent_bytes", "recv_bytes", "plain_out", "plain_in", "transport", "error"] {
        if !h[k].is_null() {
            e[k] = h[k].clone();
        }
    }
    e
}

async fn run_scenario(s: S, run: String, token: String) {
    let t0 = Instant::now();
    s.emit(json!({"type": "run_start", "run": run, "topic": TOPIC, "t": now_ms()})).await;
    let hop1 = |state: &str| json!({"seq": 1, "from": "cloud", "to": "zentrale", "channel": "cloud-zentrale", "service": "toern_briefing", "state": state});
    s.emit(hop_event(&run, &hop1("start"), "hub")).await;
    let payload = json!({"topic": TOPIC, "run": run, "report": format!("{}/node/hop/{}", s.cfg.public_url.trim_end_matches('/'), run), "token": token});
    let r = s.call("cloud-zentrale", "toern_briefing", payload, Duration::from_secs(170)).await;
    let reply: Option<Value> = r.output.as_deref().and_then(|o| serde_json::from_str(o).ok());
    let mut h1 = hop1(if r.ok { "ok" } else { "error" });
    h1["dur_ms"] = json!(r.dur_ms);
    h1["sent_bytes"] = json!(r.sent_bytes);
    h1["recv_bytes"] = json!(r.recv_bytes);
    h1["plain_out"] = json!(r.plain_out);
    h1["plain_in"] = json!(r.plain_in);
    h1["transport"] = json!(r.transport);
    h1["error"] = json!(r.error);
    s.emit(hop_event(&run, &h1, "hub")).await;
    // Hops the node could not report live (telemetry blocked/unreachable) come from the
    // authoritative trace inside the encrypted reply, marked source=trace.
    let seen = s.run.lock().await.as_ref().map(|a| a.seen.clone()).unwrap_or_default();
    if let Some(trace) = reply.as_ref().and_then(|v| v["hops"].as_array()) {
        for h in trace {
            let seq = h["seq"].as_u64().unwrap_or(0);
            if !seen.contains(&seq) {
                s.emit(hop_event(&run, h, "trace")).await;
            }
        }
    }
    *s.run.lock().await = None;
    let total_ms = t0.elapsed().as_millis() as u64;
    match reply {
        Some(v) if r.ok && v["ok"] == true => {
            s.emit(json!({"type": "run_done", "run": run, "total_ms": total_ms, "briefing": v["briefing"], "telemetry": v["telemetry"], "t": now_ms()})).await
        }
        Some(v) if r.ok => s.emit(json!({"type": "run_error", "run": run, "error": v["error"], "total_ms": total_ms, "t": now_ms()})).await,
        _ => s.emit(json!({"type": "run_error", "run": run, "error": r.error.clone().unwrap_or_else(|| "no reply".into()), "total_ms": total_ms, "t": now_ms()})).await,
    }
    s.poll().await;
}

/// Capability endpoint for live hop reports from Zentrale. Not behind the login gate (the
/// node has no browser session): authorised by the per-run random token the hub put into the
/// (end-to-end encrypted) request; valid only while that run is active; strictly whitelisted.
async fn node_hop(State(s): State<S>, Path(run): Path<String>, headers: HeaderMap, body: axum::body::Bytes) -> StatusCode {
    if body.len() > 4096 {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let tok = headers.get("x-run-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    let Ok(h) = serde_json::from_slice::<Value>(&body) else { return StatusCode::BAD_REQUEST };
    let mut guard = s.run.lock().await;
    let Some(active) = guard.as_mut().filter(|a| a.id == run && gate::tokens_equal(&a.token, tok)) else { return StatusCode::UNAUTHORIZED };
    let ok_node = |k: &str| h[k].as_str().is_some_and(|v| KNOWN_NODES.contains(&v));
    let state_ok = matches!(h["state"].as_str(), Some("start" | "ok" | "error"));
    let seq = h["seq"].as_u64().unwrap_or(0);
    if active.reports >= MAX_REPORTS_PER_RUN || !ok_node("from") || !ok_node("to") || !state_ok || !(2..=9).contains(&seq)
        || !h["service"].as_str().is_some_and(|v| KNOWN_SERVICES.contains(&v))
    {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }
    active.reports += 1;
    if h["state"] != "start" {
        active.seen.insert(seq);
    }
    let mut ev = hop_event(&run, &h, "node-report");
    if let Some(e) = ev["error"].as_str() {
        ev["error"] = json!(e.chars().take(200).collect::<String>());
    }
    drop(guard);
    s.emit(ev).await;
    StatusCode::NO_CONTENT
}

async fn healthz() -> &'static str {
    "ok"
}

async fn asset(body: &'static str, ct: &'static str) -> Response {
    ([(header::CONTENT_TYPE, ct)], body).into_response()
}

fn load_cfg() -> Result<(Cfg, Manifest), String> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let g = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let manifest_path = g("NW_NODES_FILE").unwrap_or_else(|| "/etc/nordwind/nodes.json".into());
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&manifest_path).map_err(|e| format!("{manifest_path}: {e}"))?)
        .map_err(|e| format!("{manifest_path}: {e}"))?;
    let lab = g("NW_LAB").as_deref() == Some("1");
    let mut env = env;
    if !lab && g("CT_CHANNEL_FRONT_DOOR").is_some() && g("CT_CHANNEL_FRONT_DOOR_CERT").is_none() {
        let cp = g("NW_CP_URL").ok_or("NW_CP_URL required")?;
        let mut der = Vec::new();
        std::io::Read::read_to_end(&mut ureq::get(&format!("{}/pki/ca", cp.trim_end_matches('/'))).call().map_err(|e| format!("/pki/ca: {e}"))?.into_reader(), &mut der).map_err(|e| e.to_string())?;
        env.insert("CT_CHANNEL_FRONT_DOOR_CERT".into(), der.iter().map(|b| format!("{b:02x}")).collect());
    }
    Ok((Cfg {
        ct_agent_bin: g("CT_AGENT_BIN").unwrap_or_else(|| "ct-agent".into()),
        cp_url: g("NW_CP_URL").unwrap_or_default(),
        oidc_issuer: g("NW_OIDC_ISSUER"),
        client_id: g("NW_CP_CLIENT_ID"),
        client_secret: g("NW_CP_CLIENT_SECRET"),
        static_token: g("NW_CP_TOKEN"),
        topology_id: g("NW_TOPOLOGY_ID"),
        public_url: g("NW_PUBLIC_URL").unwrap_or_else(|| "https://a2a-demo.bunsenbrenner.org".into()),
        lab,
        env,
    }, manifest))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (cfg, manifest) = load_cfg().map_err(|e| format!("startup: {e}"))?;
    if cfg.lab {
        eprintln!("nordwind-hub: NW_LAB=1 -- LOGIN GATE DISABLED, local lab only, never set this in production");
    }
    let (tx, _) = broadcast::channel(256);
    let hub: S = Arc::new(Hub {
        cfg, manifest, live: Mutex::new(Live::default()), cp_cache: Mutex::new(None), token_cache: Mutex::new(None),
        run: Mutex::new(None), last_run: Mutex::new(vec![]), last_start: Mutex::new(None), tx,
    });
    let poller = hub.clone();
    tokio::spawn(async move {
        loop {
            poller.poll().await;
            tokio::time::sleep(POLL_EVERY).await;
        }
    });
    let app = Router::new()
        .route("/", get(|| asset(include_str!("../../web/nordwind.html"), "text/html; charset=utf-8")))
        .route("/app.js", get(|| asset(include_str!("../../web/nordwind.js"), "text/javascript; charset=utf-8")))
        .route("/app.css", get(|| asset(include_str!("../../web/nordwind.css"), "text/css; charset=utf-8")))
        .route("/healthz", get(healthz))
        .route("/api/me", get(me))
        .route("/api/state", get(api_state))
        .route("/api/topology", get(api_topology))
        .route("/api/events", get(api_events))
        .route("/api/scenario", post(api_scenario))
        .route("/node/hop/:run", post(node_hop))
        .layer(middleware::from_fn_with_state(hub.clone(), gate_mw))
        .with_state(hub);
    let addr = std::env::var("NW_LISTEN").unwrap_or_else(|_| "0.0.0.0:8791".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("nordwind-hub: serving on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
