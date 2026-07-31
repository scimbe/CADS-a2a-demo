//! CADS-a2a-demo bridge: a live web dashboard over a REAL Agent-Fabric channel call
//! between two genuinely separate containers -- `agent-alice` (this repo's
//! `a2a-demo-alice` service, a persistent broker-mediated `ct-agent channel --serve`
//! process) and `agent-bob` (spawned by THIS process, per visitor request, as its own
//! `ct-agent channel` initiator). Neither shares a filesystem or process tree with the
//! other; both are real, operator-admitted members of a real channel registered with
//! the control plane (`POST /me/channels`), reached only through the production edge's
//! broker/relay (`CT_CHANNEL_BROKER`/`CT_CHANNEL_RELAY`) -- the exact broker-mediated
//! mechanism docs.bunsenbrenner.org's `_how-to/join-a-channel.md` and
//! `_how-to/serve-a-channel-service.md` describe, click-tested live against this
//! production edge while building this rework (see README). This bridge is just what
//! clicks the buttons a human would: it dials agent-bob's own process with the
//! visitor's message on stdin and streams every real step over SSE. No fixture, no
//! simulated network, no identity minted at request time -- bob's identity is fixed and
//! was registered with the control plane once, ahead of time (see `provision.md`).

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

const SERVICE: &str = "text_generation";
const MAX_MESSAGE_LEN: usize = 400;
/// How long to wait for agent-bob's real reply over the broker-mediated channel --
/// generous (broker rendezvous + a real Noise handshake through the edge relay takes
/// longer than the direct-address path this demo used before), but bounded so a
/// genuinely stuck admission fails loudly instead of hanging the dashboard forever.
const INITIATOR_TIMEOUT: Duration = Duration::from_secs(25);

fn ct_agent_bin() -> String {
    std::env::var("CT_AGENT_BIN").unwrap_or_else(|_| "ct-agent".to_string())
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} must be set (see README/provision.md)"))
}

/// Agent-bob's fixed, real, pre-registered channel identity -- a real operator-signed
/// grant for a real holder key registered with the control plane via `POST
/// /me/channels/:channel/members`, never minted fresh per process or per request.
/// Loaded once at startup, not re-derived: unlike the identity this bridge used to mint
/// itself, these values MUST match what was actually registered, so generating a fresh
/// keypair here would just produce a holder the edge doesn't recognize.
struct BobIdentity {
    holder_key_hex: String,
    noise_key_hex: String,
    grant_hex: String,
    holder_pubkey_hex: String,
    channel_id_hex: String,
    broker_addr: String,
    relay_addr: String,
    /// TCP-:443 handover rung (the `Ladder` fallback: QUIC-relay -> TCP-over-front-door)
    /// -- both fields required together, mirrors agent-alice's alice-entrypoint.sh.
    front_door_addr: String,
    front_door_cert_hex: String,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Fetches the real Mesh-Plane CA root from the control plane's own published
/// `GET /pki/ca` -- the same trust anchor
/// `crates/edge/src/pki.rs::build_channel_front_door_acceptor` issues the :443 channel
/// front-door's leaf cert from (confirmed by reading both sides directly). Reached over
/// the compose-internal network (plain HTTP, never TLS here), once at startup -- not
/// re-fetched per round.
fn fetch_front_door_cert_hex(cp_url: &str) -> Result<String, String> {
    let url = format!("{}/pki/ca", cp_url.trim_end_matches('/'));
    let resp = ureq::get(&url)
        .call()
        .map_err(|e| format!("fetching {url}: {e}"))?;
    let mut der = Vec::new();
    resp.into_reader()
        .read_to_end(&mut der)
        .map_err(|e| format!("reading {url} response: {e}"))?;
    if der.is_empty() {
        return Err(format!("empty /pki/ca response from {cp_url}, refusing to start without a front-door trust anchor"));
    }
    Ok(hex_encode(&der))
}

impl BobIdentity {
    fn from_env() -> Result<Self, String> {
        let cp_url = required_env("A2A_AGENT_CP_URL")?;
        let front_door_cert_hex = fetch_front_door_cert_hex(&cp_url)?;
        Ok(Self {
            holder_key_hex: required_env("A2A_BOB_HOLDER_KEY")?,
            noise_key_hex: required_env("A2A_BOB_NOISE_KEY")?,
            grant_hex: required_env("A2A_BOB_GRANT")?,
            holder_pubkey_hex: required_env("A2A_BOB_HOLDER_PUBKEY")?,
            channel_id_hex: required_env("A2A_CHANNEL_ID")?,
            broker_addr: required_env("A2A_CHANNEL_BROKER")?,
            relay_addr: required_env("A2A_CHANNEL_RELAY")?,
            front_door_addr: required_env("A2A_CHANNEL_FRONT_DOOR")?,
            front_door_cert_hex,
        })
    }
}

struct BridgeState {
    bob: BobIdentity,
    round: AtomicU32,
    busy: Arc<Mutex<()>>,
    tx: broadcast::Sender<String>,
}

async fn broadcast_event(tx: &broadcast::Sender<String>, ev: Value) {
    let _ = tx.send(ev.to_string());
}

fn is_log_line(l: &str) -> bool {
    l.starts_with("ct-agent channel:")
}

/// Which relay-ladder rung actually carried this session, parsed from ct-agent's own
/// `"ct-agent channel: relay leg via ..."` status lines (added upstream specifically for
/// this dashboard, scimbe/ct-agent#106). `None` for every other line -- this is never
/// "direct": both channel members are relay-only by design, so it only distinguishes
/// the QUIC relay port from the `:443` front door.
fn relay_transport_from_log_line(l: &str) -> Option<&'static str> {
    if !is_log_line(l) || !l.contains("relay leg via") {
        return None;
    }
    if l.contains("front door") {
        Some("tcp_443")
    } else if l.contains("QUIC") {
        Some("quic")
    } else {
        None
    }
}

