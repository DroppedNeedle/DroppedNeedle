//! Runs the mock slskd and the mock MusicBrainz as a program, for browser
//! journeys against the server container. Each mock listens on the
//! address it is given and the program runs until SIGTERM or Ctrl-C.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::process::ExitCode;

use droppedneedle_testkit::musicbrainz::MockMusicBrainz;
use droppedneedle_testkit::slskd::MockSlskd;

const USAGE: &str = "usage: droppedneedle-mocks [--slskd ADDR] [--musicbrainz ADDR]

Serves each mock named on ADDR (for example 0.0.0.0:9001; port 0 picks
one) and prints one line per mock once it listens. Runs until SIGTERM or
Ctrl-C. MusicBrainz answers under /ws/2, slskd under /api/v0.";

struct Args {
    slskd: Option<SocketAddr>,
    musicbrainz: Option<SocketAddr>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        slskd: None,
        musicbrainz: None,
    };
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--slskd" => &mut parsed.slskd,
            "--musicbrainz" => &mut parsed.musicbrainz,
            other => return Err(format!("unknown argument {other}")),
        };
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} needs an address"))?;
        let addr = value
            .parse()
            .map_err(|error| format!("{flag} {value}: {error}"))?;
        *slot = Some(addr);
    }
    if parsed.slskd.is_none() && parsed.musicbrainz.is_none() {
        return Err("name at least one mock".to_owned());
    }
    Ok(parsed)
}

/// Start the mocks, which serve from their own tasks until the runtime
/// ends, then wait for the stop signal.
async fn run(args: Args) -> Result<(), String> {
    if let Some(addr) = args.slskd {
        let mock = MockSlskd::start_at(addr).await?;
        println!("slskd listening on {}", mock.base_url());
    }
    if let Some(addr) = args.musicbrainz {
        let mock = MockMusicBrainz::start_at(addr).await?;
        println!("musicbrainz listening on {}", mock.base_url());
    }
    stop_signal().await
}

/// Resolve on SIGTERM or Ctrl-C.
async fn stop_signal() -> Result<(), String> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| format!("cannot watch for SIGTERM: {error}"))?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.map_err(|error| error.to_string()),
        _ = terminate.recv() => Ok(()),
    }
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))
        .and_then(|runtime| runtime.block_on(run(args)));
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("droppedneedle-mocks: {error}");
            ExitCode::FAILURE
        }
    }
}
