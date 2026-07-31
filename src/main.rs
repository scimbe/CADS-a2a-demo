//! CADS-a2a-demo bridge: a live web dashboard over a REAL Agent-Fabric channel call
//! between two genuinely separate `ct-agent channel` OS processes -- the exact
//! direct-address mechanism docs.bunsenbrenner.org's `_tutorials/first-channel.md`
//! walks through by hand (verified there: two independent processes, a real Noise_IK
//! handshake, a real request/response). This bridge is just what clicks the buttons a
//! human would: it spawns the responder, parses the cert it prints, spawns the
//! initiator with the visitor's own message on stdin, and streams every real step over
//! SSE. No fixture, no simulated network -- direct-address is single-shot by design
//! (confirmed in the docs), so a fresh responder process is spawned each round.

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
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
/// How long to wait for the responder to print its listening line / cert before giving
/// up -- generous for a cold container start, but bounded so a broken `ct-agent` binary
/// fails loudly instead of hanging the dashboard forever.
const RESPONDER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const INITIATOR_TIMEOUT: Duration = Duration::from_secs(15);

fn ct_agent_bin() -> String {
    std::env::var("CT_AGENT_BIN").unwrap_or_else(|_| "ct-agent".to_string())
}

fn handler_path() -> String {
    std::env::var("A2A_HANDLER_SCRIPT").unwrap_or_else(|_| "/usr/local/bin/handler.sh".to_string())
}

/// One side's Noise (X25519) identity: what `ct-agent channel init` actually printed,
/// parsed from its real stdout -- never fabricated locally, so the exact same key
/// material `ct-agent` itself would use is what this bridge hands back to it.
#[derive(Clone)]
struct Identity {
    noise_priv_hex: String,
    noise_pub_hex: String,
}

/// Parses `ct-agent channel init`'s real stdout shape (verified against a real captured
/// run and against docs.bunsenbrenner.org's own captured run in `_tutorials/first-channel.md`):
/// ```text
/// #   noise_pubkey  = <hex>
/// export CT_CHANNEL_NOISE_KEY=<hex>
/// ```
/// The comment lines are `#`-prefixed before the field name, so a prefix-match on
/// "noise_pubkey" itself (after trim) misses them -- split on `=` instead.
fn parse_channel_init_output(text: &str, label: &str) -> Result<Identity, String> {
    let noise_pub_hex = text
        .lines()
        .find(|l| l.contains("noise_pubkey") && l.contains('='))
        .and_then(|l| l.split('=').nth(1))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("could not find noise_pubkey in `ct-agent channel init` output for {label}: {text}"))?;
    let noise_priv_hex = text
        .lines()
        .find(|l| l.contains("CT_CHANNEL_NOISE_KEY="))
        .and_then(|l| l.split("CT_CHANNEL_NOISE_KEY=").nth(1))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("could not find CT_CHANNEL_NOISE_KEY in `ct-agent channel init` output for {label}: {text}"))?;
    Ok(Identity { noise_priv_hex, noise_pub_hex })
}

