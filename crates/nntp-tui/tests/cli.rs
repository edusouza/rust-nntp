//! End-to-end tests of the `nntp-tui` binary against the fake server.
//!
//! These run the real executable as a subprocess, so they cover argument parsing, the
//! configuration merge, connection set-up and output formatting together — the seams
//! where unit tests do not reach.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::process::{Command, Output};

use nntp_testserver::{Corpus, ServerConfig, TestServer};

/// Runs the binary, returning its output.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nntp-tui"))
        .args(args)
        // An empty configuration path keeps a developer's real configuration file out of
        // the test: without this the tests would behave differently on different machines.
        .env("RUST_LOG", "warn")
        .output()
        .expect("run nntp-tui")
}

/// Runs the binary against `server`, pointing it at the loopback port with no TLS.
///
/// The connection flags go *after* the subcommand, which also checks that clap accepts
/// global and flattened arguments in that position.
fn run_against(server: &TestServer, args: &[&str]) -> Output {
    let port = server.port().to_string();
    let mut full: Vec<&str> = args.to_vec();
    full.extend_from_slice(&[
        "--config",
        missing_config(),
        "--host",
        "127.0.0.1",
        "--port",
        &port,
        "--no-tls",
    ]);
    run(&full)
}

/// A path that deliberately does not exist, so the defaults are used.
fn missing_config() -> &'static str {
    "/nonexistent/nntp-tui-test-config.toml"
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn serve() -> TestServer {
    TestServer::with(Corpus::sample(), ServerConfig::new()).expect("start server")
}

