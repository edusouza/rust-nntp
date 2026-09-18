//! Configuration: a TOML file, deserialised with serde.
//!
//! One hand-editable file, in the platform configuration directory. The shape and the
//! reasoning behind not using a database for this are in [ADR-0005].
//!
//! Every field has a default, so an empty file is valid and a missing file is not an
//! error — the reader still works with nothing but a `--host` on the command line.
//!
//! [ADR-0005]: https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0005-config-and-state-storage.md

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use nntp_client::{ConnectOptions, Limits, Security};
use serde::{Deserialize, Serialize};

/// The application name, used for the configuration and log directories.
pub const APP_NAME: &str = "nntp-tui";

/// The whole configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Which server to use when none is named on the command line.
    ///
    /// If unset and exactly one server is defined, that one is used.
    pub default_server: Option<String>,

    /// Named servers.
    pub servers: BTreeMap<String, ServerConfig>,

    /// Response size limits.
    pub limits: LimitsConfig,

    /// User interface preferences.
    pub ui: UiConfig,
}

/// One server definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Host name or address.
    pub host: String,

    /// Port. Defaults to the port conventional for [`Self::security`].
    pub port: Option<u16>,

    /// Whether and how to encrypt the connection.
    pub security: SecurityConfig,

    /// Who to post as: the `From` header of anything written on this server.
    ///
    /// Per server, because an identity is. There is deliberately no default — a guess
    /// assembled from the login name and the machine's host name is how articles end up
    /// signed `user@localhost`, and a reader that will not post until told who you are is
    /// better than one that posts as somebody who does not exist.
    pub from: Option<String>,

    /// Which groups to fetch from this server, as wildmat patterns.
    ///
    /// Empty — the default — means the whole group list, which on a full-feed server is
    /// several megabytes and tens of seconds before anything can be read. Most people read
    /// a handful of hierarchies, and saying so turns that fetch into a small one:
    ///
    /// ```toml
    /// subscriptions = ["comp.lang.*", "misc.test", "news.software.*"]
    /// ```
    ///
    /// The patterns are sent to the server as the wildmat argument of `LIST ACTIVE`
    /// (RFC 3977 §7.6.3), so the filtering happens there rather than here. Within a
    /// pattern `*` matches any run of characters and `?` exactly one; an entry beginning
    /// with `!` excludes what it matches, and the last pattern that matches a group
    /// decides — `["comp.*", "!comp.os.*"]` is every `comp` group but those.
    ///
    /// Per server, because a subscription is: the same person reads different groups on
    /// different machines, and an article number only means anything on the server that
    /// issued it.
    ///
    /// This is not a filter for the group list on screen — `/` does that, instantly, on
    /// what has already been fetched. This decides what is fetched at all, and `S` in the
    /// reader searches past it when something outside the subscriptions is wanted.
    pub subscriptions: Vec<String>,

    /// Username for `AUTHINFO USER`.
    pub username: Option<String>,

    /// Password for `AUTHINFO PASS`.
    ///
    /// Prefer [`Self::password_command`]: a password in a configuration file is a
    /// password in every backup of that file.
    pub password: Option<String>,

    /// A command whose first line of output is the password.
    ///
    /// Lets the password live in a password manager rather than in this file, for example
    /// `password_command = "pass show news/eternal-september"`.
    pub password_command: Option<String>,

    /// The name of an environment variable holding the password.
    ///
    /// The most robust of the three, because it is the only one with no shell in the path.
    /// `password_command` runs through `sh -c` or `cmd /C`, which brings two hazards: a
    /// password written literally into the command string has to be quoted correctly for
    /// that shell, and on Windows `cmd` expands `%VAR%` during parsing and then continues
    /// parsing the result, so a value containing `&`, `|`, `<` or `>` is interpreted
    /// rather than passed through. (A POSIX shell does not re-parse an expansion, so
    /// `sh -c 'printf %s "$VAR"'` is safe there.) Reading the variable directly avoids
    /// the question entirely.
    pub password_env: Option<String>,

    /// Allow `AUTHINFO PASS` over an unencrypted connection.
    ///
    /// Off by default, and worth leaving off: the password crosses the network in clear
    /// text.
    pub allow_plaintext_auth: bool,

    /// A PEM bundle of extra certificate authorities, for a server signed by a private CA.
    pub extra_ca_file: Option<PathBuf>,

    /// Verify the certificate against this name rather than [`Self::host`].
    pub tls_server_name: Option<String>,

    /// Seconds to wait for the TCP handshake.
    pub connect_timeout_secs: u64,

    /// Seconds to wait for data once connected.
    pub read_timeout_secs: u64,

    /// Seconds to wait for a write to complete.
    pub write_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: None,
            security: SecurityConfig::default(),
            from: None,
            subscriptions: Vec::new(),
            username: None,
            password: None,
            password_command: None,
            password_env: None,
            allow_plaintext_auth: false,
            extra_ca_file: None,
            tls_server_name: None,
            connect_timeout_secs: 20,
            read_timeout_secs: 60,
            write_timeout_secs: 30,
        }
    }
}

