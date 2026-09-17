//! Command-line interface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// A terminal news reader for Usenet.
#[derive(Debug, Parser)]
#[command(name = "nntp-tui", version, about, long_about = None)]
pub struct Cli {
    /// Path to the configuration file [default: the platform configuration directory]
    #[arg(long, short = 'c', global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Log filter directive, for example `info` or `nntp_client=trace`
    #[arg(long, global = true, value_name = "DIRECTIVE", env = "RUST_LOG")]
    pub log: Option<String>,

    /// Write logs to this file instead of standard error
    #[arg(long, global = true, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    /// What to do. With no subcommand, the terminal reader opens.
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// Whether this invocation will open the terminal reader.
    ///
    /// The reader owns the terminal, so logs have to go to a file rather than to standard
    /// error; this is how that decision is made before anything is initialised.
    pub fn opens_the_reader(&self) -> bool {
        matches!(self.command, None | Some(Command::Tui { .. }))
    }
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the terminal reader (the default with no subcommand)
    Tui {
        /// Connection details.
        #[command(flatten)]
        server: ServerArgs,
    },

    /// Probe a server and report what it supports
    ///
    /// Run this first against a new server. It reports the greeting, the capability list,
    /// whether reader mode or authentication is needed, which overview command works, the
    /// clock difference, and a sample group — everything needed to explain why the reader
    /// behaves the way it does against that server.
    Doctor {
        /// Connection details.
        #[command(flatten)]
        server: ServerArgs,

        /// Also try a group and fetch one article from it
        #[arg(long, value_name = "GROUP")]
        group: Option<String>,
    },

    /// List the newsgroups a server carries
    Groups {
        /// Connection details.
        #[command(flatten)]
        server: ServerArgs,

        /// Only groups matching this wildmat pattern, for example `comp.lang.*`
        #[arg(long, short = 'p', value_name = "WILDMAT")]
        pattern: Option<String>,

        /// Show descriptions instead of article counts
        #[arg(long, short = 'd')]
        descriptions: bool,

        /// Show at most this many groups
        #[arg(long, short = 'n', value_name = "COUNT")]
        limit: Option<usize>,
    },

    /// List the newest articles in a group
    Overview {
        /// Connection details.
        #[command(flatten)]
        server: ServerArgs,

        /// The group to list
        #[arg(value_name = "GROUP")]
        group: String,

        /// How many of the newest articles to show
        #[arg(long, short = 'n', value_name = "COUNT", default_value_t = 20)]
        count: u64,
    },

    /// Print one article
    Article {
        /// Connection details.
        #[command(flatten)]
        server: ServerArgs,

        /// An article number, which needs --group, or a message-id in angle brackets
        #[arg(value_name = "NUMBER|MESSAGE-ID")]
        article: String,

        /// The group the article number belongs to
        #[arg(long, short = 'g', value_name = "GROUP")]
        group: Option<String>,

        /// What to print
        #[arg(long, value_enum, default_value_t = ArticlePart::All)]
        part: ArticlePart,

        /// Print the article exactly as received, without decoding
        #[arg(long)]
        raw: bool,
    },

    /// Inspect the configuration
    Config {
        /// What to do with it.
        #[command(subcommand)]
        action: ConfigAction,
    },
}

/// Which part of an article to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ArticlePart {
    /// Headers and body.
    All,
    /// Headers only, using `HEAD`.
    Headers,
    /// Body only, using `BODY`.
    Body,
}

/// What to do with the configuration.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the path the configuration is read from
    Path,
    /// Print the configuration as it was parsed, with defaults filled in
    Show,
    /// Write a commented example configuration
    Init {
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
}

/// How to reach a server: either a name from the configuration file, or explicit flags.
///
/// The flags override the configured server field by field, so
/// `--server es --host localhost` uses the credentials from `es` against a local server —
/// which is exactly what one wants when reproducing a problem.
#[derive(Debug, Args, Clone, Default)]
pub struct ServerArgs {
    /// Use this server from the configuration file
    #[arg(long, short = 's', value_name = "NAME")]
    pub server: Option<String>,

    /// Host name or address, overriding the configuration
    #[arg(long, short = 'H', value_name = "HOST")]
    pub host: Option<String>,

