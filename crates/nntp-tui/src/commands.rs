//! The command-line subcommands.

use std::io::Write;

use anyhow::{Context as _, bail};
use nntp_client::{Client, Transport};
use nntp_proto::{ArticleSpec, GroupName, MessageId, Range, Wildmat};

use crate::cli::{ArticlePart, ConfigAction, ServerArgs};
use crate::config::Config;
use crate::readstate::ReadStore;
use crate::session;

/// Probes a server and reports what it supports.
///
/// This is the command to run against a new server, and the one to paste into a bug
/// report. Every line is something that changes how the reader behaves, so a surprising
/// line here explains a surprising behaviour there.
///
/// # Errors
///
/// Returns an error only if the connection itself fails. Individual probes that fail are
/// *reported*, not propagated: "this server has no `NEWNEWS`" is the answer, not a
/// failure.
pub fn doctor(config: &Config, args: &ServerArgs, group: Option<&str>) -> anyhow::Result<()> {
    let target = session::resolve(config, args)?;
    let out = &mut std::io::stdout().lock();

    writeln!(
        out,
        "server:      {} ({})",
        target.authority(),
        target.label
    )?;

    let options = target.server.connect_options(target.limits)?;
    writeln!(out, "transport:   {:?}", options.security)?;

    let started = std::time::Instant::now();
    let mut client = nntp_client::connector::connect(&options)
        .with_context(|| format!("connecting to {}", target.authority()))?;
    writeln!(out, "connected:   in {:?}", started.elapsed())?;
    writeln!(out, "encrypted:   {}", yes_no(client.is_encrypted()))?;
    writeln!(
        out,
        "greeting:    {} {}",
        client.greeting().code,
        client.greeting().text
    )?;
    writeln!(
        out,
        "posting:     {}",
        yes_no(client.greeting().posting_allowed)
    )?;

    client.handshake().context("negotiating")?;

    let capabilities = client.capabilities().clone();
    if capabilities.is_empty() {
        writeln!(
            out,
            "capabilities: none — this server predates RFC 3977, so the reader will use \
             the RFC 2980 command set (XOVER, XHDR)"
        )?;
    } else {
        writeln!(
            out,
            "implementation: {}",
            capabilities
                .implementation()
                .unwrap_or_else(|| "(not reported)".to_owned())
        )?;
        writeln!(out, "capabilities:")?;
        for entry in capabilities.entries() {
            if entry.args.is_empty() {
                writeln!(out, "  {}", entry.label)?;
            } else {
                writeln!(out, "  {} {}", entry.label, entry.args.join(" "))?;
            }
        }
    }

    writeln!(out, "reader mode: {}", yes_no(capabilities.has_reader()))?;
    writeln!(
        out,
        "overview:    {}",
        if capabilities.has_over() {
            if capabilities.over_accepts_message_id() {
                "OVER, including by message-id"
            } else {
                "OVER, by range only"
            }
        } else {
            "not advertised — the reader will try OVER and fall back to XOVER"
        }
    )?;
    writeln!(out, "HDR:         {}", yes_no(capabilities.has_hdr()))?;
    writeln!(out, "NEWNEWS:     {}", yes_no(capabilities.has_newnews()))?;
    writeln!(out, "STARTTLS:    {}", yes_no(capabilities.has_starttls()))?;
    writeln!(
        out,
        "AUTHINFO:    {}",
        if capabilities.has_authinfo_user() {
            "USER/PASS"
        } else if capabilities.sasl_mechanisms().is_empty() {
            "not advertised"
        } else {
            "SASL only — not supported by this reader yet"
        }
    )?;
    if !capabilities.sasl_mechanisms().is_empty() {
        writeln!(
            out,
            "  SASL mechanisms: {}",
            capabilities.sasl_mechanisms().join(", ")
        )?;
    }
    writeln!(
        out,
        "COMPRESS:    {}",
        yes_no(capabilities.has_compress_deflate())
    )?;

    // Authentication, if any is configured. Reported rather than fatal: knowing that the
    // credentials are wrong is the point of the exercise.
    match session::authenticate(&mut client, &target) {
        Ok(()) if client.is_authenticated() => writeln!(out, "auth:        accepted")?,
        Ok(()) => writeln!(out, "auth:        not attempted (no username configured)")?,
        Err(error) => writeln!(out, "auth:        FAILED — {error:#}")?,
    }

    match client.server_date() {
        Ok(date) => {
            let skew = chrono::Utc::now().signed_duration_since(date);
            writeln!(
                out,
                "server date: {} (clock differs from ours by {}s)",
                date.to_rfc3339(),
                skew.num_seconds()
            )?;
        }
        // NEWGROUPS and NEWNEWS are relative to the server's clock, so this matters.
        Err(error) => writeln!(out, "server date: unavailable — {error}")?,
    }

    match client.overview_format() {
        Ok(fmt) => {
            let names: Vec<&str> = fmt.fields().iter().map(|f| f.name.as_str()).collect();
            writeln!(out, "overview fmt: {}", names.join(", "))?;
            if !fmt.is_standard_prefix() {
                writeln!(
                    out,
                    "  warning: this layout does not start with the seven RFC 3977 \
                     fields; the reader will map fields by name"
                )?;
            }
        }
        Err(error) => writeln!(out, "overview fmt: unavailable — {error}")?,
    }

    if let Some(name) = group {
        probe_group(&mut client, name, out)?;
    }

    let _ = client.quit();
    writeln!(out, "\nall probes finished")?;
    Ok(())
}

