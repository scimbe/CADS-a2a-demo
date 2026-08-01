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

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
///
/// 25s (found live, 2026-08-01): too tight for the real cross-NAT scenarios
/// (alice-bob1/alice-bob2) -- this budget covers process spawn, broker admission,
/// relay setup, the #104 in-band direct-upgrade probe, AND the actual service-call
/// round-trip to a real remote participant's machine, all in sequence. The
/// same-container baseline "bob" pairing comfortably fits in 25s (negligible network
/// latency); a genuinely NAT'd/remote peer does not. Bumped to give real-world
/// latency room without making a stuck admission hang indefinitely.
///
/// 90s (found live, 2026-08-01, #248 continued): every scenario here (including the
/// baseline "bob" pair -- run_round sets CT_CHANNEL_RELAY_GATE unconditionally, not
/// just for bob1/bob2) now attempts the relay-gate DCUtR path, which ct-agent retries
/// up to twice on a transient `#140` admission stall (ADMISSION_EXCHANGE_TIMEOUT=15s
/// per attempt, ~45s worst case for admission alone). 60s left too little room for the
/// rest of a round on top of that. This still fails loudly, just later.
const INITIATOR_TIMEOUT: Duration = Duration::from_secs(90);

fn ct_agent_bin() -> String {
    std::env::var("CT_AGENT_BIN").unwrap_or_else(|_| "ct-agent".to_string())
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} must be set (see README/provision.md)"))
}

/// Absent-is-fine env lookup, for scenarios that aren't provisioned yet (#134-follow: the
/// bob-1/bob-2 scenarios go live incrementally as intern/remote post their attestations on
/// GitHub #248 -- the bridge must start and serve the OTHER, already-real scenarios in the
/// meantime rather than hard-failing on a channel nobody's finished setting up yet).
fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
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

/// One real, pre-registered "alice initiates, a remote persistent `ct-agent channel
/// --serve` accepts" scenario -- alice-bob1 (intern, NAT'd) or alice-bob2 (remote,
/// public-IP). Reuses alice's SAME holder/noise identity across both (one person, two
/// separate channels/grants -- see `.env`'s `A2A_SCENARIOS_ALICE_*`), unlike
/// [`BobIdentity`] which is a wholly separate identity this bridge itself spawns.
struct PeerScenario {
    label: &'static str,
    alice_holder_key_hex: String,
    alice_noise_key_hex: String,
    grant_hex: String,
    channel_id_hex: String,
    peer_holder_pubkey_hex: String,
}

impl PeerScenario {
    /// `None` when not yet provisioned (intern/remote haven't posted their attestation
    /// yet, see GitHub #248) -- absence is a normal, expected, temporary state, not an
    /// error; the scenario simply doesn't appear until it's real. `channel_var`/
    /// `grant_var`/`peer_pubkey_var` are the exact `.env` var names (deliberately explicit
    /// rather than derived from one shared prefix -- alice-bob1/alice-bob2/bob1-bob2 don't
    /// share a naming pattern regular enough to derive safely, see `.env`).
    fn from_env(label: &'static str, channel_var: &str, grant_var: &str, peer_pubkey_var: &str) -> Option<Self> {
        Some(Self {
            label,
            alice_holder_key_hex: optional_env("A2A_SCENARIOS_ALICE_HOLDER_KEY")?,
            alice_noise_key_hex: optional_env("A2A_SCENARIOS_ALICE_NOISE_KEY")?,
            grant_hex: optional_env(grant_var)?,
            channel_id_hex: optional_env(channel_var)?,
            peer_holder_pubkey_hex: optional_env(peer_pubkey_var)?,
        })
    }
}

/// The shared plane leg every scenario dials through -- same production edge, same
/// broker/relay/front-door, only the identity+grant changes per scenario.
struct PlaneConfig {
    broker_addr: String,
    relay_addr: String,
    front_door_addr: String,
    front_door_cert_hex: String,
}

impl PlaneConfig {
    fn from_env(cp_url: &str) -> Result<Self, String> {
        Ok(Self {
            broker_addr: required_env("A2A_CHANNEL_BROKER")?,
            relay_addr: required_env("A2A_CHANNEL_RELAY")?,
            front_door_addr: required_env("A2A_CHANNEL_FRONT_DOOR")?,
            front_door_cert_hex: fetch_front_door_cert_hex(cp_url)?,
        })
    }
}