#[test]
fn prints_help_and_version() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let text = stdout(&help);
    for expected in ["doctor", "groups", "overview", "article", "config"] {
        assert!(
            text.contains(expected),
            "help does not mention {expected}: {text}"
        );
    }

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert!(stdout(&version).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn doctor_reports_what_the_server_supports() {
    let server = serve();
    let output = run_against(&server, &["doctor", "--group", "misc.test"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);

    assert!(text.contains("greeting:"), "{text}");
    assert!(text.contains("encrypted:   no"), "{text}");
    assert!(text.contains("reader mode: yes"), "{text}");
    assert!(text.contains("OVER, including by message-id"), "{text}");
    assert!(text.contains("nntp-testserver"), "{text}");
    // The group probe ran.
    assert!(text.contains("probing group misc.test"), "{text}");
    assert!(text.contains("ARTICLE 3"), "{text}");
    assert!(text.contains("all probes finished"), "{text}");
}

#[test]
fn doctor_reports_a_missing_group_without_failing() {
    // "this server does not carry that group" is an answer, not an error.
    let server = serve();
    let output = run_against(&server, &["doctor", "--group", "no.such.group"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("GROUP failed"), "{text}");
    assert!(text.contains("all probes finished"), "{text}");
}

#[test]
fn groups_lists_every_group_in_aligned_columns() {
    let server = serve();
    let output = run_against(&server, &["groups"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);

    for group in ["misc.test", "comp.lang.rust", "de.comp.test", "empty.group"] {
        assert!(text.contains(group), "{group} missing from: {text}");
    }
    assert!(text.contains("moderated"), "{text}");

    // The watermark span is an upper bound on the article count, and says so.
    assert!(text.contains("≤6"), "{text}");

    // Columns line up: every line puts the bracketed range at the same offset.
    let offsets: Vec<usize> = text
        .lines()
        .filter(|line| line.contains('['))
        .map(|line| line.find('[').unwrap_or_default())
        .collect();
    assert!(offsets.len() >= 4, "{text}");
    assert!(
        offsets.windows(2).all(|pair| pair[0] == pair[1]),
        "columns are ragged, so Display is ignoring the field width: {offsets:?}\n{text}"
    );
}

#[test]
fn groups_filters_by_wildmat_and_limits_the_count() {
    let server = serve();

    let filtered = run_against(&server, &["groups", "--pattern", "comp.*"]);
    assert!(filtered.status.success(), "stderr: {}", stderr(&filtered));
    // The fake server does not implement wildmat matching, so this checks that the
    // pattern is accepted and sent, not that it filters.
    assert!(stdout(&filtered).contains("comp.lang.rust"));

    let limited = run_against(&server, &["groups", "-n", "2"]);
    assert_eq!(stdout(&limited).lines().count(), 2, "{}", stdout(&limited));
}

#[test]
fn groups_shows_descriptions_including_non_ascii() {
    let server = serve();
    let output = run_against(&server, &["groups", "--descriptions"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("For testing purposes only"), "{text}");
    assert!(text.contains("äöü"), "{text}");
}

#[test]
fn groups_rejects_an_invalid_wildmat() {
    let server = serve();
    let output = run_against(&server, &["groups", "--pattern", "has space"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("wildmat"), "{}", stderr(&output));
}

#[test]
fn overview_lists_the_newest_articles() {
    let server = serve();
    let output = run_against(&server, &["overview", "misc.test", "-n", "2"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);

    // Counting back from the high watermark: articles 2 and 3, not 1 and 2.
    assert!(text.contains("Re: A plain test article"), "{text}");
    assert!(
        text.contains("An article with an unreadable date"),
        "{text}"
    );
    assert!(!text.contains("  A plain test article"), "{text}");

    // An unparseable date is shown as "?" rather than hiding the article.
    assert!(text.contains('?'), "{text}");
}

#[test]
fn overview_reports_an_empty_group_plainly() {
    let server = serve();
    let output = run_against(&server, &["overview", "empty.group"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("is empty"), "{}", stdout(&output));
}

#[test]
fn overview_rejects_an_invalid_group_name() {
    let server = serve();
    let output = run_against(&server, &["overview", "not a group"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not a valid group name"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn article_decodes_headers_and_body_by_message_id() {
    let server = serve();
    let output = run_against(&server, &["article", "<b64@test.invalid>"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);

    // A base64 subject, a Q-encoded author and an unlabelled Latin-1 Organization.
    assert!(text.contains("Subject: Re: café and crates"), "{text}");
    assert!(text.contains("Åsa Lindqvist"), "{text}");
    assert!(text.contains("Organization: Café Central"), "{text}");
    // A quoted-printable body with a soft line break joined back together.
    assert!(text.contains("quoted-printable body: café."), "{text}");
    assert!(
        text.contains("break follows here and this continues"),
        "{text}"
    );
}

#[test]
fn article_by_number_needs_a_group_and_says_so() {
    let server = serve();
    let output = run_against(&server, &["article", "2"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("--group"), "{}", stderr(&output));
}

#[test]
fn article_by_number_works_with_a_group() {
    let server = serve();
    let output = run_against(&server, &["article", "2", "--group", "misc.test"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    // The dot-stuffed line survived the round trip with exactly one dot.
    assert!(
        text.contains("\n.signature-like line that starts with a dot"),
        "{text}"
    );
    assert!(!text.contains("..signature-like"), "{text}");
}

#[test]
fn article_can_print_only_the_headers_or_only_the_body() {
    let server = serve();

    let headers = run_against(
        &server,
        &["article", "<root@test.invalid>", "--part", "headers"],
    );
    let headers_text = stdout(&headers);
    assert!(
        headers_text.contains("Subject: A plain test article"),
        "{headers_text}"
    );
    assert!(
        !headers_text.contains("This is the first article."),
        "{headers_text}"
    );

    let body = run_against(
        &server,
        &["article", "<root@test.invalid>", "--part", "body"],
    );
    let body_text = stdout(&body);
    assert!(
        body_text.contains("This is the first article."),
        "{body_text}"
    );
    assert!(!body_text.contains("Subject:"), "{body_text}");
}

#[test]
fn article_raw_does_not_decode() {
    let server = serve();
    let output = run_against(
        &server,
        &[
            "article",
            "<b64@test.invalid>",
            "--part",
            "headers",
            "--raw",
        ],
    );

    let text = stdout(&output);
    // --raw means the encoded word is shown as it arrived.
    assert!(text.contains("=?UTF-8?B?"), "{text}");
    assert!(!text.contains("Re: café and crates"), "{text}");
}

#[test]
fn a_missing_article_fails_with_a_readable_message() {
    let server = serve();
    let output = run_against(&server, &["article", "<nope@test.invalid>"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no such article"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_refused_connection_fails_with_a_readable_message() {
    // Nothing is listening on port 1 of the loopback interface.
    let output = run(&[
        "doctor",
        "--config",
        missing_config(),
        "--host",
        "127.0.0.1",
        "--port",
        "1",
        "--no-tls",
    ]);

    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("connecting to 127.0.0.1:1"), "{message}");
}

#[test]
fn asking_for_a_server_with_no_configuration_explains_what_to_do() {
    let output = run(&["groups", "--config", missing_config()]);

    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("--host"), "{message}");
    assert!(message.contains("config init"), "{message}");
}

#[test]
fn config_init_then_show_round_trips() {
    let directory = std::env::temp_dir().join(format!("nntp-tui-cli-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let path = directory.join("config.toml");
    let path_str = path.to_string_lossy().into_owned();

    let init = run(&["config", "init", "--config", &path_str]);
    assert!(init.status.success(), "stderr: {}", stderr(&init));
    assert!(path.exists());
    assert!(stdout(&init).contains("doctor"), "{}", stdout(&init));

    // A second init refuses rather than overwriting somebody's configuration.
    let again = run(&["config", "init", "--config", &path_str]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("--force"), "{}", stderr(&again));

    let forced = run(&["config", "init", "--config", &path_str, "--force"]);
    assert!(forced.status.success());

    // The written file parses, and `show` prints it with the defaults filled in.
    let show = run(&["config", "show", "--config", &path_str]);
    assert!(show.status.success(), "stderr: {}", stderr(&show));
    let shown = stdout(&show);
    assert!(shown.contains("eternal-september"), "{shown}");
    assert!(shown.contains("implicit-tls"), "{shown}");

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn config_path_reports_every_path_the_program_uses() {
    let output = run(&["config", "path", "--config", missing_config()]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("configuration:"), "{text}");
    assert!(text.contains("does not exist yet"), "{text}");
    assert!(text.contains("log file:"), "{text}");
    // Read state is a file the user may want to inspect, hand-edit or copy from another
    // newsreader, and a path nothing printed is a path nobody can find.
    assert!(text.contains("read state:"), "{text}");
}

#[test]
fn config_path_names_the_read_state_file_after_the_configured_server() {
    // The file is per server, because article numbers are the server's own. With a server
    // configured, the exact file can be printed rather than just its directory.
    let path = std::env::temp_dir().join(format!("nntp-tui-newsrc-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        b"default_server = \"es\"\n\n[servers.es]\nhost = \"news.example.org\"\n",
    )
    .unwrap();

    let output = run(&["config", "path", "--config", &path.to_string_lossy()]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("news.example.org.newsrc"), "{text}");

    std::fs::remove_file(&path).ok();
}

#[test]
fn config_show_reports_a_broken_file_rather_than_ignoring_it() {
    let path = std::env::temp_dir().join(format!("nntp-tui-broken-{}.toml", std::process::id()));
    std::fs::write(&path, b"[servers.a]\nhost = \"x\"\nsecurty = \"plain\"\n").unwrap();

    let output = run(&["config", "show", "--config", &path.to_string_lossy()]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("parsing"), "{}", stderr(&output));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_configured_server_is_used_without_any_flags() {
    let server = serve();
    let directory =
        std::env::temp_dir().join(format!("nntp-tui-configured-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("config.toml");

    std::fs::write(
        &path,
        format!(
            "[servers.local]\nhost = \"127.0.0.1\"\nport = {}\nsecurity = \"plain\"\n",
            server.port()
        ),
    )
    .unwrap();

    let output = run(&["groups", "--config", &path.to_string_lossy()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("misc.test"), "{}", stdout(&output));

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_flag_overrides_a_configured_server() {
    // The configured port is wrong; the flag points at the real one. This is the
    // "reproduce it against my own server" case.
    let server = serve();
    let directory = std::env::temp_dir().join(format!("nntp-tui-override-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("config.toml");

    std::fs::write(
        &path,
        "[servers.local]\nhost = \"127.0.0.1\"\nport = 1\nsecurity = \"plain\"\n",
    )
    .unwrap();

    let output = run(&[
        "groups",
        "--config",
        &path.to_string_lossy(),
        "--port",
        &server.port().to_string(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("misc.test"));

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn credentials_are_not_sent_over_a_plaintext_link_without_consent() {
    let server = TestServer::with(
        Corpus::sample(),
        ServerConfig::new().require_auth("bob", "hunter2"),
    )
    .expect("start");

    let port = server.port().to_string();
    let refused = run(&[
        "groups",
        "--config",
        missing_config(),
        "--host",
        "127.0.0.1",
        "--port",
        &port,
        "--no-tls",
        "--username",
        "bob",
        "--password-command",
        "echo hunter2",
    ]);

    assert!(!refused.status.success());
    let message = stderr(&refused);
    assert!(message.contains("unencrypted"), "{message}");
    assert!(message.contains("--allow-plaintext-auth"), "{message}");
    // The password itself must not appear in the error.
    assert!(!message.contains("hunter2"), "{message}");

    // With consent, it works.
    let allowed = run(&[
        "groups",
        "--config",
        missing_config(),
        "--host",
        "127.0.0.1",
        "--port",
        &port,
        "--no-tls",
        "--username",
        "bob",
        "--password-command",
        "echo hunter2",
        "--allow-plaintext-auth",
    ]);
    assert!(allowed.status.success(), "stderr: {}", stderr(&allowed));
    assert!(stdout(&allowed).contains("misc.test"));
}