/// Selects a group and fetches one article from it, reporting whatever happens.
fn probe_group(
    client: &mut Client<Transport>,
    name: &str,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(out, "\nprobing group {name}:")?;

    let group =
        GroupName::parse(name).with_context(|| format!("{name:?} is not a valid group name"))?;
    let summary = match client.select_group(&group) {
        Ok(summary) => summary,
        Err(error) => {
            writeln!(out, "  GROUP failed — {error}")?;
            return Ok(());
        }
    };

    writeln!(
        out,
        "  selected: {} articles, numbers {}..{}",
        summary.estimated_count, summary.low, summary.high
    )?;

    let Some((low, high)) = summary.range() else {
        writeln!(out, "  the group is empty; nothing more to probe")?;
        return Ok(());
    };

    let newest = Range::between(high.saturating_sub(4).max(low), high);
    match client.overview(newest) {
        Ok(result) => {
            writeln!(
                out,
                "  OVER {}: {} record(s), {} unparseable line(s)",
                newest.to_argument(),
                result.len(),
                result.skipped.len()
            )?;
            for record in result.entries.iter().take(3) {
                writeln!(
                    out,
                    "    {:>10}  {}  {}",
                    record.number,
                    record.date.map_or_else(
                        || format!("unparseable date {:?}", record.date_raw),
                        |date| date.to_rfc3339()
                    ),
                    truncate(&record.subject, 60)
                )?;
            }
            for line in result.skipped.iter().take(3) {
                writeln!(out, "    could not parse: {}", truncate(line, 70))?;
            }
        }
        Err(error) => writeln!(out, "  OVER failed — {error}")?,
    }

    match client.article(ArticleSpec::Number(high)) {
        Ok(article) => writeln!(
            out,
            "  ARTICLE {high}: {} header(s), {} body line(s), transfer encoding {:?}",
            article.headers.len(),
            article.line_count(),
            article.transfer_encoding()
        )?,
        Err(error) => writeln!(out, "  ARTICLE {high} failed — {error}")?,
    }

    Ok(())
}

/// Lists the groups a server carries.
///
/// # Errors
///
/// Returns an error if the connection fails or the server refuses `LIST`.
pub fn groups(
    config: &Config,
    args: &ServerArgs,
    pattern: Option<&str>,
    descriptions: bool,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    let target = session::resolve(config, args)?;
    let mut client = session::connect(&target)?;
    let out = &mut std::io::stdout().lock();

    let wildmat = pattern
        .map(Wildmat::parse)
        .transpose()
        .context("the pattern is not a valid wildmat")?;

    let mut shown = 0usize;
    let mut skipped = 0usize;

    if descriptions {
        let result = client.list_group_descriptions(wildmat.as_ref())?;
        skipped = result.skipped.len();
        for entry in result.entries {
            if limit.is_some_and(|limit| shown >= limit) {
                break;
            }
            writeln!(out, "{:<40} {}", entry.name, entry.description)?;
            shown += 1;
        }
    } else {
        // Streamed, because `LIST ACTIVE` against a full feed is several megabytes and
        // there is no reason to hold it all before printing the first line.
        client.list_groups_streaming(wildmat.as_ref(), |entry| match entry {
            Ok(entry) => {
                if limit.is_none_or(|limit| shown < limit) {
                    // The watermarks bound the article count from above; expiry and
                    // cancellation leave gaps, so "6 articles" in a group holding two
                    // would be a lie. LIST ACTIVE gives nothing better.
                    let _ = writeln!(
                        out,
                        "{:<40} {:>9}  [{}..{}]  {}",
                        entry.name,
                        format!("\u{2264}{}", entry.estimated_count()),
                        entry.low,
                        entry.high,
                        entry.status.describe()
                    );
                    shown += 1;
                }
            }
            Err(_) => skipped += 1,
        })?;
    }

    if skipped > 0 {
        writeln!(out, "\n{skipped} line(s) could not be parsed")?;
    }

    let _ = client.quit();
    Ok(())
}

