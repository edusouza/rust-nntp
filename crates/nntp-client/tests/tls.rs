//! TLS end-to-end tests, against a server using a certificate generated at run time.
//!
//! Certificate verification stays on throughout. The server generates a self-signed
//! authority, the test writes it to a file, and the client is told to trust that file —
//! so the code path under test is the same one used against a public server, rather than
//! a "skip verification" shortcut that would prove nothing.
//!
//! Untested TLS is the kind that fails on the day somebody actually needs it.

#![cfg(feature = "tls")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use nntp_client::{ClientError, ConnectOptions, Security, TlsOptions, connector};
use nntp_proto::{ArticleSpec, GroupName, Range};
use nntp_testserver::{Corpus, ServerConfig, TestServer, TlsMode};

/// Starts a TLS server and returns it together with the path of its CA certificate.
fn tls_server(mode: TlsMode, config: ServerConfig) -> (TestServer, std::path::PathBuf) {
    let server = TestServer::with_tls(Corpus::sample(), config, mode).expect("start TLS server");

    // A distinct file per server, so parallel tests do not overwrite each other's CA.
    let path = std::env::temp_dir().join(format!("nntp-test-ca-{}.pem", server.port()));
    server.write_ca_pem(&path).expect("write CA");

    (server, path)
}

/// Options that trust the server's generated CA and verify against the name in its
/// certificate, since the connection itself is made to 127.0.0.1.
fn options(server: &TestServer, ca: &std::path::Path, security: Security) -> ConnectOptions {
    ConnectOptions::new("127.0.0.1")
        .security(security)
        .port(server.port())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5))
        .write_timeout(Duration::from_secs(5))
        .tls_options(TlsOptions::new().extra_ca_file(ca).server_name("localhost"))
}

#[test]
fn reads_a_group_over_implicit_tls() {
    let (server, ca) = tls_server(TlsMode::Implicit, ServerConfig::new());
    let mut client =
        connector::connect(&options(&server, &ca, Security::ImplicitTls)).expect("connect");

    assert!(client.is_encrypted());
    client.handshake().expect("handshake");
    assert!(client.capabilities().has_reader());

    let summary = client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .unwrap();
    assert_eq!(summary.range(), Some((1, 3)));

    let overview = client.overview(Range::between(1, 3)).expect("overview");
    assert_eq!(overview.len(), 3);

    // A whole article over TLS, to prove the framing survives record boundaries.
    let article = client.article(ArticleSpec::Number(2)).expect("article");
    assert!(article.body_text().contains("\n.signature-like"));

    client.quit().expect("quit");
    let _ = std::fs::remove_file(&ca);
}

#[test]
fn upgrades_with_starttls() {
    let (server, ca) = tls_server(TlsMode::StartTls, ServerConfig::new());
    let mut client =
        connector::connect(&options(&server, &ca, Security::StartTls)).expect("connect");

    // connect() performed the upgrade, so the client is already encrypted and has re-read
    // its capabilities over the encrypted link.
    assert!(client.is_encrypted());
    assert!(!client.capabilities().is_empty());
    // STARTTLS is no longer advertised, because it has already happened.
    assert!(!client.capabilities().has_starttls());

    client.handshake().expect("handshake");
    client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .unwrap();
    assert_eq!(client.overview(Range::between(1, 3)).unwrap().len(), 3);

    client.quit().expect("quit");
    let _ = std::fs::remove_file(&ca);
}

#[test]
fn authenticates_over_tls_without_the_plaintext_opt_in() {
    let (server, ca) = tls_server(
        TlsMode::Implicit,
        ServerConfig::new().require_auth("bob", "hunter2"),
    );
    let mut client =
        connector::connect(&options(&server, &ca, Security::ImplicitTls)).expect("connect");
    client.handshake().expect("handshake");

    // The link is encrypted, so no opt-in is needed: this is the whole point of the
    // refusal on a plaintext connection.
    client
        .authenticate("bob", Some("hunter2"), false)
        .expect("authenticate");
    assert!(client.is_authenticated());

    client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .expect("group");
    let _ = std::fs::remove_file(&ca);
}

#[test]
fn refuses_a_certificate_it_cannot_verify() {
    // Same server, but the client is not told about its private CA.
    let (server, ca) = tls_server(TlsMode::Implicit, ServerConfig::new());

    let plain_options = ConnectOptions::new("127.0.0.1")
        .security(Security::ImplicitTls)
        .port(server.port())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5))
        .tls_options(TlsOptions::new().server_name("localhost"));

    let error = connector::connect(&plain_options).unwrap_err();
    assert!(
        matches!(&error, ClientError::Tls(_)),
        "expected a TLS error, got {error}"
    );
    // The message should say something about the certificate, not just "handshake failed".
    let message = error.to_string();
    assert!(
        message.contains("certificate")
            || message.contains("CaUsed")
            || message.contains("Unknown"),
        "unhelpful TLS error: {message}"
    );

    let _ = std::fs::remove_file(&ca);
}

#[test]
fn refuses_a_certificate_for_the_wrong_name() {
    // The certificate names "localhost"; verifying it against another name must fail even
    // though the CA is trusted.
    let (server, ca) = tls_server(TlsMode::Implicit, ServerConfig::new());

    let wrong_name = ConnectOptions::new("127.0.0.1")
        .security(Security::ImplicitTls)
        .port(server.port())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5))
        .tls_options(
            TlsOptions::new()
                .extra_ca_file(&ca)
                .server_name("not-localhost.invalid"),
        );

    let error = connector::connect(&wrong_name).unwrap_err();
    assert!(
        matches!(&error, ClientError::Tls(_)),
        "expected a TLS error, got {error}"
    );

    let _ = std::fs::remove_file(&ca);
}

#[test]
fn reports_a_server_that_does_not_offer_starttls() {
    // A plaintext server, asked for STARTTLS. The client should say so clearly rather
    // than sending a command it knows will fail.
    let server = TestServer::with(Corpus::sample(), ServerConfig::new()).expect("start");

    let starttls = ConnectOptions::new("127.0.0.1")
        .security(Security::StartTls)
        .port(server.port())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5));

    let error = connector::connect(&starttls).unwrap_err();
    match &error {
        ClientError::Tls(message) => assert!(
            message.contains("does not offer STARTTLS"),
            "unhelpful message: {message}"
        ),
        other => panic!("expected a TLS error, got {other}"),
    }
}

#[test]
fn plaintext_still_works_when_tls_is_compiled_in() {
    // A regression guard: adding TLS must not break the plaintext path.
    let server = TestServer::with(Corpus::sample(), ServerConfig::new()).expect("start");
    let mut client = connector::connect(
        &ConnectOptions::new("127.0.0.1")
            .port(server.port())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(5)),
    )
    .expect("connect");

    assert!(!client.is_encrypted());
    client.handshake().expect("handshake");
    client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .expect("group");
}

#[test]
fn changing_the_security_mode_picks_the_conventional_port() {
    assert_eq!(
        ConnectOptions::new("news.example.org").port,
        nntp_client::DEFAULT_PORT
    );
    assert_eq!(
        ConnectOptions::new("news.example.org")
            .security(Security::ImplicitTls)
            .port,
        nntp_client::DEFAULT_TLS_PORT
    );
    assert_eq!(
        ConnectOptions::new("news.example.org")
            .security(Security::StartTls)
            .port,
        nntp_client::DEFAULT_PORT
    );
    // An explicit port after the mode still wins.
    assert_eq!(
        ConnectOptions::new("news.example.org")
            .security(Security::ImplicitTls)
            .port(5563)
            .port,
        5563
    );
}