    /// Port, overriding the configuration
    #[arg(long, short = 'P', value_name = "PORT")]
    pub port: Option<u16>,

    /// Connect with implicit TLS (port 563 unless --port says otherwise)
    #[arg(long, conflicts_with_all = ["starttls", "no_tls"])]
    pub tls: bool,

    /// Connect in the clear, then upgrade with STARTTLS
    #[arg(long, conflicts_with_all = ["tls", "no_tls"])]
    pub starttls: bool,

    /// Connect without encryption
    #[arg(long, conflicts_with_all = ["tls", "starttls"])]
    pub no_tls: bool,

    /// Username for AUTHINFO, overriding the configuration
    #[arg(long, short = 'u', value_name = "USER")]
    pub username: Option<String>,

    /// Read the password from this command's first line of output
    ///
    /// The command runs through a shell, which has to be quoted correctly for that
    /// shell. On Windows `cmd` also re-parses what `%VAR%` expands to, so `&`, `|`, `<`
    /// and `>` in a password are interpreted rather than passed on. Prefer
    /// --password-env, which has no shell in the path.
    #[arg(long, value_name = "COMMAND", conflicts_with = "password_env")]
    pub password_command: Option<String>,

    /// Read the password from this environment variable
    ///
    /// The password never passes through a shell, so no quoting can mangle it. An unset
    /// or empty variable is an error rather than an empty password.
    #[arg(long, value_name = "VARIABLE")]
    pub password_env: Option<String>,

    /// Send the password over an unencrypted connection
    ///
    /// The password crosses the network in clear text. Only for a server on your own
    /// machine, or one you do not mind losing the account of.
    #[arg(long)]
    pub allow_plaintext_auth: bool,

    /// Trust the certificate authorities in this PEM bundle, in addition to the built-in
    /// ones
    #[arg(long, value_name = "PATH")]
    pub ca_file: Option<PathBuf>,

    /// Verify the certificate against this name rather than the host connected to
    #[arg(long, value_name = "NAME")]
    pub tls_server_name: Option<String>,
}

