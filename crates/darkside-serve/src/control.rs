//! The HTTP control surface: one route per command, JSON in and out.
//!
//! A frontend, nothing more. Each handler decodes its body into a
//! [`Command`], hands it to the [`Dispatcher`], and renders whatever comes
//! back. All the behavior lives in `command`.
//!
//! This surface fabricates value and rewrites history, so binding it
//! anywhere but loopback is refused until there is something to authenticate
//! with. See [`is_loopback`].

use std::net::SocketAddr;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::command::{self, Command, Dispatcher, Outcome, parse_receivers, parse_zec};
use crate::driver::Tick;

/// Routes for `dispatcher`, ready to serve.
pub fn router(dispatcher: Dispatcher) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/mine", post(mine))
        .route("/to", post(mine_to))
        .route("/next", post(next_upgrade))
        .route("/advance", post(advance))
        .route("/fund", post(fund))
        .route("/pause", post(pause))
        .route("/resume", post(resume))
        .route("/tick", post(tick))
        .route("/reorg", post(reorg))
        .route("/reset", post(reset))
        .route("/withhold", post(withhold))
        .with_state(dispatcher)
}

/// Whether `addr` is a loopback address, the only kind the control surface
/// may bind. The sync surface leaks fabricated state. This one hands over the
/// ability to author it, and there is no token to gate that yet.
pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// The route and JSON body a command travels as.
///
/// Kept beside the handlers that receive them, so a client cannot drift from
/// the routes it calls.
pub fn request(command: &Command) -> (&'static str, serde_json::Value) {
    use serde_json::json;
    match command {
        Command::Mine { blocks } => ("/mine", json!({ "blocks": blocks })),
        Command::MineTo { height } => ("/to", json!({ "height": height })),
        Command::NextUpgrade => ("/next", json!({})),
        Command::Advance { upgrade } => ("/advance", json!({ "upgrade": upgrade })),
        Command::Fund {
            address,
            zats,
            receivers,
        } => (
            "/fund",
            json!({
                "address": address,
                "zec": zats_as_zec(*zats),
                "receivers": receivers
                    .as_ref()
                    .map(|set| set.iter().map(|r| r.letter()).collect::<String>()),
            }),
        ),
        Command::Pause => ("/pause", json!({})),
        Command::Resume => ("/resume", json!({})),
        Command::SetTick { tick } => ("/tick", json!({ "tick": tick.to_string() })),
        Command::Reorg { depth } => ("/reorg", json!({ "depth": depth })),
        Command::Reset => ("/reset", json!({})),
        Command::Withhold { on } => ("/withhold", json!({ "on": on })),
        Command::Status => ("/status", json!({})),
    }
}

/// Zatoshis as the exact decimal string `/fund` expects, so a round trip
/// through the wire never touches a float.
fn zats_as_zec(zats: u64) -> String {
    format!("{}.{:08}", zats / 100_000_000, zats % 100_000_000)
}

/// A command that failed, rendered as a 400 with a JSON body. Every failure
/// here is caller-caused: a malformed request, or one this chain cannot
/// satisfy.
struct Failure(command::Error);

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

impl From<command::Error> for Failure {
    fn from(e: command::Error) -> Self {
        Failure(e)
    }
}

type Reply = Result<Json<Outcome>, Failure>;

fn run(dispatcher: &Dispatcher, command: Command) -> Reply {
    let outcome = dispatcher.run(command)?;
    for warning in &outcome.warnings {
        tracing::warn!(target: "darkside", "{warning}");
    }
    Ok(Json(outcome))
}

#[derive(Deserialize)]
struct MineBody {
    #[serde(default = "one")]
    blocks: u32,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct ToBody {
    height: u32,
}

#[derive(Deserialize)]
struct AdvanceBody {
    /// Upgrade name, e.g. `ironwood` or `nu5`. Resolved server-side too, so
    /// a direct caller gets the same validation the console does.
    upgrade: String,
}

#[derive(Deserialize)]
struct FundBody {
    address: String,
    /// ZEC as a decimal string. A string rather than a number so no amount
    /// passes through a float on its way to zatoshis.
    zec: String,
    /// Receiver letters in payment order. Absent means the newest receiver
    /// the address carries and the chain has active.
    #[serde(default)]
    receivers: Option<String>,
}

#[derive(Deserialize)]
struct TickBody {
    /// Seconds between blocks: `N`, or `LOW..HIGH` for a range.
    tick: String,
}

#[derive(Deserialize)]
struct ReorgBody {
    depth: u32,
}

#[derive(Deserialize)]
struct WithholdBody {
    on: bool,
}

async fn status(State(dispatcher): State<Dispatcher>) -> Reply {
    run(&dispatcher, Command::Status)
}

async fn mine(State(dispatcher): State<Dispatcher>, Json(body): Json<MineBody>) -> Reply {
    run(
        &dispatcher,
        Command::Mine {
            blocks: body.blocks,
        },
    )
}

async fn mine_to(State(dispatcher): State<Dispatcher>, Json(body): Json<ToBody>) -> Reply {
    run(
        &dispatcher,
        Command::MineTo {
            height: body.height,
        },
    )
}

async fn next_upgrade(State(dispatcher): State<Dispatcher>) -> Reply {
    run(&dispatcher, Command::NextUpgrade)
}

async fn advance(State(dispatcher): State<Dispatcher>, Json(body): Json<AdvanceBody>) -> Reply {
    run(
        &dispatcher,
        Command::Advance {
            upgrade: body.upgrade,
        },
    )
}

async fn fund(State(dispatcher): State<Dispatcher>, Json(body): Json<FundBody>) -> Reply {
    let receivers = body.receivers.as_deref().map(parse_receivers).transpose()?;
    run(
        &dispatcher,
        Command::Fund {
            address: body.address,
            zats: parse_zec(&body.zec)?,
            receivers,
        },
    )
}

async fn pause(State(dispatcher): State<Dispatcher>) -> Reply {
    run(&dispatcher, Command::Pause)
}

async fn resume(State(dispatcher): State<Dispatcher>) -> Reply {
    run(&dispatcher, Command::Resume)
}

async fn tick(State(dispatcher): State<Dispatcher>, Json(body): Json<TickBody>) -> Reply {
    let tick: Tick = body
        .tick
        .parse()
        .map_err(|e: String| Failure(command::Error::Request(e)))?;
    run(&dispatcher, Command::SetTick { tick })
}

async fn reorg(State(dispatcher): State<Dispatcher>, Json(body): Json<ReorgBody>) -> Reply {
    run(&dispatcher, Command::Reorg { depth: body.depth })
}

async fn reset(State(dispatcher): State<Dispatcher>) -> Reply {
    run(&dispatcher, Command::Reset)
}

async fn withhold(State(dispatcher): State<Dispatcher>, Json(body): Json<WithholdBody>) -> Reply {
    run(&dispatcher, Command::Withhold { on: body.on })
}