/// Lists the newest articles in a group.
///
/// # Errors
///
/// Returns an error if the group does not exist or overview data cannot be fetched.
pub fn overview(config: &Config, args: &ServerArgs, group: &str, count: u64) -> anyhow::Result<()> {
    let target = session::resolve(config, args)?;
    let mut client = session::connect(&target)?;
    let out = &mut std::io::stdout().lock();

    let name =
        GroupName::parse(group).with_context(|| format!("{group:?} is not a valid group name"))?;
    let summary = client.select_group(&name)?;

    let Some((low, high)) = summary.range() else {
        writeln!(out, "{group} is empty")?;
        let _ = client.quit();
        return Ok(());
    };

    // Count back from the high watermark, not forward from the low one: the newest
    // articles are what anybody wants to see first, and a group can hold millions.
    let first = high.saturating_sub(count.saturating_sub(1)).max(low);
    let result = client.overview(Range::between(first, high))?;

    for record in &result.entries {
        writeln!(
            out,
            "{:>10}  {:<16}  {:<28}  {}",
            record.number,
            record.date.map_or_else(
                || "?".to_owned(),
                |date| date.format(&config.ui.date_format).to_string()
            ),
            truncate(&record.from, 28),
            truncate(&record.subject, 72)
        )?;
    }

    writeln!(
        out,
        "\n{} of {} articles in {group} (numbers {low}..{high})",
        result.len(),
        summary.estimated_count
    )?;
    if !result.skipped.is_empty() {
        writeln!(out, "{} line(s) could not be parsed", result.skipped.len())?;
    }

    let _ = client.quit();
    Ok(())
}

/// Prints one article.
///
/// # Errors
///
/// Returns an error if the article cannot be identified or fetched.
pub fn article(
    config: &Config,
    args: &ServerArgs,
    article: &str,
    group: Option<&str>,
    part: ArticlePart,
    raw: bool,
) -> anyhow::Result<()> {
    let target = session::resolve(config, args)?;
    let mut client = session::connect(&target)?;
    let out = &mut std::io::stdout().lock();

    let spec = parse_spec(article, group.is_some())?;

    if let Some(group) = group {
        let name = GroupName::parse(group)
            .with_context(|| format!("{group:?} is not a valid group name"))?;
        client.select_group(&name)?;
    }

    match part {
        ArticlePart::Body => {
            let body = client.body(spec)?;
            if raw {
                out.write_all(&body.to_bytes())?;
            } else {
                writeln!(out, "{}", body.to_text())?;
            }
        }
        ArticlePart::Headers => {
            let fetched = client.head(spec)?;
            print_headers(&fetched, raw, out)?;
        }
        ArticlePart::All => {
            let fetched = client.article(spec)?;
            print_headers(&fetched, raw, out)?;
            writeln!(out)?;
            if raw {
                out.write_all(&fetched.body_bytes())?;
                writeln!(out)?;
            } else {
                // The part a reader can read, not the raw body: `--raw` above is how to
                // get the boundaries and the base64. Attachments are named rather than
                // dumped, for the same reason they are in the terminal reader.
                let attachments = fetched.attachments();
                if !attachments.is_empty() {
                    for summary in &attachments {
                        writeln!(out, "[other part] {summary}")?;
                    }
                    writeln!(out)?;
                }

                let text = fetched.display_text();
                if text.is_empty() && !attachments.is_empty() {
                    // The same note the reader shows: an article that is only an
                    // attachment should say so rather than look like a failed fetch.
                    writeln!(out, "(no text in this article)")?;
                } else {
                    writeln!(out, "{text}")?;
                }
            }
        }
    }

    let _ = client.quit();
    Ok(())
}

fn print_headers(
    article: &nntp_proto::Article,
    raw: bool,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    for (name, value) in article.headers.iter() {
        if raw {
            out.write_all(name.as_str().as_bytes())?;
            out.write_all(b": ")?;
            out.write_all(value.as_bytes())?;
            out.write_all(b"\n")?;
        } else {
            writeln!(out, "{name}: {}", value.decoded())?;
        }
    }

    if !article.headers.malformed().is_empty() {
        writeln!(
            out,
            "\n({} header line(s) could not be parsed)",
            article.headers.malformed().len()
        )?;
    }

    Ok(())
}