/// How a connection is protected, as written in the configuration file.
///
/// The spellings accept the aliases people actually type: `tls` for implicit TLS and
/// `starttls` — the command's own name — rather than only the kebab-cased variant names.
/// Rejecting `starttls` in a configuration file because the enum variant is `StartTls`
/// would be a needless papercut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityConfig {
    /// No encryption, port 119.
    #[serde(rename = "plain", alias = "none", alias = "cleartext")]
    Plain,
    /// TLS from the first byte, port 563. The default, because it should be.
    #[default]
    #[serde(rename = "implicit-tls", alias = "tls", alias = "implicit_tls")]
    ImplicitTls,
    /// Plaintext then `STARTTLS`, port 119.
    #[serde(rename = "starttls", alias = "start-tls", alias = "start_tls")]
    StartTls,
}

impl From<SecurityConfig> for Security {
    fn from(value: SecurityConfig) -> Self {
        match value {
            SecurityConfig::Plain => Self::Plain,
            SecurityConfig::ImplicitTls => Self::ImplicitTls,
            SecurityConfig::StartTls => Self::StartTls,
        }
    }
}

/// Response size limits, in the configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Largest single response line, in octets.
    pub max_line_bytes: usize,
    /// Largest multi-line block, in octets.
    pub max_block_bytes: usize,
    /// Largest number of lines in a multi-line block.
    pub max_block_lines: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        let defaults = Limits::DEFAULT;
        Self {
            max_line_bytes: defaults.max_line_len,
            max_block_bytes: defaults.max_block_bytes,
            max_block_lines: defaults.max_block_lines,
        }
    }
}

impl From<LimitsConfig> for Limits {
    fn from(value: LimitsConfig) -> Self {
        Self {
            max_line_len: value.max_line_bytes,
            max_block_bytes: value.max_block_bytes,
            max_block_lines: value.max_block_lines,
        }
    }
}

/// User interface preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiConfig {
    /// How many overview records to request per round trip.
    ///
    /// Smaller values make a large group appear sooner and cost more round trips.
    pub overview_chunk: u64,

    /// How many of a group's newest articles to load when it is opened.
    pub initial_articles: u64,

    /// `strftime` format for dates in the article list.
    pub date_format: String,

    /// Whether opening an article marks it read.
    ///
    /// On by default, because that is what the act of reading means and what every other
    /// newsreader does. Turning it off leaves `M` as the only way an article becomes read,
    /// which suits someone who skims a group and wants to decide deliberately.
    pub mark_read_on_open: bool,

    /// Whether the article list starts showing only unread articles.
    ///
    /// The `u` key toggles it either way; this is only the state the reader opens in.
    pub unread_only: bool,

    /// Whether the article list starts grouped into conversations.
    ///
    /// On by default: a newsreader that shows replies next to what they reply to is what
    /// `slrn`, `tin` and the rest have done for thirty years, and a flat list of a busy
    /// group is a list of fragments. The `t` key switches either way.
    pub threaded: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            overview_chunk: 500,
            initial_articles: 300,
            date_format: "%Y-%m-%d %H:%M".to_owned(),
            mark_read_on_open: true,
            unread_only: false,
            threaded: true,
        }
    }
}

impl Config {
    /// The path the configuration is read from, unless overridden.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform has no configuration directory.
    pub fn default_path() -> anyhow::Result<PathBuf> {
        let directories = directories::ProjectDirs::from("", "", APP_NAME)
            .context("this platform has no configuration directory")?;
        Ok(directories.config_dir().join("config.toml"))
    }

