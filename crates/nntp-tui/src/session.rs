//! Turning configuration plus command-line flags into a connected client.
//!
//! Kept separate from both the configuration and the commands because the merge rules are
//! the fiddly part: a flag overrides the configured server field by field, so
//! `--server es --host localhost` uses the credentials from `es` against a local server,
//! which is what one wants when reproducing a problem.

use anyhow::Context as _;
use nntp_client::{Client, Limits, Transport, connector};

use crate::cli::ServerArgs;
use crate::config::{Config, ServerConfig};

/// A resolved server: where it came from, and what to do with it.
#[derive(Debug, Clone)]
pub struct Target {
    /// A label for messages: the configured name, or `command line`.
    pub label: String,
    /// The merged server definition.
    pub server: ServerConfig,
    /// Response size limits from the configuration.
    pub limits: Limits,
}

impl Target {
    /// `host:port`, for messages.
    pub fn authority(&self) -> String {
        match self.server.port {
            Some(port) => format!("{}:{port}", self.server.host),
            None => {
                let port: nntp_client::Security = self.server.security.into();
                format!("{}:{}", self.server.host, port.default_port())
            }
        }
    }
}

/// Merges the configuration and the command-line flags.
///
/// # Errors
///
/// Returns an error if no server can be identified: neither a configured one nor a
/// `--host`.
pub fn resolve(config: &Config, args: &ServerArgs) -> anyhow::Result<Target> {
    let (label, mut server) = match config.server(args.server.as_deref()) {
        Ok((name, server)) => (name.to_owned(), server.clone()),
        // A --host on its own is enough; the configuration is then irrelevant.
        Err(error) => {
            if args.host.is_none() {
                return Err(error);
            }
            ("command line".to_owned(), ServerConfig::default())
        }
    };

    if let Some(host) = &args.host {
        server.host = host.clone();
        // A host given on the command line with no port means "use the default for the
        // transport", not "reuse the configured server's port", which would be a
        // surprising way to connect to the wrong place.
        server.port = None;
    }
    if let Some(port) = args.port {
        server.port = Some(port);
    }
    if let Some(security) = args.security_override() {
        server.security = security;
    }
    if let Some(from) = &args.from {
        server.from = Some(from.clone());
    }
    if let Some(username) = &args.username {
        server.username = Some(username.clone());
    }
    // Either password flag replaces whatever the configuration supplied, rather than
    // competing with it: a flag is a deliberate override, and leaving two sources in play
    // would make it unclear which one is in force.
    if let Some(command) = &args.password_command {
        server.password_command = Some(command.clone());
        server.password = None;
        server.password_env = None;
    }
    if let Some(variable) = &args.password_env {
        server.password_env = Some(variable.clone());
        server.password = None;
        server.password_command = None;
    }
    if args.allow_plaintext_auth {
        server.allow_plaintext_auth = true;
    }
    if let Some(path) = &args.ca_file {
        server.extra_ca_file = Some(path.clone());
    }
    if let Some(name) = &args.tls_server_name {
        server.tls_server_name = Some(name.clone());
    }

    Ok(Target {
        label,
        server,
        limits: config.limits.into(),
    })
}

/// Connects, negotiates, and authenticates if credentials are configured.
///
/// # Errors
///
/// Returns an error if the connection, negotiation or authentication fails. An
/// authentication failure carries the server's own message, which is usually the only
/// clue about *why*.
pub fn connect(target: &Target) -> anyhow::Result<Client<Transport>> {
    let options = target
        .server
        .connect_options(target.limits)
        .with_context(|| format!("server {:?}", target.label))?;

    let mut client = connector::connect(&options)
        .with_context(|| format!("connecting to {}", target.authority()))?;

    client
        .handshake()
        .with_context(|| format!("negotiating with {}", target.authority()))?;

    authenticate(&mut client, target)?;
    Ok(client)
}