/// bob-1 (intern)/bob-2 (remote) self-report liveness by POSTing a shared-secret
/// heartbeat every ~15s (operator's explicit choice over the bridge actively polling
/// them -- their own persistent `ct-agent channel --serve` process is the thing that's
/// actually real; this is just "are you still there", not a channel-level probe).
/// "Online" = a heartbeat within [`HEARTBEAT_STALE_AFTER`].
const HEARTBEAT_STALE_AFTER: Duration = Duration::from_secs(35);

struct HeartbeatPeer {
    token: String,
    last_seen: Mutex<Option<Instant>>,
}

struct HeartbeatState {
    bob1: Option<HeartbeatPeer>,
    bob2: Option<HeartbeatPeer>,
}

impl HeartbeatState {
    fn from_env() -> Self {
        Self {
            bob1: optional_env("A2A_HEARTBEAT_TOKEN_BOB1").map(|token| HeartbeatPeer { token, last_seen: Mutex::new(None) }),
            bob2: optional_env("A2A_HEARTBEAT_TOKEN_BOB2").map(|token| HeartbeatPeer { token, last_seen: Mutex::new(None) }),
        }
    }

    fn peer(&self, name: &str) -> Option<&HeartbeatPeer> {
        match name {
            "bob1" => self.bob1.as_ref(),
            "bob2" => self.bob2.as_ref(),
            _ => None,
        }
    }

    async fn online(&self, name: &str) -> bool {
        match self.peer(name) {
            Some(p) => p.last_seen.lock().await.is_some_and(|t| t.elapsed() < HEARTBEAT_STALE_AFTER),
            None => false,
        }
    }
}

/// Bounded per-peer log retention for the `/logs/:peer` self-service + core-operator
/// endpoints below. In-memory ring buffer, not a file -- a peer's own `ct-agent
/// channel --serve` process already owns its own durable log; what this bridge holds is
/// only the lines it chose to ship via `monitor.sh`, and only the most recent
/// `LOG_BUFFER_CAP_LINES` of them (each already capped at `MAX_MONITOR_LINE_BYTES` by
/// `monitor_handler`). Worst case per peer: ~2MB, fixed regardless of how long the
/// process runs or how chatty the peer gets -- the disk footprint is zero by
/// construction (nothing here ever touches disk), and the memory footprint cannot grow
/// past the cap. Lost on bridge restart; that tradeoff is deliberate, not an oversight --
/// a size-capped rotating file would survive restarts but adds real disk-management
/// surface this demo doesn't need yet.
const LOG_BUFFER_CAP_LINES: usize = 500;

struct LogStore {
    lines: Mutex<HashMap<String, VecDeque<String>>>,
}

impl LogStore {
    fn new() -> Self {
        Self { lines: Mutex::new(HashMap::new()) }
    }

    async fn push(&self, peer: &str, line: &str) {
        let mut lines = self.lines.lock().await;
        let buf = lines.entry(peer.to_string()).or_default();
        buf.push_back(line.to_string());
        while buf.len() > LOG_BUFFER_CAP_LINES {
            buf.pop_front();
        }
    }

    async fn get(&self, peer: &str) -> Vec<String> {
        self.lines.lock().await.get(peer).map(|b| b.iter().cloned().collect()).unwrap_or_default()
    }

    async fn get_all(&self) -> HashMap<String, Vec<String>> {
        self.lines.lock().await.iter().map(|(k, v)| (k.clone(), v.iter().cloned().collect())).collect()
    }
}