    /// Loads the configuration from `path`, or the default path if `path` is `None`.
    ///
    /// A missing file yields the defaults: the reader is usable with command-line
    /// arguments alone, and refusing to start without a configuration file would be
    /// gratuitous.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed, or if the parsed
    /// configuration is inconsistent. A configuration file that is present and wrong is
    /// always an error: silently ignoring it would be worse than refusing to start.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => match Self::default_path() {
                Ok(path) => path,
                // No config directory is not fatal; it just means no config file.
                Err(_) => return Ok(Self::default()),
            },
        };

        if !path.exists() {
            tracing::debug!(path = %path.display(), "no configuration file; using defaults");
            return Ok(Self::default());
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

        config
            .validate()
            .with_context(|| format!("in {}", path.display()))?;
        tracing::debug!(path = %path.display(), servers = config.servers.len(), "configuration loaded");
        Ok(config)
    }

    /// Checks the configuration for contradictions.
    ///
    /// # Errors
    ///
    /// Returns an error if a server has no host, if `default_server` names a server that
    /// does not exist, or if a server sets both `password` and `password_command`.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(name) = &self.default_server
            && !self.servers.contains_key(name)
        {
            bail!("default_server = {name:?} but no server by that name is defined");
        }

        for (name, server) in &self.servers {
            if server.host.trim().is_empty() {
                bail!("server {name:?} has no host");
            }
            let sources = [
                server.password.as_ref().map(|_| "password"),
                server.password_env.as_ref().map(|_| "password_env"),
                server.password_command.as_ref().map(|_| "password_command"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if sources.len() > 1 {
                bail!(
                    "server {name:?} sets {}; pick one so it is clear which applies",
                    sources.join(" and ")
                );
            }

            // A pattern that cannot be sent has to be reported now, by name. Dropping it
            // silently would hide groups the user asked for and leave nothing to look at
            // to find out why.
            for pattern in &server.subscriptions {
                if let Err(error) = nntp_proto::Wildmat::parse(pattern) {
                    bail!("server {name:?} has an unusable subscription {pattern:?}: {error}");
                }
            }
        }

        Ok(())
    }

    /// Looks up a server by name, or the default server if `name` is `None`.
    ///
    /// # Errors
    ///
    /// Returns an error naming the servers that *are* defined, which is more useful than
    /// "not found".
    pub fn server<'a>(&'a self, name: Option<&str>) -> anyhow::Result<(&'a str, &'a ServerConfig)> {
        if let Some(name) = name {
            // The key is returned rather than the argument, so the borrow is tied to the
            // configuration rather than to the caller's string.
            let (key, server) = self.servers.get_key_value(name).with_context(|| {
                format!(
                    "no server named {name:?}; defined servers: {}",
                    self.server_names()
                )
            })?;
            return Ok((key.as_str(), server));
        }

        if let Some(default) = &self.default_server {
            let server = self
                .servers
                .get(default)
                .context("default_server names a server that is not defined")?;
            return Ok((default.as_str(), server));
        }

        // Exactly one server needs no naming.
        let mut iter = self.servers.iter();
        match (iter.next(), iter.next()) {
            (Some((name, server)), None) => Ok((name.as_str(), server)),
            (None, _) => bail!(
                "no servers are configured; pass --host, or add a [servers.<name>] \
                 section to the configuration file (see `nntp-tui config init`)"
            ),
            _ => bail!(
                "several servers are configured; pass --server <name> or set \
                 default_server. Defined: {}",
                self.server_names()
            ),
        }
    }

    fn server_names(&self) -> String {
        if self.servers.is_empty() {
            return "(none)".to_owned();
        }
        self.servers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A commented example configuration, for `nntp-tui config init`.
    pub fn example_toml() -> String {
        // Written by hand rather than serialised, so it can carry comments. The values
        // match the defaults in this module; the test below keeps them honest.
        format!(
            r#"# Configuration for nntp-tui.
#
# Every setting has a default, so you can delete anything you do not need.

# Which server to use when --server is not given. Optional if only one is defined.
default_server = "eternal-september"

[servers.eternal-september]
host = "news.eternal-september.org"
# Defaults to 563 for implicit-tls and 119 otherwise.
port = 563
# plain | implicit-tls | starttls.  Prefer implicit-tls.
security = "implicit-tls"
username = "your-username"
# Three ways to supply the password; set at most one.
#   password_env     — read from an environment variable. The most robust: the password
#                      never passes through a shell, so no quoting can mangle it.
#   password_command — run a command and use its first line. Good with a password manager.
#   password         — written here in plain text. A password in a configuration file is a
#                      password in every backup of that file.
password_env = "NNTP_PASSWORD"
# password_command = "pass show news/eternal-september"
# password = "..."
# Only set this if you understand that the password crosses the network in clear text.
allow_plaintext_auth = false
# For a server signed by a private certificate authority.
# extra_ca_file = "/etc/ssl/private-ca.pem"
# If the certificate names a host other than the one you connect to.
# tls_server_name = "news.example.org"
connect_timeout_secs = 20
read_timeout_secs = 60
write_timeout_secs = 30
# Which groups to fetch. Empty or absent means the server's whole list, which on a
# full-feed server is several megabytes before you can read anything. These are wildmat
# patterns, filtered by the server: `*` is any run of characters, `?` is one, a leading
# `!` excludes, and the last pattern that matches a group wins. Press S in the reader to
# search past them.
# subscriptions = ["comp.lang.*", "misc.test", "news.software.readers"]

# A second server, to show that several can coexist.
# Who your articles are posted as. There is no default: a guess would put somebody
# else's address on your posts.
# from = "Your Name <you@example.org>"

[servers.local-test]
host = "127.0.0.1"
port = 1119
security = "plain"

[limits]
# Guards against a server that never terminates a line or a block.
max_line_bytes = {max_line_bytes}
max_block_bytes = {max_block_bytes}
max_block_lines = {max_block_lines}

[ui]
# Overview records fetched per round trip: smaller shows the list sooner.
overview_chunk = {overview_chunk}
# How many of a group's newest articles to load when it is opened.
initial_articles = {initial_articles}
date_format = "{date_format}"
# Whether opening an article marks it read. Off leaves `M` as the only way.
mark_read_on_open = {mark_read_on_open}
# Whether the article list starts showing only unread articles. `u` toggles it.
unread_only = {unread_only}
# Whether the article list starts grouped into conversations. `t` toggles it.
threaded = {threaded}
"#,
            max_line_bytes = LimitsConfig::default().max_line_bytes,
            max_block_bytes = LimitsConfig::default().max_block_bytes,
            max_block_lines = LimitsConfig::default().max_block_lines,
            overview_chunk = UiConfig::default().overview_chunk,
            initial_articles = UiConfig::default().initial_articles,
            date_format = UiConfig::default().date_format,
            mark_read_on_open = UiConfig::default().mark_read_on_open,
            unread_only = UiConfig::default().unread_only,
            threaded = UiConfig::default().threaded,
        )
    }
}