/// Interprets an article argument as a number or a message-id.
fn parse_spec(argument: &str, has_group: bool) -> anyhow::Result<ArticleSpec> {
    if argument.starts_with('<') {
        let id = MessageId::parse(argument)
            .with_context(|| format!("{argument:?} is not a valid message-id"))?;
        return Ok(ArticleSpec::MessageId(id));
    }

    let number: u64 = argument.parse().with_context(|| {
        format!("{argument:?} is neither an article number nor a message-id in angle brackets")
    })?;

    if !has_group {
        bail!("an article number needs a group: pass --group <GROUP>");
    }

    Ok(ArticleSpec::Number(number))
}

/// Handles `nntp-tui config …`.
///
/// # Errors
///
/// Returns an error if the configuration path cannot be determined, or if writing the
/// example file fails.
pub fn config(
    action: &ConfigAction,
    explicit_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let out = &mut std::io::stdout().lock();

    let path = match explicit_path {
        Some(path) => path.to_path_buf(),
        None => Config::default_path()?,
    };

    match action {
        ConfigAction::Path => {
            writeln!(out, "configuration: {}", path.display())?;
            if !path.exists() {
                writeln!(
                    out,
                    "               (does not exist yet; `nntp-tui config init` writes an \
                     example there)"
                )?;
            }
            match crate::logging::default_log_path() {
                Ok(log) => writeln!(out, "log file:      {}", log.display())?,
                Err(error) => writeln!(out, "log file:      unavailable — {error}")?,
            }

            // Read state is per server, so the path depends on which one is in play.
            // Printing it for the server that would be used is more useful than printing
            // the directory and leaving the reader to work out the file name themselves.
            let config = Config::load(Some(&path)).unwrap_or_default();
            let server = config
                .server(None)
                .map(|(_, server)| server.host.clone())
                .unwrap_or_default();
            match ReadStore::default_path(&server) {
                Ok(newsrc) if server.is_empty() => writeln!(
                    out,
                    "read state:    {} (no server configured; the name comes from the \
                     server in use)",
                    newsrc.parent().unwrap_or(&newsrc).display()
                )?,
                Ok(newsrc) => writeln!(out, "read state:    {}", newsrc.display())?,
                Err(error) => writeln!(out, "read state:    unavailable — {error}")?,
            }
        }
        ConfigAction::Show => {
            let config = Config::load(Some(&path))?;
            write!(out, "{}", toml::to_string_pretty(&config)?)?;
        }
        ConfigAction::Init { force } => {
            if path.exists() && !force {
                bail!(
                    "{} already exists; pass --force to overwrite it",
                    path.display()
                );
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&path, Config::example_toml())
                .with_context(|| format!("writing {}", path.display()))?;
            writeln!(out, "wrote an example configuration to {}", path.display())?;
            writeln!(
                out,
                "edit it, then run `nntp-tui doctor` to check the server"
            )?;
        }
    }

    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Shortens `text` to `width` characters, marking the cut.
///
/// Counts characters rather than bytes so a multi-byte subject is not split mid-character.
fn truncate(text: &str, width: usize) -> String {
    let mut characters = text.chars();
    let head: String = characters.by_ref().take(width).collect();
    if characters.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_article_number_when_a_group_is_given() {
        assert_eq!(parse_spec("42", true).unwrap(), ArticleSpec::Number(42));
    }

    #[test]
    fn an_article_number_without_a_group_says_what_is_missing() {
        let error = parse_spec("42", false).unwrap_err().to_string();
        assert!(error.contains("--group"), "{error}");
    }

    #[test]
    fn parses_a_message_id_with_or_without_a_group() {
        for has_group in [true, false] {
            assert_eq!(
                parse_spec("<a@b>", has_group).unwrap(),
                ArticleSpec::MessageId(MessageId::parse("<a@b>").unwrap())
            );
        }
    }

    #[test]
    fn rejects_an_argument_that_is_neither() {
        let error = parse_spec("not-an-id", true).unwrap_err().to_string();
        assert!(error.contains("message-id"), "{error}");
        assert!(parse_spec("<unterminated", true).is_err());
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly-10", 10), "exactly-10");
        assert_eq!(truncate("0123456789x", 10), "0123456789…");
        // Four multi-byte characters are four characters, not twelve bytes.
        assert_eq!(truncate("日本語です", 4), "日本語で…");
        assert_eq!(truncate("", 4), "");
    }

    #[test]
    fn yes_no_is_unambiguous() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
    }
}