struct BridgeState {
    bob: BobIdentity,
    plane: PlaneConfig,
    bob1: Option<PeerScenario>,
    bob2: Option<PeerScenario>,
    heartbeats: HeartbeatState,
    round: AtomicU32,
    busy: Arc<Mutex<()>>,
    tx: broadcast::Sender<String>,
    logs: LogStore,
    /// Operator/"core" secret: presenting this via `x-core-log-token` on `/logs` grants
    /// read access to every peer's buffered log, not just one's own. Unset by default
    /// (`None`) -- the endpoint that depends on it is simply unavailable until an
    /// operator explicitly configures `A2A_CORE_LOG_TOKEN`, matching this file's existing
    /// pattern for `HeartbeatPeer` tokens (`optional_env`, no default secret ever baked in).
    core_log_token: Option<String>,
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

/// Everything one `ct-agent channel` initiator dial needs -- factored out of the
/// original single-bob-only `run_round` so the SAME real dial-and-call logic serves
/// the alice-bob1/alice-bob2 scenarios too, not just agent-bob (#134-follow).
struct DialParams<'a> {
    scenario: &'static str,
    broker_addr: &'a str,
    relay_addr: &'a str,
    holder_key_hex: &'a str,
    noise_key_hex: &'a str,
    grant_hex: &'a str,
    front_door_addr: &'a str,
    front_door_cert_hex: &'a str,
    // #248 root-cause (2026-08-01): CT_CHANNEL_RELAY_ONLY/RELAY_GATE below only make
    // sense when the RESPONDER on the other end is *also* configured for the relay-gate
    // DCUtR Accept path. For bob1/bob2 that's true -- their own external --serve
    // processes were set up to match, live-reproduced getting past admission to a real
    // hole-punch attempt (#248 GitHub thread). For the baseline "bob" scenario the
    // responder is the host-native Alice daemon (real public IP, CT_CHANNEL_DIRECT_UPGRADE=1,
    // relay_only=false by design for the #104 direct-dial experiment) -- she never runs
    // the matching Accept-side code at all. Live-reproduced: every baseline round failed
    // with a downstream "early eof" once relay-gate was unconditionally wired into this
    // function (4ca276c's message said "for bob1/bob2 rounds" but the code change was
    // to this shared function, silently affecting "bob" too). This flag restores the
    // baseline scenario to the plain plane-brokered + #104 path that actually matches
    // what Alice runs, instead of a mismatched protocol neither side can complete.
    use_relay_gate: bool,
}