impl ServerConfig {
    /// Builds connection options from this server definition.
    ///
    /// # Errors
    ///
    /// Returns an error if the host is empty.
    pub fn connect_options(&self, limits: Limits) -> anyhow::Result<ConnectOptions> {
        if self.host.trim().is_empty() {
            bail!("no host configured");
        }

        let security: Security = self.security.into();
        let mut options = ConnectOptions::new(self.host.trim())
            .security(security)
            .connect_timeout(Duration::from_secs(self.connect_timeout_secs))
            .read_timeout(Duration::from_secs(self.read_timeout_secs))
            .write_timeout(Duration::from_secs(self.write_timeout_secs))
            .limits(limits);

        // security() resets the port to that mode's default, so an explicit port is
        // applied afterwards.
        if let Some(port) = self.port {
            options = options.port(port);
        }

        #[cfg(feature = "tls")]
        {
            let mut tls = nntp_client::TlsOptions::new();
            if let Some(path) = &self.extra_ca_file {
                tls = tls.extra_ca_file(path);
            }
            if let Some(name) = &self.tls_server_name {
                tls = tls.server_name(name);
            }
            options = options.tls_options(tls);
        }

        Ok(options)
    }

    /// The password, running [`Self::password_command`] if that is how it is supplied.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be run, exits non-zero, or produces no
    /// output. A password command that fails must not be mistaken for "no password":
    /// that would send an empty password to the server.
    pub fn resolve_password(&self) -> anyhow::Result<Option<String>> {
        self.resolve_password_with(|name| std::env::var(name).ok(), run_shell)
    }

