//! A standalone fake news server, so the reader can be driven without a Usenet account.
//!
//! ```sh
//! cargo run -p nntp-testserver -- --port 1119
//! cargo run -p nntp-tui -- --server 127.0.0.1:1119
//! ```
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::process::ExitCode;

use nntp_testserver::{CapabilityProfile, Corpus, Quirks, ServerConfig, TestServer};

/// Parsed command-line arguments.
struct Args {
    port: u16,
    profile: CapabilityProfile,
    credentials: Option<(String, String)>,
    quirks: Quirks,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = match parse_args() {
        Ok(Some(args)) => args,
        // --help was asked for.
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nntp-testserver: {message}");
            eprintln!("try --help");
            return ExitCode::FAILURE;
        }
    };

    let mut config = ServerConfig::new()
        .capabilities(args.profile)
        .quirks(args.quirks);
    if let Some((username, password)) = &args.credentials {
        config = config.require_auth(username, password);
    }

    // Port 0 asks the operating system for a free port, which is only useful once the
    // chosen one is printed.
    let server = match start(args.port, config) {
        Ok(server) => server,
        Err(error) => {
            eprintln!(
                "nntp-testserver: could not listen on port {}: {error}",
                args.port
            );
            return ExitCode::FAILURE;
        }
    };

    println!(
        "nntp-testserver {} listening on {}",
        nntp_testserver::VERSION,
        server.authority()
    );
    println!("groups:");
    for group in Corpus::sample().groups() {
        let (low, high) = group.watermarks();
        println!(
            "  {:<24} {:>5} article(s)  [{low}..{high}]  {}",
            group.name,
            group.articles.len(),
            group.description
        );
    }
    println!("press Ctrl-C to stop");

    // Nothing else to do: the server owns its threads. Park until interrupted.
    loop {
        std::thread::park();
    }
}

fn start(port: u16, config: ServerConfig) -> std::io::Result<TestServer> {
    if port == 0 {
        return TestServer::with(Corpus::sample(), config);
    }
    TestServer::with_port(port, Corpus::sample(), config)
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut port = 1119u16;
    let mut profile = CapabilityProfile::Modern;
    let mut credentials = None;
    let mut quirks = Quirks::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("nntp-testserver {}", nntp_testserver::VERSION);
                return Ok(None);
            }
            "-p" | "--port" => {
                let value = args.next().ok_or("--port needs a value")?;
                port = value.parse().map_err(|_| format!("bad port: {value}"))?;
            }
            "--profile" => {
                let value = args.next().ok_or("--profile needs a value")?;
                profile = match value.as_str() {
                    "modern" => CapabilityProfile::Modern,
                    "no-over" => CapabilityProfile::NoOver,
                    "transit" => CapabilityProfile::Transit,
                    "legacy" => CapabilityProfile::Legacy,
                    other => {
                        return Err(format!(
                            "unknown profile {other:?}; expected modern, no-over, transit or legacy"
                        ));
                    }
                };
            }
            "--auth" => {
                let value = args.next().ok_or("--auth needs user:password")?;
                let (user, password) = value
                    .split_once(':')
                    .ok_or("--auth expects user:password")?;
                credentials = Some((user.to_owned(), password.to_owned()));
            }
            "--no-overview-fmt" => quirks.no_overview_fmt = true,
            "--reject-open-ranges" => quirks.reject_open_ended_ranges = true,
            "--bare-lf" => quirks.bare_lf = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }

    Ok(Some(Args {
        port,
        profile,
        credentials,
        quirks,
    }))
}

fn print_help() {
    println!(
        "\
nntp-testserver {version} — a fake NNTP server for driving a client offline

USAGE:
    nntp-testserver [OPTIONS]

OPTIONS:
    -p, --port <PORT>        Port to listen on, 0 for any free port [default: 1119]
        --profile <PROFILE>  Capability profile: modern, no-over, transit, legacy
                             [default: modern]
        --auth <USER:PASS>   Require authentication with these credentials
        --no-overview-fmt    Refuse LIST OVERVIEW.FMT, as some servers do
        --reject-open-ranges Refuse an OVER range with an open upper bound
        --bare-lf            Terminate lines with LF instead of CRLF
    -h, --help               Print this help
    -V, --version            Print the version

The corpus is fixed and deliberately awkward: sparse article numbers, RFC 2047
encoded subjects in both encodings, an unlabelled Latin-1 header, a body line
beginning with a dot, a Date header no parser can read, a moderated group and an
empty group.",
        version = nntp_testserver::VERSION
    );
}