impl ServerArgs {
    /// Whether any flag asks for a specific transport.
    pub fn security_override(&self) -> Option<crate::config::SecurityConfig> {
        use crate::config::SecurityConfig;
        if self.tls {
            Some(SecurityConfig::ImplicitTls)
        } else if self.starttls {
            Some(SecurityConfig::StartTls)
        } else if self.no_tls {
            Some(SecurityConfig::Plain)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_interface_definition_is_internally_consistent() {
        // Catches duplicated short flags, bad default values and conflicting argument
        // groups, all of which are otherwise runtime panics.
        Cli::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[test]
    fn no_subcommand_opens_the_reader() {
        let cli = parse(&["nntp-tui"]);
        assert!(cli.command.is_none());
        assert!(cli.opens_the_reader());
    }

    #[test]
    fn the_tui_subcommand_takes_connection_flags() {
        let cli = parse(&["nntp-tui", "tui", "--host", "127.0.0.1", "--no-tls"]);
        assert!(cli.opens_the_reader());
        match cli.command {
            Some(Command::Tui { server }) => {
                assert_eq!(server.host.as_deref(), Some("127.0.0.1"));
                assert!(server.no_tls);
            }
            other => panic!("expected Tui, got {other:?}"),
        }
    }

    #[test]
    fn the_command_line_subcommands_do_not_open_the_reader() {
        // They print to standard output, so their logs belong on standard error.
        for args in [
            vec!["nntp-tui", "groups"],
            vec!["nntp-tui", "config", "path"],
            vec!["nntp-tui", "doctor", "--host", "x"],
        ] {
            assert!(!parse(&args).opens_the_reader(), "{args:?}");
        }
    }

    #[test]
    fn parses_doctor_with_explicit_connection_flags() {
        let cli = parse(&["nntp-tui", "doctor", "--host", "news.example.org", "--tls"]);
        match cli.command.expect("a subcommand") {
            Command::Doctor { server, group } => {
                assert_eq!(server.host.as_deref(), Some("news.example.org"));
                assert!(server.tls);
                assert_eq!(
                    server.security_override(),
                    Some(crate::config::SecurityConfig::ImplicitTls)
                );
                assert!(group.is_none());
            }
            other => panic!("expected Doctor, got {other:?}"),
        }
    }

    #[test]
    fn the_two_password_sources_are_mutually_exclusive() {
        // Supplying both leaves it ambiguous which one is in force, and getting that wrong
        // means a login failure with no explanation.
        assert!(
            Cli::try_parse_from([
                "nntp-tui",
                "doctor",
                "--host",
                "x",
                "--password-command",
                "c",
                "--password-env",
                "E",
            ])
            .is_err()
        );

        for args in [
            vec!["nntp-tui", "doctor", "--host", "x", "--password-env", "E"],
            vec![
                "nntp-tui",
                "doctor",
                "--host",
                "x",
                "--password-command",
                "c",
            ],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn transport_flags_are_mutually_exclusive() {
        // Asking for TLS and no TLS at once has no sensible interpretation, and guessing
        // could mean sending a password in the clear.
        assert!(Cli::try_parse_from(["nntp-tui", "doctor", "--tls", "--no-tls"]).is_err());
        assert!(Cli::try_parse_from(["nntp-tui", "doctor", "--tls", "--starttls"]).is_err());
    }

    #[test]
    fn parses_groups_with_a_pattern_and_a_limit() {
        let cli = parse(&["nntp-tui", "groups", "-p", "comp.*", "-n", "5", "-d"]);
        match cli.command.expect("a subcommand") {
            Command::Groups {
                pattern,
                limit,
                descriptions,
                ..
            } => {
                assert_eq!(pattern.as_deref(), Some("comp.*"));
                assert_eq!(limit, Some(5));
                assert!(descriptions);
            }
            other => panic!("expected Groups, got {other:?}"),
        }
    }

    #[test]
    fn parses_overview_with_a_default_count() {
        let cli = parse(&["nntp-tui", "overview", "misc.test"]);
        match cli.command.expect("a subcommand") {
            Command::Overview { group, count, .. } => {
                assert_eq!(group, "misc.test");
                assert_eq!(count, 20);
            }
            other => panic!("expected Overview, got {other:?}"),
        }
    }

    #[test]
    fn parses_an_article_by_number_and_by_message_id() {
        let by_number = parse(&["nntp-tui", "article", "42", "--group", "misc.test"]);
        match by_number.command.expect("a subcommand") {
            Command::Article {
                article,
                group,
                part,
                raw,
                ..
            } => {
                assert_eq!(article, "42");
                assert_eq!(group.as_deref(), Some("misc.test"));
                assert_eq!(part, ArticlePart::All);
                assert!(!raw);
            }
            other => panic!("expected Article, got {other:?}"),
        }

        let by_id = parse(&["nntp-tui", "article", "<a@b>", "--part", "headers"]);
        match by_id.command.expect("a subcommand") {
            Command::Article { article, part, .. } => {
                assert_eq!(article, "<a@b>");
                assert_eq!(part, ArticlePart::Headers);
            }
            other => panic!("expected Article, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_config_subcommands() {
        assert!(matches!(
            parse(&["nntp-tui", "config", "path"]).command,
            Some(Command::Config {
                action: ConfigAction::Path
            })
        ));
        assert!(matches!(
            parse(&["nntp-tui", "config", "init", "--force"]).command,
            Some(Command::Config {
                action: ConfigAction::Init { force: true }
            })
        ));
    }

    #[test]
    fn global_options_work_before_or_after_the_subcommand() {
        let before = parse(&["nntp-tui", "--config", "/tmp/a.toml", "config", "path"]);
        let after = parse(&["nntp-tui", "config", "path", "--config", "/tmp/a.toml"]);
        assert_eq!(before.config, after.config);
        assert_eq!(
            before.config.as_deref(),
            Some(std::path::Path::new("/tmp/a.toml"))
        );
    }

    #[test]
    fn no_transport_flag_means_no_override() {
        let cli = parse(&["nntp-tui", "doctor", "--host", "x"]);
        match cli.command.expect("a subcommand") {
            Command::Doctor { server, .. } => assert_eq!(server.security_override(), None),
            other => panic!("expected Doctor, got {other:?}"),
        }
    }
}