async fn run_round(st: Arc<BridgeState>, message: String) {
    let tx = st.tx.clone();
    let round = st.round.fetch_add(1, Ordering::SeqCst) + 1;

    broadcast_event(&tx, json!({"type": "round_start", "round": round, "message": message})).await;

    // --- dial agent-bob's own process: broker-mediated, through the real edge --------
    // This is a genuinely separate process from agent-alice's container -- it reaches
    // the service only via CT_CHANNEL_BROKER/CT_CHANNEL_RELAY (the production edge),
    // never a local address. `CT_CHANNEL_CALL_SERVICE` + stdin is exactly the pattern
    // docs.bunsenbrenner.org's serve-a-channel-service.md step 4 walks through by hand.
    broadcast_event(&tx, json!({"type": "initiator_dialing", "broker": st.bob.broker_addr})).await;
    let mut initiator = match Command::new(ct_agent_bin())
        .arg("channel")
        .env("CT_CHANNEL_ROLE", "initiate")
        .env("CT_CHANNEL_BROKER", &st.bob.broker_addr)
        .env("CT_CHANNEL_RELAY", &st.bob.relay_addr)
        .env("CT_CHANNEL_RELAY_ONLY", "1")
        .env("CT_CHANNEL_HOLDER_KEY", &st.bob.holder_key_hex)
        .env("CT_CHANNEL_NOISE_KEY", &st.bob.noise_key_hex)
        .env("CT_CHANNEL_GRANT", &st.bob.grant_hex)
        .env("CT_CHANNEL_CALL_SERVICE", SERVICE)
        // TCP-:443 handover rung -- see BobIdentity::from_env's fetch_front_door_cert_hex.
        // Without these, ct-agent's RelayFallback only ever tries the QUIC-only rung.
        .env("CT_CHANNEL_FRONT_DOOR", &st.bob.front_door_addr)
        .env("CT_CHANNEL_FRONT_DOOR_CERT", &st.bob.front_door_cert_hex)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            broadcast_event(&tx, json!({"type": "round_error", "message": format!("spawning agent-bob: {e}")})).await;
            return;
        }
    };
    if let Some(mut stdin) = initiator.stdin.take() {
        let _ = stdin.write_all(message.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        // Drop closes stdin so ct-agent's read_line sees EOF-after-newline, same as
        // the tutorial's `echo "..." | ct-agent channel` shape.
    }
    // Two independent streams to drain concurrently: stderr carries `ct-agent`'s own
    // "ct-agent channel: ..." status lines (including the real broker-rendezvous and
    // relay-fallback lines), stdout carries agent-alice's real reply -- relayed back
    // through the edge from a container this process never touches directly.
    let mut initiator_err = BufReader::new(initiator.stderr.take().expect("piped")).lines();
    let mut initiator_out = BufReader::new(initiator.stdout.take().expect("piped")).lines();
    let mut reply: Option<String> = None;
    let wait_result = tokio::time::timeout(INITIATOR_TIMEOUT, async {
        let stderr_task = async {
            while let Ok(Some(line)) = initiator_err.next_line().await {
                if let Some(transport) = relay_transport_from_log_line(&line) {
                    // #106: which rung of ct-agent's relay ladder actually carried this
                    // session -- the real, honest signal for the dashboard's animation.
                    // Never "direct" here: agent-bob and agent-alice are both
                    // CT_CHANNEL_RELAY_ONLY=1 by design (see Alice.Dockerfile) so every
                    // round's data always crosses the edge relay, never peer-to-peer --
                    // this only distinguishes QUIC-relay-port from the :443 front door.
                    broadcast_event(&tx, json!({"type": "transport", "transport": transport, "line": line})).await;
                } else if is_log_line(&line) && (line.contains("relay") || line.contains("brokered")) {
                    broadcast_event(&tx, json!({"type": "channel_connected", "line": line})).await;
                } else {
                    broadcast_event(&tx, json!({"type": "initiator_log", "line": line})).await;
                }
            }
        };
        let stdout_task = async {
            while let Ok(Some(line)) = initiator_out.next_line().await {
                if !line.trim().is_empty() {
                    reply = Some(line);
                }
            }
        };
        tokio::join!(stderr_task, stdout_task);
        initiator.wait().await
    })
    .await;

    match wait_result {
        Ok(Ok(status)) if status.success() => match reply {
            Some(r) => broadcast_event(&tx, json!({"type": "reply_received", "reply": r})).await,
            None => broadcast_event(&tx, json!({"type": "round_error", "message": "agent-bob exited 0 but printed no reply line"})).await,
        },
        Ok(Ok(status)) => broadcast_event(&tx, json!({"type": "round_error", "message": format!("agent-bob exited with {status}")})).await,
        Ok(Err(e)) => broadcast_event(&tx, json!({"type": "round_error", "message": format!("waiting on agent-bob: {e}")})).await,
        Err(_) => {
            let _ = initiator.kill().await;
            broadcast_event(&tx, json!({"type": "round_error", "message": "timed out waiting for agent-alice's reply through the real edge"})).await;
        }
    }
    broadcast_event(&tx, json!({"type": "round_done", "round": round})).await;
}

