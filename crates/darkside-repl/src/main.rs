//! darkside-repl: the interactive console for a running darkside.
//!
//! A client, nothing else. It owns no chain and serves no RPC. Every line
//! typed here becomes the same HTTP request a script would send, so a command
//! cannot work at this prompt and fail over the wire.
//!
//! Point it at a `darkside serve` and drive: mine, fund an address, reorg,
//! withhold, retime the miner.

use std::io::{BufRead as _, Write as _};

use anyhow::{Context as _, anyhow};
use clap::Parser;
use darkside_serve::command::{Command, Detail, Outcome, parse_line, upgrade_names};
use darkside_serve::control;
use serde::Deserialize;

#[derive(Parser)]
#[command(
    name = "darkside-repl",
    version,
    about = "darkside-repl: an interactive console for a running darkside server"
)]
struct Args {
    /// Base URL of the darkside control surface.
    #[arg(long, default_value = "http://127.0.0.1:9068")]
    control: String,
}

/// The body a failed command comes back as.
#[derive(Deserialize)]
struct Failure {
    error: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let base = args.control.trim_end_matches('/').to_owned();
    let agent = agent();

    // Ask for status before printing a prompt, so an unreachable server is an
    // error at startup rather than a mystery on the first command.
    let hello = send(&agent, &base, &Command::Status)
        .with_context(|| format!("no darkside answered at {base}. Is `darkside serve` running?"))?;
    match hello {
        Ok(outcome) => {
            eprintln!("darkside-repl: connected to {base}");
            if let Detail::Status(status) = &outcome.detail {
                eprintln!("  serving {} at tip {}", status.chain, status.tip);
            }
            eprintln!("{}", outcome.summary);
        }
        Err(failure) => {
            return Err(anyhow!(
                "{base} refused a status request: {}",
                failure.error
            ));
        }
    }
    eprintln!("type help for commands");

    repl(&agent, &base)
}

fn repl(agent: &ureq::Agent, base: &str) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut line = String::new();
    loop {
        print!("darkside> ");
        std::io::stdout().flush().ok();
        line.clear();
        if handle.read_line(&mut line)? == 0 {
            println!();
            return Ok(());
        }
        let line = line.trim();
        match line.split_whitespace().next() {
            None => continue,
            Some("help" | "?") => {
                print_help();
                continue;
            }
            Some("quit" | "exit" | "q") => return Ok(()),
            Some(_) => {}
        }
        match parse_line(line) {
            Ok(None) => {}
            Ok(Some(command)) => report(send(agent, base, &command)),
            Err(e) => eprintln!("{e}"),
        }
    }
}

/// Render one exchange. A rejected command is reported and the loop carries
/// on, the way a terminal should behave.
fn report(reply: anyhow::Result<Result<Outcome, Failure>>) {
    match reply {
        Ok(Ok(outcome)) => {
            for warning in &outcome.warnings {
                eprintln!("warning: {warning}");
            }
            println!("{}", outcome.summary);
        }
        Ok(Err(failure)) => eprintln!("{}", failure.error),
        Err(e) => eprintln!("{e:#}"),
    }
}

/// An agent that hands back 4xx responses instead of erroring on them, since
/// a rejection carries the reason in its body and is a reply worth reading.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

/// Send one command and decode the reply. The outer error is the connection
/// failing. The inner one is darkside refusing the command.
fn send(
    agent: &ureq::Agent,
    base: &str,
    command: &Command,
) -> anyhow::Result<Result<Outcome, Failure>> {
    let (route, body) = control::request(command);
    let url = format!("{base}{route}");
    let mut response = match command {
        Command::Status => agent.get(&url).call(),
        _ => agent.post(&url).send_json(&body),
    }
    .with_context(|| format!("reaching {url}"))?;

    if response.status().is_success() {
        Ok(Ok(response
            .body_mut()
            .read_json()
            .context("decoding the reply")?))
    } else {
        Ok(Err(response
            .body_mut()
            .read_json()
            .context("decoding the rejection")?))
    }
}

fn print_help() {
    eprintln!("commands:");
    eprintln!("  mine [N]                  mine N blocks now (default 1)");
    eprintln!("  to <height>               mine forward until the tip reaches <height>");
    eprintln!("  next                      mine forward to the next network upgrade");
    eprintln!("  advance <upgrade>         mine forward to a named upgrade's activation");
    eprintln!("  fund <addr> <zec> [tsoi]  fabricate value to any address");
    eprintln!("  pause | resume            stop or start auto-mining");
    eprintln!("  tick <seconds|low..high>  set the auto-mining cadence");
    eprintln!("  reorg <depth>             fork <depth> blocks back and serve the fork");
    eprintln!("  reset                     rebuild the boot chain and serve it");
    eprintln!("  withhold on|off           accept submissions but hold them out of blocks");
    eprintln!("  status                    tip, pools, next upgrade, mempool, miner state");
    eprintln!("  help                      this list");
    eprintln!("  quit                      leave (Ctrl-D also works)");
    eprintln!();
    eprintln!("receivers  t transparent, s sapling, o orchard, i ironwood. several split the");
    eprintln!("           amount, remainder to the first typed. omit for the newest.");
    eprintln!("upgrades   {}", upgrade_names());
}