    /// The password resolution logic, with its two sources of outside data injected.
    ///
    /// Taking the environment lookup and the shell as parameters keeps the precedence
    /// rules testable without mutating process-global state — which also means the tests
    /// can run in parallel, and that none of them needs `unsafe` to call
    /// `std::env::set_var`.
    fn resolve_password_with(
        &self,
        lookup_env: impl Fn(&str) -> Option<String>,
        run: impl Fn(&str) -> anyhow::Result<String>,
    ) -> anyhow::Result<Option<String>> {
        if let Some(password) = &self.password {
            return Ok(Some(password.clone()));
        }

        if let Some(variable) = &self.password_env {
            tracing::debug!(variable, "reading the password from the environment");
            // An unset variable must not be mistaken for an empty password: sending an
            // empty password to a server is worse than not trying.
            let value = lookup_env(variable).with_context(|| {
                format!("password_env names {variable:?}, which is not set in the environment")
            })?;
            // Only the line ending is trimmed. Leading and trailing spaces can be part of
            // a password, and stripping them silently would produce a login failure with
            // no explanation.
            let value = value.trim_end_matches(['\r', '\n']);
            if value.is_empty() {
                bail!("the environment variable {variable:?} is empty");
            }
            return Ok(Some(value.to_owned()));
        }

        let Some(command) = &self.password_command else {
            return Ok(None);
        };

        tracing::debug!("running password_command");
        let output =
            run(command).with_context(|| format!("running password_command {command:?}"))?;

        let first_line = output
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .with_context(|| format!("password_command {command:?} produced no output"))?;

        Ok(Some(first_line.to_owned()))
    }
}