#[derive(Deserialize)]
struct RunReq {
    message: String,
}

async fn run_handler(State(st): State<Arc<BridgeState>>, Json(req): Json<RunReq>) -> impl IntoResponse {
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "message must not be empty").into_response();
    }
    if message.len() > MAX_MESSAGE_LEN {
        return (axum::http::StatusCode::BAD_REQUEST, format!("message must be <= {MAX_MESSAGE_LEN} chars")).into_response();
    }
    // Only one round at a time -- agent-alice's persistent process accepts concurrent
    // sessions (#200), but this demo keeps one round in flight so the dashboard's
    // event stream stays a single legible sequence per visitor action.
    let Ok(guard) = st.busy.clone().try_lock_owned() else {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, "a round is already running, wait for round_done").into_response();
    };
    let st2 = st.clone();
    tokio::spawn(async move {
        run_round(st2, message).await;
        drop(guard);
    });
    axum::http::StatusCode::ACCEPTED.into_response()
}

async fn events_handler(State(st): State<Arc<BridgeState>>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = st.tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(line) => Some(Ok(Event::default().data(line))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn identities_handler(State(st): State<Arc<BridgeState>>) -> impl IntoResponse {
    Json(json!({
        "channel_id": st.bob.channel_id_hex,
        "bob": {"role": "initiator (calls it, spawned by this bridge per request)", "holder_pubkey": st.bob.holder_pubkey_hex},
        "alice": {"role": "responder (serves text_generation)", "note": "a separate, persistent container (a2a-demo-alice) -- this bridge has no handle on it, only the real channel id both were registered against"},
    }))
}

const INDEX_HTML: &str = include_str!("../index.html");

async fn index_handler() -> impl IntoResponse {
    Html(INDEX_HTML)
}

fn build_state() -> Result<Arc<BridgeState>, String> {
    let bob = BobIdentity::from_env()?;
    let (tx, _rx) = broadcast::channel::<String>(128);
    Ok(Arc::new(BridgeState { bob, round: AtomicU32::new(0), busy: Arc::new(Mutex::new(())), tx }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = build_state().map_err(|e| format!("startup: {e}"))?;
    eprintln!(
        "a2a-demo-bridge: agent-bob configured -- channel={} holder_pubkey={}",
        state.bob.channel_id_hex,
        &state.bob.holder_pubkey_hex[..16.min(state.bob.holder_pubkey_hex.len())]
    );

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/events", get(events_handler))
        .route("/run", post(run_handler))
        .route("/identities", get(identities_handler))
        .with_state(state);

    let addr = std::env::var("A2A_BRIDGE_LISTEN").unwrap_or_else(|_| "0.0.0.0:8790".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("a2a-demo-bridge: serving on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_env_errors_with_the_specific_var_name_instead_of_panicking() {
        // Isolated from the real environment: this test must not accidentally pass
        // because a developer's shell happens to export A2A_BOB_HOLDER_KEY.
        let err = required_env("A2A_DEFINITELY_NOT_SET_IN_ANY_ENV").unwrap_err();
        assert!(err.contains("A2A_DEFINITELY_NOT_SET_IN_ANY_ENV"), "error should name the missing var: {err}");
    }

    #[test]
    fn is_log_line_matches_ct_agent_status_prefix_only() {
        assert!(is_log_line("ct-agent channel: plane-brokered Initiate (relay 1.2.3.4:4436)"));
        assert!(!is_log_line("agent-alice (pid 123, 12:00:00): you said \"hi\""));
    }

    #[test]
    fn relay_transport_from_log_line_distinguishes_quic_from_the_443_front_door() {
        assert_eq!(
            relay_transport_from_log_line("ct-agent channel: relay leg via QUIC (172.18.0.6:4436) (#106)"),
            Some("quic")
        );
        assert_eq!(
            relay_transport_from_log_line("ct-agent channel: relay leg via the :443 front door (edge:443) (#106)"),
            Some("tcp_443")
        );
        assert_eq!(relay_transport_from_log_line("ct-agent channel: plane-brokered Initiate (relay 1.2.3.4:4436)"), None);
        assert_eq!(relay_transport_from_log_line("agent-alice (pid 123, 12:00:00): you said \"hi\""), None);
    }
}