/// Sends credentials if any are configured.
///
/// # Errors
///
/// Returns an error if the password cannot be obtained or the server rejects the
/// credentials. A refusal to send a password over a plaintext link is reported with the
/// flag that would allow it, so the user can make that choice knowingly.
pub fn authenticate(client: &mut Client<Transport>, target: &Target) -> anyhow::Result<()> {
    let Some(username) = &target.server.username else {
        return Ok(());
    };

    let password = target
        .server
        .resolve_password()
        .with_context(|| format!("obtaining the password for {:?}", target.label))?;

    match client.authenticate(
        username,
        password.as_deref(),
        target.server.allow_plaintext_auth,
    ) {
        Ok(()) => Ok(()),
        Err(nntp_client::ClientError::PlaintextAuthenticationRefused) => Err(anyhow::anyhow!(
            "refusing to send the password for {:?} over an unencrypted connection.\n\
             Use --tls (or --starttls) if the server supports it, or pass \
             --allow-plaintext-auth if you accept that the password crosses the network \
             in clear text.",
            target.label
        )),
        Err(error) => Err(anyhow::Error::new(error).context(format!(
            "authenticating as {username:?} with {}",
            target.authority()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecurityConfig;

    fn config(text: &str) -> Config {
        let config: Config = toml::from_str(text).expect("parse");
        config.validate().expect("validate");
        config
    }

    fn args() -> ServerArgs {
        ServerArgs {
            server: None,
            host: None,
            port: None,
            tls: false,
            starttls: false,
            no_tls: false,
            from: None,
            username: None,
            password_command: None,
            password_env: None,
            allow_plaintext_auth: false,
            ca_file: None,
            tls_server_name: None,
        }
    }

    #[test]
    fn the_from_flag_overrides_the_configured_identity() {
        // The case that made this necessary: a server given entirely on the command line
        // has no configured identity to post as, so without the flag the reader can read
        // from a fake server but cannot write to it.
        let config = Config::default();
        let target = resolve(
            &config,
            &ServerArgs {
                host: Some("127.0.0.1".to_owned()),
                port: Some(1119),
                no_tls: true,
                from: Some("A Tester <tester@example.org>".to_owned()),
                ..ServerArgs::default()
            },
        )
        .expect("resolve");

        assert_eq!(
            target.server.from.as_deref(),
            Some("A Tester <tester@example.org>")
        );
    }

    const CONFIGURED: &str = r#"
        [servers.es]
        host = "news.eternal-september.org"
        security = "implicit-tls"
        username = "bob"
        password_command = "pass show news"
    "#;

    #[test]
    fn uses_the_configured_server_when_no_flags_are_given() {
        let target = resolve(&config(CONFIGURED), &args()).unwrap();
        assert_eq!(target.label, "es");
        assert_eq!(target.server.host, "news.eternal-september.org");
        assert_eq!(target.authority(), "news.eternal-september.org:563");
    }

    #[test]
    fn a_host_flag_alone_works_without_any_configuration() {
        let target = resolve(
            &Config::default(),
            &ServerArgs {
                host: Some("127.0.0.1".to_owned()),
                port: Some(1119),
                no_tls: true,
                ..args()
            },
        )
        .unwrap();

        assert_eq!(target.label, "command line");
        assert_eq!(target.authority(), "127.0.0.1:1119");
        assert_eq!(target.server.security, SecurityConfig::Plain);
    }

    #[test]
    fn flags_override_the_configured_server_field_by_field() {
        // The point: keep the credentials, change where they are sent.
        let target = resolve(
            &config(CONFIGURED),
            &ServerArgs {
                server: Some("es".to_owned()),
                host: Some("127.0.0.1".to_owned()),
                port: Some(1119),
                no_tls: true,
                ..args()
            },
        )
        .unwrap();

        assert_eq!(target.server.host, "127.0.0.1");
        assert_eq!(target.server.security, SecurityConfig::Plain);
        // Credentials survived.
        assert_eq!(target.server.username.as_deref(), Some("bob"));
        assert_eq!(
            target.server.password_command.as_deref(),
            Some("pass show news")
        );
    }

    #[test]
    fn a_host_override_drops_the_configured_port() {
        // Reusing the configured port against a different host is a good way to connect
        // somewhere unintended.
        let target = resolve(
            &config("[servers.a]\nhost = \"a.example\"\nport = 5563\n"),
            &ServerArgs {
                host: Some("b.example".to_owned()),
                ..args()
            },
        )
        .unwrap();

        assert_eq!(target.server.port, None);
        assert_eq!(target.authority(), "b.example:563");
    }

    #[test]
    fn a_password_env_flag_replaces_every_configured_source() {
        let target = resolve(
            &config("[servers.a]\nhost=\"x\"\npassword_command=\"pass show news\"\n"),
            &ServerArgs {
                password_env: Some("NNTP_PASSWORD".to_owned()),
                ..args()
            },
        )
        .unwrap();

        assert_eq!(target.server.password_env.as_deref(), Some("NNTP_PASSWORD"));
        assert_eq!(target.server.password_command, None);
        assert_eq!(target.server.password, None);
    }

    #[test]
    fn a_password_command_flag_replaces_a_configured_literal_password() {
        let target = resolve(
            &config("[servers.a]\nhost=\"x\"\npassword=\"literal\"\n"),
            &ServerArgs {
                password_command: Some("echo other".to_owned()),
                ..args()
            },
        )
        .unwrap();

        assert_eq!(target.server.password, None);
        assert_eq!(
            target.server.password_command.as_deref(),
            Some("echo other")
        );
    }

    #[test]
    fn each_transport_flag_selects_its_transport() {
        for (flags, expected) in [
            (
                ServerArgs {
                    tls: true,
                    ..args()
                },
                SecurityConfig::ImplicitTls,
            ),
            (
                ServerArgs {
                    starttls: true,
                    ..args()
                },
                SecurityConfig::StartTls,
            ),
            (
                ServerArgs {
                    no_tls: true,
                    ..args()
                },
                SecurityConfig::Plain,
            ),
        ] {
            let target = resolve(
                &config("[servers.a]\nhost=\"x\"\nsecurity=\"starttls\"\n"),
                &flags,
            )
            .unwrap();
            assert_eq!(target.server.security, expected);
        }
    }

    #[test]
    fn no_server_and_no_host_is_an_error_that_explains_itself() {
        let error = resolve(&Config::default(), &args())
            .unwrap_err()
            .to_string();
        assert!(error.contains("--host"), "{error}");
    }

    #[test]
    fn passes_the_configured_limits_through() {
        let target = resolve(
            &config("[servers.a]\nhost=\"x\"\n[limits]\nmax_line_bytes = 4096\n"),
            &args(),
        )
        .unwrap();
        assert_eq!(target.limits.max_line_len, 4096);
    }
}