/// Runs a command through the platform shell and returns its standard output.
fn run_shell(command: &str) -> anyhow::Result<String> {
    let output = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", command])
            .output()
    } else {
        std::process::Command::new("sh")
            .args(["-c", command])
            .output()
    }
    .context("could not start the shell")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("command failed with {}: {}", output.status, stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> anyhow::Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn an_empty_file_is_valid() {
        let config = parse("").unwrap();
        assert_eq!(config, Config::default());
        assert!(config.servers.is_empty());
        assert_eq!(config.ui.overview_chunk, 500);
    }

    #[test]
    fn parses_a_realistic_file() {
        let config = parse(
            r#"
            default_server = "es"

            [servers.es]
            host = "news.eternal-september.org"
            security = "implicit-tls"
            username = "bob"
            password_command = "pass show news"

            [servers.local]
            host = "127.0.0.1"
            port = 1119
            security = "plain"

            [ui]
            overview_chunk = 50
            "#,
        )
        .unwrap();

        assert_eq!(config.servers.len(), 2);
        assert_eq!(config.ui.overview_chunk, 50);
        // Unset UI fields keep their defaults.
        assert_eq!(config.ui.initial_articles, 300);

        let (name, server) = config.server(None).unwrap();
        assert_eq!(name, "es");
        assert_eq!(server.security, SecurityConfig::ImplicitTls);
        assert!(!server.allow_plaintext_auth);
    }

    #[test]
    fn accepts_the_spellings_people_type() {
        for (text, expected) in [
            ("plain", SecurityConfig::Plain),
            ("none", SecurityConfig::Plain),
            ("implicit-tls", SecurityConfig::ImplicitTls),
            ("tls", SecurityConfig::ImplicitTls),
            ("starttls", SecurityConfig::StartTls),
            ("start-tls", SecurityConfig::StartTls),
        ] {
            let config = parse(&format!("[servers.a]\nhost=\"x\"\nsecurity=\"{text}\"\n"))
                .unwrap_or_else(|error| panic!("{text:?} should parse: {error}"));
            assert_eq!(config.server(None).unwrap().1.security, expected, "{text}");
        }

        // A spelling nobody uses is still an error, with the accepted ones listed.
        let error = parse("[servers.a]\nhost=\"x\"\nsecurity=\"ssl\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("starttls") || error.contains("plain"),
            "{error}"
        );
    }

    #[test]
    fn serialises_back_to_the_canonical_spelling() {
        // `config show` must not print something the parser would reject.
        let config = parse("[servers.a]\nhost=\"x\"\nsecurity=\"tls\"\n").unwrap();
        let text = toml::to_string(&config).unwrap();
        assert!(text.contains("implicit-tls"), "{text}");
        assert_eq!(parse(&text).unwrap(), config);
    }

    #[test]
    fn defaults_to_tls_when_security_is_not_stated() {
        let config = parse("[servers.a]\nhost = \"x\"\n").unwrap();
        let (_, server) = config.server(None).unwrap();
        assert_eq!(server.security, SecurityConfig::ImplicitTls);
    }

    #[test]
    fn a_single_server_needs_no_naming() {
        let config = parse("[servers.only]\nhost = \"x\"\n").unwrap();
        assert_eq!(config.server(None).unwrap().0, "only");
    }

    #[test]
    fn several_servers_without_a_default_is_an_error_that_lists_them() {
        let config = parse("[servers.a]\nhost=\"x\"\n[servers.b]\nhost=\"y\"\n").unwrap();
        let error = config.server(None).unwrap_err().to_string();
        assert!(error.contains("--server"), "{error}");
        assert!(error.contains('a') && error.contains('b'), "{error}");
    }

    #[test]
    fn no_servers_points_at_the_way_out() {
        let error = Config::default().server(None).unwrap_err().to_string();
        assert!(error.contains("--host"), "{error}");
        assert!(error.contains("config init"), "{error}");
    }

    #[test]
    fn an_unknown_server_name_lists_the_known_ones() {
        let config = parse("[servers.a]\nhost=\"x\"\n").unwrap();
        let error = config.server(Some("b")).unwrap_err().to_string();
        assert!(error.contains("\"b\""), "{error}");
        assert!(error.contains('a'), "{error}");
    }

    #[test]
    fn rejects_a_default_server_that_does_not_exist() {
        let error = parse("default_server = \"ghost\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("ghost"), "{error}");
    }

    #[test]
    fn rejects_a_server_with_no_host() {
        let error = parse("[servers.a]\nhost = \"\"\n").unwrap_err().to_string();
        assert!(error.contains("no host"), "{error}");
    }

    #[test]
    fn keeps_subscriptions_in_the_order_they_were_written() {
        // Order is meaning: the last pattern that matches a group decides, so a list that
        // came back sorted or deduplicated would quietly change what it selects.
        let config = parse(
            "[servers.a]\nhost=\"x\"\nsubscriptions=[\"comp.*\", \"!comp.os.*\", \"misc.test\"]\n",
        )
        .expect("a valid file");
        let server = &config.servers["a"];
        assert_eq!(server.subscriptions, ["comp.*", "!comp.os.*", "misc.test"]);
    }

    #[test]
    fn rejects_a_subscription_that_cannot_be_sent() {
        // A pattern with a space cannot be a wildmat, so it would be dropped on the way to
        // the server and the groups it named would silently not appear.
        let error = parse("[servers.a]\nhost=\"x\"\nsubscriptions=[\"two words\"]\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("two words"), "{error}");
        assert!(error.contains("subscription"), "{error}");
    }

    #[test]
    fn no_subscriptions_means_the_whole_group_list() {
        let config = parse("[servers.a]\nhost=\"x\"\n").expect("a valid file");
        assert!(config.servers["a"].subscriptions.is_empty());
    }

    #[test]
    fn rejects_more_than_one_password_source() {
        for keys in [
            "password=\"p\"\npassword_command=\"c\"",
            "password=\"p\"\npassword_env=\"E\"",
            "password_env=\"E\"\npassword_command=\"c\"",
            "password=\"p\"\npassword_env=\"E\"\npassword_command=\"c\"",
        ] {
            let error = parse(&format!("[servers.a]\nhost=\"x\"\n{keys}\n"))
                .unwrap_err()
                .to_string();
            assert!(error.contains("pick one"), "{keys}: {error}");
        }

        // Exactly one of each is fine.
        for key in [
            "password=\"p\"",
            "password_env=\"E\"",
            "password_command=\"c\"",
        ] {
            parse(&format!("[servers.a]\nhost=\"x\"\n{key}\n"))
                .unwrap_or_else(|error| panic!("{key} should be valid: {error}"));
        }
    }

    #[test]
    fn rejects_an_unknown_key_rather_than_ignoring_it() {
        // A typo in a configuration file should be reported, not silently dropped.
        let error = parse("[servers.a]\nhost=\"x\"\nsecurty=\"plain\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("secur"), "{error}");
    }

    #[test]
    fn security_picks_the_conventional_port_unless_one_is_given() {
        let tls = ServerConfig {
            host: "x".to_owned(),
            security: SecurityConfig::ImplicitTls,
            ..ServerConfig::default()
        };
        assert_eq!(
            tls.connect_options(Limits::DEFAULT).unwrap().port,
            nntp_client::DEFAULT_TLS_PORT
        );

        let plain = ServerConfig {
            host: "x".to_owned(),
            security: SecurityConfig::Plain,
            ..ServerConfig::default()
        };
        assert_eq!(
            plain.connect_options(Limits::DEFAULT).unwrap().port,
            nntp_client::DEFAULT_PORT
        );

        let explicit = ServerConfig {
            host: "x".to_owned(),
            security: SecurityConfig::ImplicitTls,
            port: Some(5563),
            ..ServerConfig::default()
        };
        assert_eq!(
            explicit.connect_options(Limits::DEFAULT).unwrap().port,
            5563
        );
    }

    #[test]
    fn connect_options_refuse_an_empty_host() {
        assert!(
            ServerConfig::default()
                .connect_options(Limits::DEFAULT)
                .is_err()
        );
    }

    #[test]
    fn limits_round_trip_through_the_config_types() {
        let limits: Limits = LimitsConfig::default().into();
        assert_eq!(limits, Limits::DEFAULT);
    }

    #[test]
    fn resolves_a_password_from_a_command() {
        let server = ServerConfig {
            host: "x".to_owned(),
            password_command: Some(if cfg!(windows) {
                "echo hunter2".to_owned()
            } else {
                "printf 'hunter2\\nignored\\n'".to_owned()
            }),
            ..ServerConfig::default()
        };
        assert_eq!(
            server.resolve_password().unwrap().as_deref(),
            Some("hunter2")
        );
    }

    /// An environment that contains exactly one variable.
    fn env_with(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |asked| (asked == name).then(|| value.to_owned())
    }

    /// A shell that always fails, for the paths that must not reach it.
    fn no_shell(command: &str) -> anyhow::Result<String> {
        panic!("the shell should not have been used, but got {command:?}")
    }

    fn server_with(server: ServerConfig) -> ServerConfig {
        ServerConfig {
            host: "x".to_owned(),
            ..server
        }
    }

    #[test]
    fn resolves_a_password_from_an_environment_variable() {
        let server = server_with(ServerConfig {
            password_env: Some("NNTP_PASSWORD".to_owned()),
            ..ServerConfig::default()
        });

        assert_eq!(
            server
                .resolve_password_with(env_with("NNTP_PASSWORD", "hunter2"), no_shell)
                .unwrap()
                .as_deref(),
            Some("hunter2")
        );
    }

    #[test]
    fn a_password_from_the_environment_arrives_byte_for_byte() {
        // Every character here is special to some shell, which is the reason to have a
        // path with no shell in it: on Windows `cmd` expands `%VAR%` and then keeps
        // parsing the result, so `&`, `|`, `<` and `>` are interpreted rather than passed
        // on. This test asserts only what it can: that nothing alters the value.
        const AWKWARD: &str = r#"p&ss|w<o>r^d%$ "quoted" 'single' `tick`"#;

        let server = server_with(ServerConfig {
            password_env: Some("P".to_owned()),
            ..ServerConfig::default()
        });

        assert_eq!(
            server
                .resolve_password_with(env_with("P", AWKWARD), no_shell)
                .unwrap()
                .as_deref(),
            Some(AWKWARD)
        );
    }

    #[test]
    fn a_password_from_the_environment_keeps_its_spaces_but_drops_a_trailing_newline() {
        let server = server_with(ServerConfig {
            password_env: Some("P".to_owned()),
            ..ServerConfig::default()
        });

        // Leading and inner spaces are part of the password; only the line ending goes.
        assert_eq!(
            server
                .resolve_password_with(env_with("P", "  spaced pass  \r\n"), no_shell)
                .unwrap()
                .as_deref(),
            Some("  spaced pass  ")
        );
    }

    #[test]
    fn an_unset_or_empty_environment_variable_is_an_error_not_an_empty_password() {
        // Sending an empty password to a server is worse than not trying.
        let server = server_with(ServerConfig {
            password_env: Some("MISSING".to_owned()),
            ..ServerConfig::default()
        });

        let unset = server
            .resolve_password_with(|_| None, no_shell)
            .unwrap_err()
            .to_string();
        assert!(unset.contains("not set in the environment"), "{unset}");

        let empty = server
            .resolve_password_with(env_with("MISSING", ""), no_shell)
            .unwrap_err()
            .to_string();
        assert!(empty.contains("empty"), "{empty}");

        // A variable holding nothing but a line ending is empty too.
        let blank = server
            .resolve_password_with(env_with("MISSING", "\n"), no_shell)
            .unwrap_err()
            .to_string();
        assert!(blank.contains("empty"), "{blank}");
    }

    #[test]
    fn a_literal_password_wins_over_the_environment_and_the_shell() {
        let server = server_with(ServerConfig {
            password: Some("literal".to_owned()),
            password_env: Some("P".to_owned()),
            password_command: Some("should not run".to_owned()),
            ..ServerConfig::default()
        });

        assert_eq!(
            server
                .resolve_password_with(env_with("P", "from-env"), no_shell)
                .unwrap()
                .as_deref(),
            Some("literal")
        );
    }

    #[test]
    fn the_environment_wins_over_the_shell() {
        let server = server_with(ServerConfig {
            password_env: Some("P".to_owned()),
            password_command: Some("should not run".to_owned()),
            ..ServerConfig::default()
        });

        assert_eq!(
            server
                .resolve_password_with(env_with("P", "from-env"), no_shell)
                .unwrap()
                .as_deref(),
            Some("from-env")
        );
    }

    #[test]
    fn a_failing_password_command_is_an_error_not_an_empty_password() {
        let server = ServerConfig {
            host: "x".to_owned(),
            password_command: Some("exit 3".to_owned()),
            ..ServerConfig::default()
        };
        assert!(server.resolve_password().is_err());
    }

    #[test]
    fn a_silent_password_command_is_an_error() {
        let server = ServerConfig {
            host: "x".to_owned(),
            password_command: Some("true".to_owned()),
            ..ServerConfig::default()
        };
        assert!(server.resolve_password().is_err());
    }

    #[test]
    fn a_literal_password_is_used_as_is() {
        let server = ServerConfig {
            host: "x".to_owned(),
            password: Some("literal".to_owned()),
            ..ServerConfig::default()
        };
        assert_eq!(
            server.resolve_password().unwrap().as_deref(),
            Some("literal")
        );
    }

    #[test]
    fn no_password_configured_yields_none() {
        let server = ServerConfig {
            host: "x".to_owned(),
            ..ServerConfig::default()
        };
        assert_eq!(server.resolve_password().unwrap(), None);
    }

    #[test]
    fn the_example_configuration_is_valid_and_matches_the_defaults() {
        // The example is hand-written so it can carry comments, which means it can drift
        // from the defaults it claims to show. This test is what stops that.
        let config = parse(&Config::example_toml()).unwrap();
        assert_eq!(config.limits, LimitsConfig::default());
        assert_eq!(config.ui, UiConfig::default());
        assert_eq!(config.servers.len(), 2);
        assert_eq!(config.server(None).unwrap().0, "eternal-september");
    }

    #[test]
    fn a_missing_file_yields_the_defaults() {
        let path = std::env::temp_dir().join("nntp-tui-does-not-exist.toml");
        let _ = std::fs::remove_file(&path);
        assert_eq!(Config::load(Some(&path)).unwrap(), Config::default());
    }

    #[test]
    fn a_present_but_broken_file_is_an_error() {
        let path = std::env::temp_dir().join("nntp-tui-broken-config.toml");
        std::fs::write(&path, b"this is not toml {{{").unwrap();
        let error = Config::load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("parsing"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_config_that_round_trips_through_serialisation_is_unchanged() {
        let config = parse(&Config::example_toml()).unwrap();
        let text = toml::to_string(&config).unwrap();
        assert_eq!(parse(&text).unwrap(), config);
    }
}
