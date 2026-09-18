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

use nntp_testserver::{CapabilityProfile, Corpus, Quirks, ServerConfig, TestServer, TlsMode};

/// Parsed command-line arguments.
struct Args {
    port: u16,
    /// Whether to serve the MIME group as well.
    mime: bool,
    profile: CapabilityProfile,
    credentials: Option<(String, String)>,
    quirks: Quirks,
    tls: TlsMode,
    ca_out: Option<std::path::PathBuf>,
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
        // A person is at the other end of this one. The library default is thirty seconds
        // so that an abandoned connection cannot hold a test suite's shutdown, and reading
        // a single article takes longer than that — against this binary it dropped the
        // connection between one keystroke and the next.
        .idle_timeout(std::time::Duration::from_secs(30 * 60))
        .quirks(args.quirks.clone());
    if let Some((username, password)) = &args.credentials {
        config = config.require_auth(username, password);
    }

    // Port 0 asks the operating system for a free port, which is only useful once the
    // chosen one is printed.
    let server = match start(&args, config) {
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
    match args.tls {
        TlsMode::Disabled => println!("transport: plaintext"),
        TlsMode::Implicit => println!("transport: implicit TLS"),
        TlsMode::StartTls => println!("transport: plaintext until STARTTLS"),
    }
    if let Some(ca) = &args.ca_out {
        match server.write_ca_pem(ca) {
            Ok(()) => println!(
                "certificate authority written to {}; point a client at it and verify \
                 against the name \"localhost\"",
                ca.display()
            ),
            Err(error) => eprintln!("could not write {}: {error}", ca.display()),
        }
    } else if args.tls != TlsMode::Disabled {
        println!(
            "note: the certificate is generated fresh at start-up and is self-signed; \
             pass --ca-out <PATH> to write it out so a client can trust it"
        );
    }
    println!("groups:");
    for group in corpus_for(&args).groups() {
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

fn start(args: &Args, config: ServerConfig) -> std::io::Result<TestServer> {
    let corpus = corpus_for(args);

    if args.tls != TlsMode::Disabled {
        // TLS servers always take an ephemeral port: the generated certificate has to be
        // written out anyway, so the port is printed with it.
        return TestServer::with_tls(corpus, config, args.tls);
    }
    if args.port == 0 {
        return TestServer::with(corpus, config);
    }
    TestServer::with_port(args.port, corpus, config)
}

fn corpus_for(args: &Args) -> Corpus {
    if args.mime {
        Corpus::sample_with_mime()
    } else {
        Corpus::sample()
    }
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut port = 1119u16;
    let mut profile = CapabilityProfile::Modern;
    let mut credentials = None;
    let mut quirks = Quirks::default();
    let mut tls = TlsMode::Disabled;
    let mut ca_out = None;
    let mut mime = false;

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
            "--tls" => tls = TlsMode::Implicit,
            "--starttls" => tls = TlsMode::StartTls,
            "--ca-out" => {
                let value = args.next().ok_or("--ca-out needs a path")?;
                ca_out = Some(std::path::PathBuf::from(value));
            }
            "--mime" => mime = true,
            "--line-delay" => {
                let value = args.next().ok_or("--line-delay needs milliseconds")?;
                let millis: u64 = value
                    .parse()
                    .map_err(|_| format!("bad millisecond count: {value}"))?;
                quirks.line_delay = (millis > 0).then(|| std::time::Duration::from_millis(millis));
            }
            "--no-overview-fmt" => quirks.no_overview_fmt = true,
            "--reject-open-ranges" => quirks.reject_open_ended_ranges = true,
            "--bare-lf" => quirks.bare_lf = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }

    Ok(Some(Args {
        port,
        mime,
        profile,
        credentials,
        quirks,
        tls,
        ca_out,
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
        --tls                Serve implicit TLS with a certificate generated at start-up
        --starttls           Serve plaintext, advertising and accepting STARTTLS
        --ca-out <PATH>      Write the generated certificate authority here, so a
                             client can be told to trust it
        --mime               Also serve news.software.readers: a mail-to-news gateway
                             multipart, a format=flowed article, and an article that is
                             nothing but an attachment
        --line-delay <MS>    Pause this long before each line of a multi-line block, so a
                             response takes long enough to be worth cancelling
        --no-overview-fmt    Refuse LIST OVERVIEW.FMT, as some servers do
        --reject-open-ranges Refuse an OVER range with an open upper bound
        --bare-lf            Terminate lines with LF instead of CRLF
    -h, --help               Print this help
    -V, --version            Print the version

The corpus is fixed and deliberately awkward: sparse article numbers, RFC 2047
encoded subjects in both encodings, an unlabelled Latin-1 header, a body line
beginning with a dot, a Date header no parser can read, a moderated group and an
empty group. --mime adds a group of the MIME traffic a reader has to survive.",
        version = nntp_testserver::VERSION
    );
}