async fn mint_identity(label: &str) -> Result<Identity, String> {
    let out = Command::new(ct_agent_bin())
        .args(["channel", "init"])
        .output()
        .await
        .map_err(|e| format!("spawning `ct-agent channel init` for {label}: {e}"))?;
    if !out.status.success() {
        return Err(format!("`ct-agent channel init` for {label} exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr)));
    }
    parse_channel_init_output(&String::from_utf8_lossy(&out.stdout), label)
}

struct BridgeState {
    alice: Identity, // responder ("agent-alice", serves text_generation)
    bob: Identity,   // initiator ("agent-bob", calls it)
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

async fn run_round(st: Arc<BridgeState>, message: String) {
    let tx = st.tx.clone();
    let round = st.round.fetch_add(1, Ordering::SeqCst) + 1;
    // 127.0.0.1 only (never exposed beyond this container) -- a fresh port per round
    // sidesteps TIME_WAIT on the previous round's now-exited responder.
    let port = 19700 + (round % 500);
    let addr = format!("127.0.0.1:{port}");

    broadcast_event(&tx, json!({"type": "round_start", "round": round, "message": message})).await;

    // --- spawn the responder (accept side, direct-address, single-shot) -------------
    broadcast_event(&tx, json!({"type": "responder_starting", "addr": addr})).await;
    let mut responder = match Command::new(ct_agent_bin())
        .arg("channel")
        .env("CT_CHANNEL_ROLE", "accept")
        .env("CT_CHANNEL_ADDR", &addr)
        .env("CT_CHANNEL_NOISE_KEY", &st.alice.noise_priv_hex)
        .env("CT_CHANNEL_PEER_NOISE_KEY", &st.bob.noise_pub_hex)
        .env("CT_CHANNEL_SERVE", "1")
        .env("CT_AGENT_SERVICE_HANDLER_CMD", handler_path())
        .env("CT_AGENT_SERVICES", SERVICE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            broadcast_event(&tx, json!({"type": "round_error", "message": format!("spawning responder: {e}")})).await;
            return;
        }
    };
    // `ct-agent channel`'s own "ct-agent channel: ..." status/log lines -- including the
    // one naming the cert a dial needs -- go to STDERR, not stdout (confirmed live: piped
    // stdout was empty the whole run, every line showed up on stderr instead). Only the
    // actual handler reply, for the *initiator* below, comes back on stdout.
    let mut responder_err = BufReader::new(responder.stderr.take().expect("piped")).lines();

    // Wait for the responder's real printed line naming its own cert (the exact hex a
    // real dial needs) -- not a fixed sleep, so this genuinely tracks when it's ready.
    let cert_hex = match tokio::time::timeout(RESPONDER_READY_TIMEOUT, async {
        loop {
            match responder_err.next_line().await {
                Ok(Some(line)) => {
                    if is_log_line(&line) {
                        broadcast_event(&tx, json!({"type": "responder_log", "line": line})).await;
                    }
                    if let Some(idx) = line.find("CT_CHANNEL_PEER_CERT=") {
                        return Some(line[idx + "CT_CHANNEL_PEER_CERT=".len()..].trim().to_string());
                    }
                }
                Ok(None) => return None, // responder exited before printing a cert -- real failure
                Err(_) => return None,
            }
        }
    })
    .await
    {
        Ok(Some(cert)) => cert,
        Ok(None) => {
            let _ = responder.kill().await;
            broadcast_event(&tx, json!({"type": "round_error", "message": "responder exited before it was ready (see responder_log lines above)"})).await;
            return;
        }
        Err(_) => {
            let _ = responder.kill().await;
            broadcast_event(&tx, json!({"type": "round_error", "message": "timed out waiting for the responder to start"})).await;
            return;
        }
    };
    broadcast_event(&tx, json!({"type": "responder_listening", "addr": addr, "cert_len": cert_hex.len()})).await;

    // --- spawn the initiator (calls it once, with the visitor's own message) --------
    broadcast_event(&tx, json!({"type": "initiator_dialing", "addr": addr})).await;
    let mut initiator = match Command::new(ct_agent_bin())
        .arg("channel")
        .env("CT_CHANNEL_ROLE", "initiate")
        .env("CT_CHANNEL_ADDR", &addr)
        .env("CT_CHANNEL_NOISE_KEY", &st.bob.noise_priv_hex)
        .env("CT_CHANNEL_PEER_NOISE_KEY", &st.alice.noise_pub_hex)
        .env("CT_CHANNEL_PEER_CERT", &cert_hex)
        .env("CT_CHANNEL_CALL_SERVICE", SERVICE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = responder.kill().await;
            broadcast_event(&tx, json!({"type": "round_error", "message": format!("spawning initiator: {e}")})).await;
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
    // "ct-agent channel: ..." status lines (including "connected to..."), stdout carries
    // the handler's real reply and nothing else -- confirmed live by piping each to a
    // separate file and inspecting both.
    let mut initiator_err = BufReader::new(initiator.stderr.take().expect("piped")).lines();
    let mut initiator_out = BufReader::new(initiator.stdout.take().expect("piped")).lines();
    let mut reply: Option<String> = None;
    let wait_result = tokio::time::timeout(INITIATOR_TIMEOUT, async {
        let stderr_task = async {
            while let Ok(Some(line)) = initiator_err.next_line().await {
                if line.contains("connected to") {
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

    let _ = responder.wait().await; // single-shot: exits right after serving this one session

    match wait_result {
        Ok(Ok(status)) if status.success() => match reply {
            Some(r) => broadcast_event(&tx, json!({"type": "reply_received", "reply": r})).await,
            None => broadcast_event(&tx, json!({"type": "round_error", "message": "initiator exited 0 but printed no reply line"})).await,
        },
        Ok(Ok(status)) => broadcast_event(&tx, json!({"type": "round_error", "message": format!("initiator exited with {status}")})).await,
        Ok(Err(e)) => broadcast_event(&tx, json!({"type": "round_error", "message": format!("waiting on initiator: {e}")})).await,
        Err(_) => {
            let _ = initiator.kill().await;
            broadcast_event(&tx, json!({"type": "round_error", "message": "timed out waiting for the initiator's reply"})).await;
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
    // Only one round at a time -- two real processes + one fixed-by-counter port per
    // round is not designed for concurrent overlapping rounds from multiple visitors.
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
        "alice": {"role": "responder (serves text_generation)", "noise_pubkey": st.alice.noise_pub_hex},
        "bob": {"role": "initiator (calls it)", "noise_pubkey": st.bob.noise_pub_hex},
    }))
}

const INDEX_HTML: &str = include_str!("../index.html");

async fn index_handler() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn build_state() -> Result<Arc<BridgeState>, String> {
    let alice = mint_identity("alice").await?;
    let bob = mint_identity("bob").await?;
    let (tx, _rx) = broadcast::channel::<String>(128);
    Ok(Arc::new(BridgeState { alice, bob, round: AtomicU32::new(0), busy: Arc::new(Mutex::new(())), tx }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = build_state().await.map_err(|e| format!("startup: {e}"))?;
    eprintln!(
        "a2a-demo-bridge: identities minted -- alice(responder)={} bob(initiator)={}",
        &state.alice.noise_pub_hex[..16.min(state.alice.noise_pub_hex.len())],
        &state.bob.noise_pub_hex[..16.min(state.bob.noise_pub_hex.len())]
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
    fn parses_a_real_captured_channel_init_output() {
        // Byte-for-byte a real `ct-agent channel init` run captured while debugging this
        // demo (the bug this test guards: the comment lines are `#`-prefixed, so an
        // earlier prefix-match version of this parser silently failed to find either
        // field and the bridge refused to start).
        let real = "# Agent-Fabric channel identity — generated locally, keep the private keys secret.\n\
# Give these PUBLIC keys to the channel operator (to sign your grant / register):\n\
#   holder_pubkey = c5fbd808a8e23e8794e85672e9dea0aee69bdef6e7867e90d0beab920989c609\n\
#   noise_pubkey  = 4ccc079c9d3175e82dd0625e30faa152c416f71eeab1e18eb212bd88732a4145\n\
export CT_CHANNEL_HOLDER_KEY=cb22cf0cd425f6775bb15f3c6a577b91abed5730e36df7c5890e55ac74ce22f9\n\
export CT_CHANNEL_NOISE_KEY=40a888140848c482811565111aebacdb78f6295692be73a336753399377eb9ae\n";
        let id = parse_channel_init_output(real, "alice").expect("real captured output must parse");
        assert_eq!(id.noise_pub_hex, "4ccc079c9d3175e82dd0625e30faa152c416f71eeab1e18eb212bd88732a4145");
        assert_eq!(id.noise_priv_hex, "40a888140848c482811565111aebacdb78f6295692be73a336753399377eb9ae");
    }

    #[test]
    fn missing_fields_error_instead_of_panicking() {
        assert!(parse_channel_init_output("garbage, no keys here", "bob").is_err());
    }
}