async fn run_round(tx: broadcast::Sender<String>, round: u32, message: String, params: DialParams<'_>) {
    let scenario = params.scenario;
    broadcast_event(&tx, json!({"type": "round_start", "scenario": scenario, "round": round, "message": message})).await;

    // --- dial the peer's own process: broker-mediated, through the real edge --------
    // This is a genuinely separate process this bridge never touches directly -- it
    // reaches the service only via CT_CHANNEL_BROKER/CT_CHANNEL_RELAY (the production
    // edge). `CT_CHANNEL_CALL_SERVICE` + stdin is exactly the pattern
    // docs.bunsenbrenner.org's serve-a-channel-service.md step 4 walks through by hand.
    broadcast_event(&tx, json!({"type": "initiator_dialing", "scenario": scenario, "broker": params.broker_addr})).await;
    let mut cmd = Command::new(ct_agent_bin());
    cmd.arg("channel")
        .env("CT_CHANNEL_ROLE", "initiate")
        .env("CT_CHANNEL_BROKER", params.broker_addr)
        .env("CT_CHANNEL_RELAY", params.relay_addr)
        .env("CT_CHANNEL_HOLDER_KEY", params.holder_key_hex)
        .env("CT_CHANNEL_NOISE_KEY", params.noise_key_hex)
        .env("CT_CHANNEL_GRANT", params.grant_hex)
        .env("CT_CHANNEL_CALL_SERVICE", SERVICE)
        // TCP-:443 handover rung -- see BobIdentity::from_env's fetch_front_door_cert_hex.
        // Without these, ct-agent's RelayFallback only ever tries the QUIC-only rung.
        .env("CT_CHANNEL_FRONT_DOOR", params.front_door_addr)
        .env("CT_CHANNEL_FRONT_DOOR_CERT", params.front_door_cert_hex)
        // #104: opt in to the in-band relay->direct upgrade, mirrors alice's
        // CT_CHANNEL_DIRECT_UPGRADE=1 (Alice.Dockerfile). No new port either side.
        .env("CT_CHANNEL_DIRECT_UPGRADE", "1")
        // This spawned initiator is a fresh, ephemeral, container-bound process with no
        // listen/advertise address of its own regardless of scenario -- always relay-only
        // from ITS side. Independent of whether it also attempts relay-gate DCUtR below
        // (that's about which protocol the RESPONDER speaks, not whether this side has an
        // address); without this, ct-agent refuses to start at all ("CT_CHANNEL_LISTEN
        // required ... or set CT_CHANNEL_RELAY_ONLY=1").
        .env("CT_CHANNEL_RELAY_ONLY", "1");
    if params.use_relay_gate {
        // #248 real hole-punch: the relay-gate leg is multiplexed on the SAME :443
        // TLS listener as the front door above (different ALPN, `ct-edge-relay` vs
        // whatever the front door uses) -- same address, same cert, no new port or
        // trust anchor. This is the actual deployed NAT-to-NAT DCUtR path
        // (channel_run.rs's `join_via_relay_gate_dcutr`, backed by the real
        // Circuit-Relay v2 relay-node this stack already runs) -- distinct from, and
        // more capable than, the CT_CHANNEL_DIRECT_UPGRADE candidate-exchange above,
        // which only ever helps when at least one side already has a real dialable
        // address. Only set when the RESPONDER is also configured for it (bob1/bob2's
        // own --serve processes) -- see DialParams::use_relay_gate's comment for why
        // this must NOT be unconditional (root-caused live, 2026-08-01: it broke the
        // baseline "bob" scenario, whose responder -- host-native Alice -- runs the
        // #104 direct-dial path instead and never speaks this protocol).
        cmd.env("CT_CHANNEL_RELAY_GATE", params.front_door_addr)
            .env("CT_CHANNEL_RELAY_GATE_CERT", params.front_door_cert_hex);
    }
    let mut initiator = match cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            broadcast_event(&tx, json!({"type": "round_error", "scenario": scenario, "message": format!("spawning the dial: {e}")})).await;
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
    // relay-fallback lines), stdout carries the peer's real reply -- relayed back
    // through the edge from a process this bridge never touches directly.
    let mut initiator_err = BufReader::new(initiator.stderr.take().expect("piped")).lines();
    let mut initiator_out = BufReader::new(initiator.stdout.take().expect("piped")).lines();
    let mut reply: Option<String> = None;
    let wait_result = tokio::time::timeout(INITIATOR_TIMEOUT, async {
        let stderr_task = async {
            while let Ok(Some(line)) = initiator_err.next_line().await {
                if let Some(transport) = relay_transport_from_log_line(&line) {
                    // #106: which rung of ct-agent's relay ladder actually carried this
                    // session -- the real, honest signal for the dashboard's animation.
                    // Never "direct" here: both members are CT_CHANNEL_RELAY_ONLY=1 by
                    // design so every round's data always crosses the edge relay, never
                    // peer-to-peer -- this only distinguishes QUIC-relay-port from :443.
                    broadcast_event(&tx, json!({"type": "transport", "scenario": scenario, "transport": transport, "line": line})).await;
                } else if is_log_line(&line) && (line.contains("relay") || line.contains("brokered")) {
                    broadcast_event(&tx, json!({"type": "channel_connected", "scenario": scenario, "line": line})).await;
                } else {
                    broadcast_event(&tx, json!({"type": "initiator_log", "scenario": scenario, "line": line})).await;
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
            Some(r) => broadcast_event(&tx, json!({"type": "reply_received", "scenario": scenario, "reply": r})).await,
            None => broadcast_event(&tx, json!({"type": "round_error", "scenario": scenario, "message": "the peer exited 0 but printed no reply line"})).await,
        },
        Ok(Ok(status)) => broadcast_event(&tx, json!({"type": "round_error", "scenario": scenario, "message": format!("the peer exited with {status}")})).await,
        Ok(Err(e)) => broadcast_event(&tx, json!({"type": "round_error", "scenario": scenario, "message": format!("waiting on the peer: {e}")})).await,
        Err(_) => {
            let _ = initiator.kill().await;
            broadcast_event(&tx, json!({"type": "round_error", "scenario": scenario, "message": "timed out waiting for a reply through the real edge"})).await;
        }
    }
    broadcast_event(&tx, json!({"type": "round_done", "scenario": scenario, "round": round})).await;
}

#[derive(Deserialize)]
struct RunReq {
    message: String,
}

fn validate_message(raw: &str) -> Result<String, (axum::http::StatusCode, &'static str)> {
    let message = raw.trim().to_string();
    if message.is_empty() {
        return Err((axum::http::StatusCode::BAD_REQUEST, "message must not be empty"));
    }
    if message.len() > MAX_MESSAGE_LEN {
        return Err((axum::http::StatusCode::BAD_REQUEST, "message too long"));
    }
    Ok(message)
}

async fn run_handler(State(st): State<Arc<BridgeState>>, Json(req): Json<RunReq>) -> impl IntoResponse {
    let message = match validate_message(&req.message) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    // Only one round at a time -- agent-alice's persistent process accepts concurrent
    // sessions (#200), but this demo keeps one round in flight so the dashboard's
    // event stream stays a single legible sequence per visitor action.
    let Ok(guard) = st.busy.clone().try_lock_owned() else {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, "a round is already running, wait for round_done").into_response();
    };
    let tx = st.tx.clone();
    let round = st.round.fetch_add(1, Ordering::SeqCst) + 1;
    let (broker, relay, holder, noise, grant, fd, fd_cert) = (
        st.bob.broker_addr.clone(),
        st.bob.relay_addr.clone(),
        st.bob.holder_key_hex.clone(),
        st.bob.noise_key_hex.clone(),
        st.bob.grant_hex.clone(),
        st.bob.front_door_addr.clone(),
        st.bob.front_door_cert_hex.clone(),
    );
    tokio::spawn(async move {
        let params = DialParams {
            scenario: "bob",
            broker_addr: &broker,
            relay_addr: &relay,
            holder_key_hex: &holder,
            noise_key_hex: &noise,
            grant_hex: &grant,
            front_door_addr: &fd,
            front_door_cert_hex: &fd_cert,
            // The responder here is the host-native Alice daemon, not relay-gate capable
            // -- see DialParams::use_relay_gate's comment.
            use_relay_gate: false,
        };
        run_round(tx, round, message, params).await;
        drop(guard);
    });
    axum::http::StatusCode::ACCEPTED.into_response()
}

/// Triggers a real dial for the alice-bob1/alice-bob2 scenario (#134-follow) -- the
/// SAME real `ct-agent channel` initiator+CT_CHANNEL_CALL_SERVICE dial `run_handler`
/// already does for agent-bob, just against a different pre-registered identity/grant.
/// 404s (not 503) when the scenario isn't provisioned yet -- a scenario that doesn't
/// exist isn't "temporarily down", it's genuinely absent from this deployment until
/// intern/remote finish their side (see GitHub #248).
async fn run_scenario_handler(State(st): State<Arc<BridgeState>>, Path(scenario): Path<String>, Json(req): Json<RunReq>) -> impl IntoResponse {
    let message = match validate_message(&req.message) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let peer = match scenario.as_str() {
        "bob1" => st.bob1.as_ref(),
        "bob2" => st.bob2.as_ref(),
        _ => return (axum::http::StatusCode::NOT_FOUND, "unknown scenario").into_response(),
    };
    let Some(peer) = peer else {
        return (axum::http::StatusCode::NOT_FOUND, "scenario not provisioned yet").into_response();
    };
    let Ok(guard) = st.busy.clone().try_lock_owned() else {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, "a round is already running, wait for round_done").into_response();
    };
    let tx = st.tx.clone();
    let round = st.round.fetch_add(1, Ordering::SeqCst) + 1;
    let scenario_label: &'static str = if scenario == "bob1" { "bob1" } else { "bob2" };
    let (broker, relay, holder, noise, grant, fd, fd_cert) = (
        st.plane.broker_addr.clone(),
        st.plane.relay_addr.clone(),
        peer.alice_holder_key_hex.clone(),
        peer.alice_noise_key_hex.clone(),
        peer.grant_hex.clone(),
        st.plane.front_door_addr.clone(),
        st.plane.front_door_cert_hex.clone(),
    );
    tokio::spawn(async move {
        let params = DialParams {
            scenario: scenario_label,
            broker_addr: &broker,
            relay_addr: &relay,
            holder_key_hex: &holder,
            noise_key_hex: &noise,
            grant_hex: &grant,
            front_door_addr: &fd,
            front_door_cert_hex: &fd_cert,
            // bob1/bob2's own --serve processes are configured to match relay-gate DCUtR
            // -- see DialParams::use_relay_gate's comment.
            use_relay_gate: true,
        };
        run_round(tx, round, message, params).await;
        drop(guard);
    });
    axum::http::StatusCode::ACCEPTED.into_response()
}

/// bob-1 (intern) / bob-2 (remote) self-report liveness here -- a shared-secret POST,
/// not the A2A channel itself (the operator's explicit choice: this is "are you still
/// there", independent of whether a channel dial happens to be in flight right now).
async fn heartbeat_handler(State(st): State<Arc<BridgeState>>, Path(peer_name): Path<String>, body: String) -> impl IntoResponse {
    let Some(peer) = st.heartbeats.peer(&peer_name) else {
        return (axum::http::StatusCode::NOT_FOUND, "unknown peer").into_response();
    };
    if body.trim() != peer.token {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    let was_online = peer.last_seen.lock().await.is_some_and(|t| t.elapsed() < HEARTBEAT_STALE_AFTER);
    *peer.last_seen.lock().await = Some(Instant::now());
    if !was_online {
        broadcast_event(&st.tx, json!({"type": "presence", "peer": peer_name, "online": true})).await;
    }
    axum::http::StatusCode::NO_CONTENT.into_response()
}

/// #248: bob-1/bob-2 self-ship real log lines from their own `ct-agent channel --serve`
/// process onto the dashboard's live event stream -- `heartbeat_handler` above only ever
/// answers "are you still there", never "what are you actually seeing". Deliberately
/// reuses the SAME per-peer heartbeat token (no new secret to generate/distribute) but
/// as a header, not the body, since the body here is the log line itself, not a token.
/// One line per request by design (matches `monitor.sh`'s stdin-line-at-a-time shipping)
/// -- a peer under real load ships more requests, not bigger ones, keeping this endpoint's
/// worst case bounded per-call regardless of how chatty the peer's own process gets.
const MAX_MONITOR_LINE_BYTES: usize = 4096;

async fn monitor_handler(
    State(st): State<Arc<BridgeState>>,
    Path(peer_name): Path<String>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let Some(peer) = st.heartbeats.peer(&peer_name) else {
        return (axum::http::StatusCode::NOT_FOUND, "unknown peer").into_response();
    };
    let token_ok = headers
        .get("x-heartbeat-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t == peer.token);
    if !token_ok {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    let line = body.trim();
    if line.is_empty() {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }
    if line.len() > MAX_MONITOR_LINE_BYTES {
        return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    st.logs.push(&peer_name, line).await;
    broadcast_event(&st.tx, json!({"type": "peer_log", "scenario": peer_name, "line": line})).await;
    axum::http::StatusCode::NO_CONTENT.into_response()
}

/// Read-back for the bounded log buffer `monitor_handler` fills. Two access levels,
/// both header-based (matches `monitor_handler`'s own convention, no query-string
/// secrets in server access logs):
///   - `x-heartbeat-token: <peer's own token>` -- that peer's own buffered lines only.
///     Same shared secret bob-1/bob-2 already hold from #248, no new credential to
///     distribute for self-service access.
///   - `x-core-log-token: <A2A_CORE_LOG_TOKEN>` -- every peer's buffered lines, keyed by
///     name. Operator-only; unavailable (404) unless that env var is actually set.
/// A caller presenting neither, or a `:peer` path with a mismatched token, sees exactly
/// what an unauthenticated caller would (401/404) -- no distinction that would leak
/// whether a given peer name is valid.
async fn logs_handler(State(st): State<Arc<BridgeState>>, Path(peer_name): Path<String>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    if let Some(core_token) = st.core_log_token.as_deref() {
        let core_ok = headers.get("x-core-log-token").and_then(|v| v.to_str().ok()).is_some_and(|t| t == core_token);
        if core_ok {
            if peer_name == "all" {
                return Json(json!(st.logs.get_all().await)).into_response();
            }
            return Json(json!({"peer": peer_name, "lines": st.logs.get(&peer_name).await})).into_response();
        }
    }
    let Some(peer) = st.heartbeats.peer(&peer_name) else {
        return (axum::http::StatusCode::NOT_FOUND, "unknown peer").into_response();
    };
    let token_ok = headers.get("x-heartbeat-token").and_then(|v| v.to_str().ok()).is_some_and(|t| t == peer.token);
    if !token_ok {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"peer": peer_name, "lines": st.logs.get(&peer_name).await})).into_response()
}

/// Current online/offline snapshot for every peer -- the frontend calls this once on
/// load (SSE only delivers events from the moment it connects onward, so a page opened
/// between heartbeats needs an explicit current-state read, not just the stream).
async fn status_handler(State(st): State<Arc<BridgeState>>) -> impl IntoResponse {
    Json(json!({
        "bob1_online": st.heartbeats.online("bob1").await,
        "bob2_online": st.heartbeats.online("bob2").await,
        "bob1_provisioned": st.bob1.is_some(),
        "bob2_provisioned": st.bob2.is_some(),
    }))
}

/// Ticks every 5s and broadcasts a `presence` event on transition to OFFLINE (the
/// transition to online is already caught by `heartbeat_handler` itself, immediately --
/// this only catches the side heartbeat POSTs can't: a peer that simply stops sending
/// them). A `status` poll (`GET /status`) always reflects the truth regardless of
/// whether this tick has caught up yet; this just makes the SSE stream self-correcting
/// too, so a dashboard left open doesn't show a peer as online forever after it drops.
async fn presence_watchdog(st: Arc<BridgeState>) {
    let mut was_online = (false, false);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let now = (st.heartbeats.online("bob1").await, st.heartbeats.online("bob2").await);
        if now.0 != was_online.0 {
            broadcast_event(&st.tx, json!({"type": "presence", "peer": "bob1", "online": now.0})).await;
        }
        if now.1 != was_online.1 {
            broadcast_event(&st.tx, json!({"type": "presence", "peer": "bob2", "online": now.1})).await;
        }
        was_online = now;
    }
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
    let peer_json = |p: &Option<PeerScenario>| p.as_ref().map(|s| json!({"label": s.label, "channel_id": s.channel_id_hex, "peer_holder_pubkey": s.peer_holder_pubkey_hex}));
    Json(json!({
        "channel_id": st.bob.channel_id_hex,
        "bob": {"role": "initiator (calls it, spawned by this bridge per request)", "holder_pubkey": st.bob.holder_pubkey_hex},
        "alice": {"role": "responder (serves text_generation)", "note": "a separate, persistent container (a2a-demo-alice) -- this bridge has no handle on it, only the real channel id both were registered against"},
        "bob1": peer_json(&st.bob1),
        "bob2": peer_json(&st.bob2),
    }))
}

const INDEX_HTML: &str = include_str!("../index.html");

async fn index_handler() -> impl IntoResponse {
    Html(INDEX_HTML)
}

fn build_state() -> Result<Arc<BridgeState>, String> {
    let bob = BobIdentity::from_env()?;
    let plane = PlaneConfig::from_env(&required_env("A2A_AGENT_CP_URL")?)?;
    let bob1 = PeerScenario::from_env("bob-1 (intern, NAT'd)", "A2A_S_ALICE_BOB1_CHANNEL_ID", "A2A_S_ALICE_BOB1_ALICE_GRANT", "A2A_S_BOB1_HOLDER_PUBKEY");
    let bob2 = PeerScenario::from_env("bob-2 (remote, public-IP)", "A2A_S_ALICE_BOB2_CHANNEL_ID", "A2A_S_ALICE_BOB2_ALICE_GRANT", "A2A_S_BOB2_HOLDER_PUBKEY");
    let heartbeats = HeartbeatState::from_env();
    let (tx, _rx) = broadcast::channel::<String>(128);
    let core_log_token = optional_env("A2A_CORE_LOG_TOKEN");
    Ok(Arc::new(BridgeState {
        bob,
        plane,
        bob1,
        bob2,
        heartbeats,
        round: AtomicU32::new(0),
        busy: Arc::new(Mutex::new(())),
        tx,
        logs: LogStore::new(),
        core_log_token,
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = build_state().map_err(|e| format!("startup: {e}"))?;
    eprintln!(
        "a2a-demo-bridge: agent-bob configured -- channel={} holder_pubkey={}",
        state.bob.channel_id_hex,
        &state.bob.holder_pubkey_hex[..16.min(state.bob.holder_pubkey_hex.len())]
    );
    eprintln!(
        "a2a-demo-bridge: bob-1 (intern) {} -- bob-2 (remote) {}",
        if state.bob1.is_some() { "provisioned" } else { "NOT yet provisioned (see GitHub #248)" },
        if state.bob2.is_some() { "provisioned" } else { "NOT yet provisioned (see GitHub #248)" },
    );

    tokio::spawn(presence_watchdog(state.clone()));

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/events", get(events_handler))
        .route("/run", post(run_handler))
        .route("/run/:scenario", post(run_scenario_handler))
        .route("/heartbeat/:peer", post(heartbeat_handler))
        .route("/monitor/:peer", post(monitor_handler))
        .route("/logs/:peer", get(logs_handler))
        .route("/status", get(status_handler))
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
