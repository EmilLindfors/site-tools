//! Publish the next confirmed revision. Runs on mail.lindfors.no as the `publisher`
//! account, from cron, against a clone of the site repo it can push to with a deploy
//! key.
//!
//! What to publish is in the writing desk's database: an article whose approval is
//! confirmed names the revision, its content digest and the instant it goes out. The
//! publisher reads that with a read-only role, and until 2026-09-29 it read a `queue/`
//! directory in the private lindfors-services repo instead, filled by a Unix-socket
//! gateway the desk called; the database is the one place the decision was ever made,
//! so the queue, the gateway and the second deploy key are gone. A run:
//!
//! - *Is anything due?* The earliest confirmed approval whose `publish_at` has passed,
//!   validated for that exact revision and not yet taken on. Nothing else counts:
//!   there is no weekly slot and no cap, because a time is chosen for every post.
//! - *Take it on.* Under the article's own advisory lock (the one the desk holds while
//!   it applies a command), read the article once more, check the approval still
//!   stands, and write the receipt. From that receipt on the desk refuses to withdraw.
//! - *The sequence.* Export the revision and its assets into a bundle, reset the site
//!   clone to its remote branch, move the bundle in, write today's date into its
//!   frontmatter and drop `draft`, generate the derived files the build would, commit,
//!   push. Then wait for the page to answer 200 and, if the approval says so, hand the
//!   slug to the newsletter binary over loopback. Every step fails closed: a failed
//!   push mails nobody, and the next run starts from the remote again. A slug the site
//!   already carries as a dated post is never published twice.
//!
//! The receipt is the record. The desk reads it back (`publishing`, `deploying`,
//! `published`, `needs attention`) and shows it; a run that dies mid-way is finished
//! by the next one from the receipt, and a newsletter send that may have started is
//! never repeated without a person.
//!
//! `date` is assigned here, not by the author. Series order is the date, so it is
//! publish order, and two posts in a series can never share one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use sha2::{Digest, Sha256};

use crate::{frontmatter, markdown, newsletter, og, pdf, speech};

const DEFAULT_CONFIG: &str = "/etc/lindfors-publisher.toml";
const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// The desk's database, as the `writing_publisher` role. Not in the config file: it
/// is a credential, and `sec exec host/publisher` puts it in the environment.
const DATABASE_ENV: &str = "PUBLISHER_DATABASE_URL";

pub struct Config {
    pub repo: PathBuf,
    /// The desk's content-addressed asset store, readable by the publisher.
    pub assets: PathBuf,
    /// Where a revision is laid out as a page bundle before it moves into the site.
    pub export: PathBuf,
    /// One JSON per slug the publisher has taken on. Outside the resettable clone,
    /// readable by the desk.
    pub receipts: PathBuf,
    pub timezone: Tz,
    pub site_url: String,
    /// The program that sends an issue, with the slug appended. On the host this is
    /// `sudo /opt/lindfors-newsletter/send-issue`, the one command the publisher may
    /// run as root, which sources the service's environment and calls its `send`.
    pub send_command: Vec<String>,
    pub wait_minutes: u64,
    pub remote: String,
    pub branch: String,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let table: toml::Table = text.parse().map_err(|e| format!("config: {e}"))?;
        let s = |key: &str, default: &str| -> String {
            table.get(key).and_then(|v| v.as_str()).unwrap_or(default).to_string()
        };
        let n = |key: &str, default: i64| -> i64 { table.get(key).and_then(|v| v.as_integer()).unwrap_or(default) };
        let tz_text = s("timezone", "Europe/Oslo");
        let timezone: Tz = tz_text
            .parse()
            .map_err(|_| format!("config: timezone {tz_text:?} is unknown"))?;
        let send_command: Vec<String> = table
            .get("send_command")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["sudo".into(), "/opt/lindfors-newsletter/send-issue".into()]);
        if send_command.is_empty() {
            return Err("config: send_command is empty".to_string());
        }
        for stale in ["queue_repo", "queue", "weekday", "hour", "minute", "max_per_week"] {
            if table.contains_key(stale) {
                return Err(format!(
                    "config: `{stale}` is from the queue-directory publisher; the desk's database decides now, remove it"
                ));
            }
        }
        Ok(Config {
            repo: PathBuf::from(s("repo", "/srv/lindfors-publisher/site")),
            assets: PathBuf::from(s("assets", "/srv/lindfors-writing/assets")),
            export: PathBuf::from(s("export", "/srv/lindfors-publisher/export")),
            receipts: PathBuf::from(s("receipts", "/srv/lindfors-publisher/receipts")),
            timezone,
            site_url: s("site_url", "https://lindfors.no").trim_end_matches('/').to_string(),
            send_command,
            wait_minutes: n("wait_minutes", 20).max(1) as u64,
            remote: s("remote", "origin"),
            branch: s("branch", "main"),
        })
    }

    fn load(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        Config::parse(&text)
    }
}

/// One confirmed approval, as the desk's article records it, with the revision it
/// names. `dir` is set once the revision has been exported as a bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub slug: String,
    pub title: String,
    pub publish_at: DateTime<Utc>,
    pub send: bool,
    pub subject: Option<String>,
    /// Submit to This Week in Rust: a trailer on the publish commit, acted on by the
    /// `twir` workflow in the repo, not by anything on this box.
    pub twir: bool,
    pub revision: u64,
    pub token: String,
    pub digest: String,
    pub markdown: String,
    /// Asset name to content hash; the bytes are `<assets>/<hash>`.
    pub assets: BTreeMap<String, String>,
    pub dir: PathBuf,
}

/// The commit trailer the `twir` workflow greps for; the two must agree.
pub const TWIR_TRAILER: &str = "Syndicate: this-week-in-rust";

/// The desk's content digest: SHA-256 over the JSON of `[markdown, assets]`, the
/// same bytes `Revision::digest` hashes in the writing crate.
pub fn digest(markdown: &str, assets: &BTreeMap<String, String>) -> String {
    let bytes = serde_json::to_vec(&(markdown, assets)).expect("a revision serializes");
    format!("{:x}", Sha256::digest(bytes))
}

impl Entry {
    /// The publishable approval in an article document, or the reason there is none.
    /// `Ok(None)` is an article with nothing confirmed; `Err` is a confirmed approval
    /// that does not add up, which is worth a line in the log and never a publish.
    pub fn from_article(article: &serde_json::Value) -> Result<Option<Entry>, String> {
        let slug = article["id"].as_str().unwrap_or("").to_string();
        if slug.is_empty()
            || !slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            || slug.starts_with('-')
        {
            return Err("invalid article slug".into());
        }
        let approval = &article["approval"];
        if approval["confirmed"] != true {
            return Ok(None);
        }
        let revision = approval["revision"]
            .as_u64()
            .ok_or_else(|| format!("{slug}: approval names no revision"))?;
        let token = approval["token"].as_str().unwrap_or("").to_string();
        if token.is_empty() {
            return Err(format!("{slug}: approval has no token"));
        }
        let r = article["revisions"]
            .as_array()
            .and_then(|rs| rs.iter().find(|r| r["id"].as_u64() == Some(revision)))
            .ok_or_else(|| format!("{slug}: revision {revision} is not in the article"))?;
        let markdown = r["markdown"].as_str().unwrap_or("").to_string();
        let assets: BTreeMap<String, String> =
            serde_json::from_value(r["assets"].clone()).map_err(|e| format!("{slug}: assets: {e}"))?;
        let expected = approval["digest"].as_str().unwrap_or("");
        if digest(&markdown, &assets) != expected {
            return Err(format!("{slug}: the approved digest does not match revision {revision}"));
        }
        let validation = &article["validation"];
        let validated = validation["revision"].as_u64() == Some(revision)
            && validation["digest"].as_str() == Some(expected)
            && validation["errors"].as_array().is_some_and(|e| e.is_empty());
        if !validated {
            return Err(format!("{slug}: revision {revision} has not passed validation"));
        }
        for (name, hash) in &assets {
            let name_ok = !name.starts_with('.')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            let hash_ok = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
            if !name_ok || !hash_ok {
                return Err(format!("{slug}: invalid asset {name}"));
            }
        }
        let fm = frontmatter::parse(&markdown).map_err(|e| format!("{slug}: {e}"))?;
        if !fm.draft {
            return Err(format!("{slug}: the approved revision is not marked draft = true"));
        }
        let publish_at = approval["publish_at"].as_str().unwrap_or("");
        let publish_at = DateTime::parse_from_rfc3339(publish_at)
            .map_err(|e| format!("{slug}: publish_at: {e}"))?
            .with_timezone(&Utc);
        Ok(Some(Entry {
            slug,
            title: article["title"].as_str().unwrap_or("").to_string(),
            publish_at,
            send: approval["send"].as_bool().unwrap_or(true),
            subject: approval["subject"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from),
            twir: approval["twir"].as_bool().unwrap_or(false),
            revision,
            token,
            digest: expected.to_string(),
            markdown,
            assets,
            dir: PathBuf::new(),
        }))
    }

    /// Lay the revision out the way `content/blog/<slug>/` and `static/` hold a post:
    /// the Markdown as `post/index.md`, images beside it, the audio and speech files
    /// under `static/`. Every asset's bytes are checked against their hash.
    pub fn export(&mut self, assets: &Path, export: &Path) -> Result<(), String> {
        let dir = export.join(&self.slug);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| format!("Failed to clear {}: {e}", dir.display()))?;
        }
        fs::create_dir_all(dir.join("post")).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        fs::write(dir.join("post/index.md"), &self.markdown).map_err(|e| e.to_string())?;
        for (name, hash) in &self.assets {
            let blob = fs::read(assets.join(hash)).map_err(|e| format!("{}: asset {name}: {e}", self.slug))?;
            if format!("{:x}", Sha256::digest(&blob)) != *hash {
                return Err(format!("{}: asset {name} does not match its hash", self.slug));
            }
            let target = if name.ends_with(".mp3") {
                dir.join("static/audio").join(format!("{}.mp3", self.slug))
            } else if name == "audio.json" {
                dir.join("static/audio").join(format!("{}.json", self.slug))
            } else if name == "speech.txt" {
                dir.join("static/speech").join(format!("{}.txt", self.slug))
            } else {
                dir.join("post").join(name)
            };
            fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::write(&target, blob).map_err(|e| format!("Failed to write {}: {e}", target.display()))?;
        }
        self.dir = dir;
        Ok(())
    }
}

/// What the publisher would refuse a post for, asked before it is confirmed: the
/// writing desk's validation runs this. A post to be published has `draft = true`
/// (the publisher removes it), no citation marker left (this build cannot resolve
/// one), and a title and a description.
pub fn check(content: &str) -> Result<(), String> {
    let fm = frontmatter::parse(content)?;
    if !fm.draft {
        return Err("the post is not a draft: a post to be published has `draft = true`, and the publisher removes it on the day".into());
    }
    let (_, body) = frontmatter::split(content)?;
    let (masked, _) = crate::codemask::mask(body);
    let pending = crate::markers::scan(&masked);
    if !pending.is_empty() {
        return Err(format!(
            "{} unresolved citation marker(s); resolve citations first, the publisher cannot",
            pending.len()
        ));
    }
    if fm.title.is_empty() || fm.title == "Untitled" || fm.description.is_empty() {
        return Err("a title and a description are required".into());
    }
    Ok(())
}

/// The earliest entry that is due, ties by slug. Later ones wait.
pub fn pick_due(entries: &[Entry], now: DateTime<Utc>) -> Option<&Entry> {
    entries
        .iter()
        .filter(|e| e.publish_at <= now)
        .min_by(|a, b| a.publish_at.cmp(&b.publish_at).then_with(|| a.slug.cmp(&b.slug)))
}

/// Set `date` and drop `draft` in the top-level table of a post's frontmatter, leaving
/// every other byte alone. Only the top-level table is touched: `[extra]` and
/// everything after it, including the references `cite` owns, stays as it is.
pub fn rewrite_frontmatter(content: &str, date: NaiveDate) -> Result<String, String> {
    let (start, end) = frontmatter::bounds(content).ok_or("no +++ frontmatter")?;
    let toml_text = &content[start..end];
    let date_line = format!("date = {}", date.format("%Y-%m-%d"));

    let mut out = String::with_capacity(toml_text.len() + 16);
    let mut in_top = true;
    let mut dated = false;
    let mut title_at: Option<usize> = None;
    for line in toml_text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if in_top && trimmed.starts_with('[') {
            in_top = false;
            if !dated {
                // No `date` line at all: put one after the title, or at the top.
                let at = title_at.unwrap_or(0);
                out.insert_str(at, &format!("{date_line}\n"));
                dated = true;
            }
        }
        if in_top {
            let key = trimmed.split('=').next().map(str::trim).unwrap_or("");
            if key == "draft" {
                continue;
            }
            if key == "date" {
                let newline = if line.ends_with("\r\n") {
                    "\r\n"
                } else if line.ends_with('\n') {
                    "\n"
                } else {
                    ""
                };
                out.push_str(&date_line);
                out.push_str(newline);
                dated = true;
                continue;
            }
            if key == "title" {
                title_at = Some(out.len() + line.len());
            }
        }
        out.push_str(line);
    }
    if !dated {
        let at = title_at.unwrap_or(0);
        out.insert_str(at, &format!("{date_line}\n"));
    }

    let mut result = String::with_capacity(content.len() + 16);
    result.push_str(&content[..start]);
    result.push_str(&out);
    result.push_str(&content[end..]);
    Ok(result)
}

/// What the repo already holds, as far as the never-twice and series rules care.
struct Post {
    slug: String,
    date: Option<NaiveDate>,
    series: Vec<String>,
    draft: bool,
}

fn read_posts(repo: &Path) -> Result<Vec<Post>, String> {
    let blog = repo.join("content/blog");
    let mut posts = Vec::new();
    for entry in fs::read_dir(&blog).map_err(|e| format!("Failed to read {}: {e}", blog.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let index = entry.path().join("index.md");
        if !index.is_file() {
            continue;
        }
        let content = fs::read_to_string(&index).map_err(|e| format!("Failed to read {}: {e}", index.display()))?;
        posts.push(post_info(&entry.file_name().to_string_lossy(), &content)?);
    }
    Ok(posts)
}

fn post_info(slug: &str, content: &str) -> Result<Post, String> {
    let (toml_str, _) = frontmatter::split(content)?;
    let table: toml::Table = toml_str.parse().map_err(|e| format!("{slug}: TOML parse error: {e}"))?;
    let date = table.get("date").and_then(|v| match v {
        toml::Value::Datetime(d) => d
            .date
            .and_then(|d| NaiveDate::from_ymd_opt(d.year as i32, d.month as u32, d.day as u32)),
        toml::Value::String(s) => NaiveDate::parse_from_str(&s[..s.len().min(10)], "%Y-%m-%d").ok(),
        _ => None,
    });
    let series = table
        .get("taxonomies")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("series"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let draft = table.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(Post {
        slug: slug.to_string(),
        date,
        series,
        draft,
    })
}

// ---------------------------------------------------------------------------
// The desk's database
// ---------------------------------------------------------------------------

fn connect() -> Result<postgres::Client, String> {
    let url = std::env::var(DATABASE_ENV)
        .map_err(|_| format!("{DATABASE_ENV} is not set; run under `sec exec host/publisher`"))?;
    postgres::Client::connect(&url, postgres::NoTls)
        .map_err(|_| "cannot connect to the writing database".to_string())
}

/// Every confirmed approval the desk holds, as entries, and the reasons the ones that
/// do not add up were left out.
fn read_confirmed(client: &mut postgres::Client) -> Result<(Vec<Entry>, Vec<String>), String> {
    let rows = client
        .query(
            "SELECT document FROM writing_articles WHERE document->'approval'->>'confirmed' = 'true' ORDER BY id",
            &[],
        )
        .map_err(|e| format!("reading approvals: {e}"))?;
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for row in rows {
        let document: serde_json::Value = row.get(0);
        match Entry::from_article(&document) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(reason) => skipped.push(reason),
        }
    }
    Ok((entries, skipped))
}

/// Take an approval on: under the article's advisory lock, the lock the desk holds
/// while it applies a command, read the article again, check the approval is the one
/// that was picked and still confirmed, and write the first receipt. Once the receipt
/// exists the desk refuses to withdraw, so a withdrawal and a publication cannot cross.
fn take_on(client: &mut postgres::Client, config: &Config, entry: &Entry) -> Result<PathBuf, String> {
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    tx.execute("SELECT pg_advisory_xact_lock(hashtextextended($1, 1))", &[&entry.slug])
        .map_err(|e| format!("locking {}: {e}", entry.slug))?;
    let row = tx
        .query_opt("SELECT document FROM writing_articles WHERE id = $1", &[&entry.slug])
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{}: the article is gone", entry.slug))?;
    let document: serde_json::Value = row.get(0);
    let current = Entry::from_article(&document)?
        .filter(|e| e.token == entry.token && e.revision == entry.revision && e.digest == entry.digest)
        .ok_or_else(|| format!("{}: the approval changed under us; nothing published", entry.slug))?;
    debug_assert_eq!(current.slug, entry.slug);
    fs::create_dir_all(&config.receipts).map_err(|e| e.to_string())?;
    let path = config.receipts.join(format!("{}.json", entry.slug));
    if path.exists() {
        return Err(format!("{}: a receipt already exists; recovery owns it", entry.slug));
    }
    write_receipt(
        &path,
        &serde_json::json!({"slug":entry.slug,"approval":entry.token,"revision":entry.revision,"status":"publishing","send":entry.send,"subject":entry.subject,"commit":null,"mail_started":false}),
    )?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub fn run(args: &[String]) -> Result<(), String> {
    let config_path = crate::parse_flag(args, "--config").unwrap_or_else(|| DEFAULT_CONFIG.to_string());
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "run" | "next" => {
            let config = Config::load(Path::new(&config_path))?;
            let dry = sub == "next" || args.iter().any(|a| a == "--dry-run");
            publish(&config, dry)
        }
        "list" => {
            let config = Config::load(Path::new(&config_path))?;
            list(&config)
        }
        "check" => {
            let path = args.get(1).ok_or("Usage: site-tools publish check <post/index.md>")?;
            let content = fs::read_to_string(path).map_err(|e| format!("Failed to read {path}: {e}"))?;
            check(&content)?;
            println!("ok");
            Ok(())
        }
        "-h" | "--help" | "help" | "" => {
            print_usage();
            if sub.is_empty() {
                Err("missing subcommand".to_string())
            } else {
                Ok(())
            }
        }
        other => Err(format!("Unknown publish subcommand: {other}")),
    }
}

fn print_usage() {
    eprintln!("site-tools publish — Publish the next confirmed revision (runs on the box, from cron)");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  run [--dry-run]   Finish any publication in progress, then publish the earliest due approval");
    eprintln!("  next              What `run` would do, changing nothing");
    eprintln!("  list              Every confirmed approval, with its time and its receipt");
    eprintln!("  check <index.md>  The pre-flight refusals, for the writing desk's validation");
    eprintln!();
    eprintln!("  --config <path>   Default {DEFAULT_CONFIG}");
    eprintln!("  {DATABASE_ENV}    The desk's database, as the writing_publisher role (from the host/publisher bundle)");
    eprintln!();
    eprintln!("A post is confirmed in the writing desk, not here. Withdraw it there too, before this");
    eprintln!("has taken it on; afterwards its receipt is the record and a person finishes it.");
}

fn list(config: &Config) -> Result<(), String> {
    let mut client = connect()?;
    let (entries, skipped) = read_confirmed(&mut client)?;
    if entries.is_empty() && skipped.is_empty() {
        println!("No confirmed approvals.");
    }
    for e in &entries {
        let receipt = read_receipt(&config.receipts.join(format!("{}.json", e.slug)));
        println!(
            "  {:<40} {}  revision {:<3} {}  {}  {}",
            e.slug,
            e.publish_at.with_timezone(&config.timezone).format("%Y-%m-%d %H:%M %Z"),
            e.revision,
            if e.send { "newsletter" } else { "no mail   " },
            if e.twir { "twir" } else { "    " },
            receipt
                .as_ref()
                .and_then(|r| r["status"].as_str().map(|s| format!("receipt: {s}")))
                .unwrap_or_else(|| "waiting".into())
        );
    }
    for reason in &skipped {
        println!("  skipped: {reason}");
    }
    println!();
    publish(config, true)
}

fn publish(config: &Config, dry: bool) -> Result<(), String> {
    let now = Utc::now();
    println!("now {}", now.with_timezone(&config.timezone).format("%Y-%m-%d %H:%M %Z"));

    if !dry {
        // The clone is a robot's. Whatever it holds, the run starts from the branch it
        // is going to push to.
        reset_to_remote(config, &config.repo)?;
        recover_receipts(config)?;
    }
    let mut client = connect()?;
    let (mut entries, skipped) = read_confirmed(&mut client)?;
    for reason in &skipped {
        println!("skipped: {reason}");
    }
    let posts = read_posts(&config.repo)?;
    // Out already, whatever the desk says, or already taken on by an earlier run
    // whose receipt recovery owns. Never twice.
    entries.retain(|e| {
        if posts.iter().any(|p| p.slug == e.slug && !p.draft && p.date.is_some()) {
            println!("{}: already published on the site; withdraw it in the desk", e.slug);
            return false;
        }
        if config.receipts.join(format!("{}.json", e.slug)).exists() {
            println!("{}: has a receipt; recovery owns it", e.slug);
            return false;
        }
        true
    });
    let Some(entry) = pick_due(&entries, now) else {
        match entries.iter().map(|e| e.publish_at).min() {
            None => println!("nothing confirmed and waiting; nothing to do."),
            Some(next) => println!(
                "nothing due; the next is at {}.",
                next.with_timezone(&config.timezone).format("%Y-%m-%d %H:%M %Z")
            ),
        }
        return Ok(());
    };

    println!(
        "{} revision {} would go out{}{}{}",
        entry.slug,
        entry.revision,
        if entry.send {
            " with a newsletter"
        } else {
            ", no newsletter"
        },
        if entry.twir {
            ", submitted to This Week in Rust"
        } else {
            ""
        },
        if dry { " (dry run)" } else { "" }
    );
    if dry {
        return Ok(());
    }

    let receipt_path = take_on(&mut client, config, entry)?;
    drop(client);
    let mut receipt = read_receipt(&receipt_path).ok_or("the receipt just written cannot be read")?;
    let mut entry = entry.clone();
    if let Err(e) = entry.export(&config.assets, &config.export) {
        // Nothing has moved. The receipt says why, and stays, so a person looks: an
        // asset that does not match its hash is not something the next run fixes.
        receipt["status"] = serde_json::json!("needs attention");
        receipt["error"] = serde_json::json!(e);
        write_receipt(&receipt_path, &receipt)?;
        return Err(e);
    }

    // --- Move the bundle in and date it -------------------------------------------
    let date = now.with_timezone(&config.timezone).date_naive();
    let dest = config.repo.join("content/blog").join(&entry.slug);
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|e| format!("Failed to clear {}: {e}", dest.display()))?;
    }
    copy_dir(&entry.dir.join("post"), &dest)?;
    let statics = entry.dir.join("static");
    if statics.is_dir() {
        copy_dir(&statics, &config.repo.join("static"))?;
    }

    let index = dest.join("index.md");
    let content = fs::read_to_string(&index).map_err(|e| format!("Failed to read {}: {e}", index.display()))?;
    let rewritten = rewrite_frontmatter(&content, date)?;
    let info = post_info(&entry.slug, &rewritten)?;
    for other in posts.iter().filter(|p| p.slug != entry.slug && !p.draft) {
        if other.date == Some(date) && other.series.iter().any(|s| info.series.contains(s)) {
            let e = format!(
                "{} is in the same series as {} and would share its date {date}; series order is the date",
                entry.slug, other.slug
            );
            receipt["status"] = serde_json::json!("needs attention");
            receipt["error"] = serde_json::json!(e);
            write_receipt(&receipt_path, &receipt)?;
            return Err(e);
        }
    }
    fs::write(&index, &rewritten).map_err(|e| format!("Failed to write {}: {e}", index.display()))?;
    println!("dated {} {}", entry.slug, date);

    // --- Derived files, as build.sh makes them, minus audio and citations ---------
    std::env::set_current_dir(&config.repo).map_err(|e| format!("Failed to enter {}: {e}", config.repo.display()))?;
    let step = |name: &str, r: Result<(), String>| {
        r.map_err(|e| format!("{name} failed, nothing pushed (the next run resets the clone): {e}"))
    };
    let derived = step("markdown all", markdown::gen_all())
        .and_then(|()| step("speech all", speech::gen_all()))
        .and_then(|()| step("pdf all", pdf::gen_all()))
        .and_then(|()| step("og all", og::gen_all()))
        .and_then(|()| {
            if entry.send {
                step("newsletter gen", newsletter::gen(&index.to_string_lossy()))
            } else {
                Ok(())
            }
        });
    if let Err(e) = derived {
        // Nothing pushed: the receipt goes, so the next run tries again from the desk.
        let _ = fs::remove_file(&receipt_path);
        return Err(e);
    }

    // --- Commit and push ------------------------------------------------------------
    git(&config.repo, &["add", "-A"])?;
    let message = commit_message(&entry);
    git(&config.repo, &["commit", "--quiet", "-m", &message])?;
    let hash = git(&config.repo, &["rev-parse", "--short", "HEAD"])?;
    receipt["commit"] = serde_json::json!(git(&config.repo, &["rev-parse", "HEAD"])?.trim());
    write_receipt(&receipt_path, &receipt)?;
    git(
        &config.repo,
        &["push", "--quiet", &config.remote, &format!("HEAD:{}", config.branch)],
    )?;
    println!("pushed {} as {}", entry.slug, hash.trim());
    let _ = fs::remove_dir_all(&entry.dir);

    receipt["status"] = serde_json::json!("deploying");
    write_receipt(&receipt_path, &receipt)?;
    finish_receipt(config, &receipt_path, &mut receipt)
}

fn read_receipt(path: &Path) -> Option<serde_json::Value> {
    fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}
fn write_receipt(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let temp = path.with_extension("tmp");
    let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(
        serde_json::to_string_pretty(value)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| e.to_string())?;
    fs::File::open(path.parent().unwrap_or(Path::new(".")))
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
/// From a pushed commit to a settled receipt: wait for the site, then mail once. A
/// send that may have started is handed to a person, never repeated.
fn finish_receipt(config: &Config, path: &Path, receipt: &mut serde_json::Value) -> Result<(), String> {
    let slug = receipt["slug"].as_str().ok_or("receipt missing slug")?.to_string();
    receipt["status"] = serde_json::json!("deploying");
    write_receipt(path, receipt)?;
    let page = format!("{}/blog/{slug}/", config.site_url);
    let issue = format!("{}/newsletter/{slug}.md", config.site_url);
    let urls: Vec<&str> = if receipt["send"] == true {
        vec![&page, &issue]
    } else {
        vec![&page]
    };
    if let Err(error) = wait_for(&urls, config.wait_minutes) {
        receipt["error"] = serde_json::json!(error);
        write_receipt(path, receipt)?;
        return Err(format!("{slug}: deployment not ready; next run will check again"));
    }
    if receipt["send"] == true {
        if receipt["mail_started"] == true {
            receipt["status"] = serde_json::json!("needs attention");
            receipt["error"]=serde_json::json!("Newsletter delivery is uncertain. Inspect the newsletter delivery records before recovery; no automatic resend.");
            write_receipt(path, receipt)?;
            return Ok(());
        }
        receipt["mail_started"] = serde_json::json!(true);
        write_receipt(path, receipt)?;
        let mut send = Command::new(&config.send_command[0]);
        send.args(&config.send_command[1..]).arg(&slug);
        if let Some(subject) = receipt["subject"].as_str().filter(|s| !s.is_empty()) {
            send.arg("--subject").arg(subject);
        }
        match send.status() {
            Ok(status) if status.success() => {}
            result => {
                receipt["status"] = serde_json::json!("needs attention");
                receipt["error"] = serde_json::json!(format!(
                    "Newsletter failed or uncertain: {result:?}. Inspect delivery records before retrying."
                ));
                write_receipt(path, receipt)?;
                return Ok(());
            }
        }
    }
    receipt["status"] = serde_json::json!("published");
    receipt["error"] = serde_json::Value::Null;
    write_receipt(path, receipt)
}
/// Receipts a previous run left unfinished. One whose commit reached the remote is
/// finished from where it stopped; one whose commit never landed, or that has no
/// commit, is discarded, so the approval is picked up again as if untouched. Settled
/// receipts (`published`, `needs attention`) are left for a person.
fn recover_receipts(config: &Config) -> Result<(), String> {
    let dir = &config.receipts;
    if !dir.exists() {
        return Ok(());
    }
    for path in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = path.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if matches!(value["status"].as_str(), Some("published" | "needs attention")) {
            continue;
        }
        let commit = value["commit"].as_str().unwrap_or("");
        if commit.is_empty() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
            continue;
        }
        let reached = Command::new("git")
            .arg("-C")
            .arg(&config.repo)
            .args(["merge-base", "--is-ancestor", commit, "HEAD"])
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !reached {
            fs::remove_file(path).map_err(|e| e.to_string())?;
            continue;
        }
        finish_receipt(config, &path, &mut value)?;
    }
    Ok(())
}

/// `Publish: <title>`, plus the syndication trailer when the approval asked for it.
/// The trailer is what the `twir` workflow reads off the push; nothing here talks to
/// GitHub's API, so the box needs no token beyond its deploy key.
fn commit_message(entry: &Entry) -> String {
    let title = if entry.title.is_empty() {
        &entry.slug
    } else {
        &entry.title
    };
    if entry.twir {
        format!("Publish: {title}\n\n{TWIR_TRAILER}\n")
    } else {
        format!("Publish: {title}")
    }
}

/// Poll until every URL answers 200, or give up after `minutes`.
fn wait_for(urls: &[&str], minutes: u64) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(minutes * 60);
    loop {
        let mut pending = Vec::new();
        for url in urls {
            let code = http_status(url);
            if code != "200" {
                pending.push(format!("{url} -> {code}"));
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "the site did not answer 200 within {minutes} minutes ({})",
                pending.join(", ")
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The status code for a GET, past any cache: a fresh query string each time, so a 404
/// Cloudflare held from before the deploy is not the answer.
fn http_status(url: &str) -> String {
    let stamp = Utc::now().timestamp();
    let busted = format!("{url}?publish={stamp}");
    Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-m",
            "20",
            "-H",
            "Cache-Control: no-cache",
            &busted,
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("curl failed: {e}"))
}

/// Fetch and hard-reset a robot clone to its remote branch. Nothing kept in it
/// survives except what is ignored.
fn reset_to_remote(config: &Config, repo: &Path) -> Result<(), String> {
    git(repo, &["fetch", "--quiet", &config.remote, &config.branch])?;
    git(
        repo,
        &[
            "reset",
            "--hard",
            "--quiet",
            &format!("{}/{}", config.remote, config.branch),
        ],
    )?;
    git(repo, &["clean", "-fdq"])?;
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("Failed to run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Copy a directory tree. The bundle is small: a markdown file and a few images.
pub fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("Failed to create {}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("Failed to read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("Failed to read {}: {e}", from.display()))?;
        let src: PathBuf = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            fs::copy(&src, &dst).map_err(|e| format!("Failed to copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slug: &str, publish_at: &str) -> Entry {
        Entry {
            slug: slug.into(),
            title: slug.into(),
            publish_at: DateTime::parse_from_rfc3339(publish_at).unwrap().with_timezone(&Utc),
            send: true,
            subject: None,
            twir: false,
            revision: 1,
            token: "t".into(),
            digest: String::new(),
            markdown: String::new(),
            assets: BTreeMap::new(),
            dir: PathBuf::from(slug),
        }
    }

    /// An article document the way the desk stores it, with one confirmed approval.
    fn article(slug: &str, markdown: &str, assets: BTreeMap<String, String>) -> serde_json::Value {
        let d = digest(markdown, &assets);
        serde_json::json!({
            "id": slug, "title": "A title", "head": 2,
            "revisions": [
                {"id": 1, "parent": 0, "markdown": "old", "assets": {}, "author": "Emil", "summary": "", "created_at": "", "proposal": false},
                {"id": 2, "parent": 1, "markdown": markdown, "assets": assets, "author": "Emil", "summary": "", "created_at": "", "proposal": false}
            ],
            "validation": {"revision": 2, "digest": d, "renderer": "zola", "errors": []},
            "approval": {"token": "tok", "revision": 2, "digest": d, "publish_at": "2030-09-11T10:00:00+02:00",
                          "send": true, "subject": "", "twir": true, "confirmed": true},
            "status": "queued"
        })
    }

    const DRAFT: &str = "+++\ntitle = \"T\"\ndescription = \"D\"\ndraft = true\n+++\n\nBody\n";

    #[test]
    fn commit_message_carries_the_trailer_only_when_asked() {
        let mut e = entry("a-post", "2026-09-01T00:00:00Z");
        e.title = "A title".into();
        assert_eq!(commit_message(&e), "Publish: A title");
        e.twir = true;
        assert_eq!(commit_message(&e), "Publish: A title\n\nSyndicate: this-week-in-rust\n");
    }

    #[test]
    fn the_digest_is_the_desks() {
        // sha256 of the JSON `["m",{}]`, what the writing crate's Revision::digest hashes.
        assert_eq!(
            digest("m", &BTreeMap::new()),
            format!("{:x}", Sha256::digest(br#"["m",{}]"#))
        );
    }

    #[test]
    fn a_confirmed_approval_becomes_an_entry() {
        let a = article("a-post", DRAFT, BTreeMap::from([("hero.webp".into(), "a".repeat(64))]));
        let e = Entry::from_article(&a).unwrap().unwrap();
        assert_eq!(e.slug, "a-post");
        assert_eq!(e.revision, 2);
        assert_eq!(e.token, "tok");
        assert!(e.twir);
        assert_eq!(e.subject, None);
        assert_eq!(e.publish_at.to_rfc3339(), "2030-09-11T08:00:00+00:00");
        assert_eq!(e.markdown, DRAFT);
    }

    #[test]
    fn an_unconfirmed_article_is_nothing_and_a_broken_approval_is_a_reason() {
        let mut a = article("a-post", DRAFT, BTreeMap::new());
        a["approval"]["confirmed"] = serde_json::json!(false);
        assert_eq!(Entry::from_article(&a).unwrap(), None);
        // The digest names other content than the revision holds.
        let mut a = article("a-post", DRAFT, BTreeMap::new());
        a["approval"]["digest"] = serde_json::json!("0".repeat(64));
        assert!(Entry::from_article(&a).unwrap_err().contains("digest"));
        // Validation is for an earlier revision.
        let mut a = article("a-post", DRAFT, BTreeMap::new());
        a["validation"]["revision"] = serde_json::json!(1);
        assert!(Entry::from_article(&a).unwrap_err().contains("validation"));
        // Not a draft.
        let a = article("a-post", "+++\ntitle = \"T\"\n+++\nBody\n", BTreeMap::new());
        assert!(Entry::from_article(&a).unwrap_err().contains("draft"));
        // A bad asset name.
        let a = article("a-post", DRAFT, BTreeMap::from([("../x.webp".into(), "a".repeat(64))]));
        assert!(Entry::from_article(&a).unwrap_err().contains("asset"));
        // A bad slug.
        let a = article("Bad Slug", DRAFT, BTreeMap::new());
        assert!(Entry::from_article(&a).is_err());
    }

    #[test]
    fn export_lays_the_revision_out_as_a_bundle_and_checks_every_asset() {
        let root = std::env::temp_dir().join(format!("publisher-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let assets = root.join("assets");
        fs::create_dir_all(&assets).unwrap();
        let webp = b"RIFFwebp";
        let mp3 = b"ID3mp3";
        let h_webp = format!("{:x}", Sha256::digest(webp));
        let h_mp3 = format!("{:x}", Sha256::digest(mp3));
        fs::write(assets.join(&h_webp), webp).unwrap();
        fs::write(assets.join(&h_mp3), mp3).unwrap();
        let manifest = BTreeMap::from([
            ("hero.webp".to_string(), h_webp.clone()),
            ("voice.mp3".to_string(), h_mp3.clone()),
        ]);
        let mut e = Entry::from_article(&article("a-post", DRAFT, manifest)).unwrap().unwrap();
        e.export(&assets, &root.join("export")).unwrap();
        assert_eq!(fs::read_to_string(e.dir.join("post/index.md")).unwrap(), DRAFT);
        assert_eq!(fs::read(e.dir.join("post/hero.webp")).unwrap(), webp);
        assert_eq!(fs::read(e.dir.join("static/audio/a-post.mp3")).unwrap(), mp3);
        // A blob that does not match its hash stops the export.
        fs::write(assets.join(&h_webp), b"tampered").unwrap();
        assert!(e.export(&assets, &root.join("export")).unwrap_err().contains("hash"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_pre_flight_check_refuses_what_the_publisher_would() {
        assert!(check(DRAFT).is_ok());
        assert!(check("+++\ntitle = \"T\"\ndescription = \"D\"\n+++\nBody\n").unwrap_err().contains("draft"));
        assert!(check("+++\ntitle = \"T\"\ndescription = \"D\"\ndraft = true\n+++\nSee [@Smith2020].\n")
            .unwrap_err()
            .contains("citation"));
        assert!(check("+++\ntitle = \"T\"\ndraft = true\n+++\nBody\n").unwrap_err().contains("description"));
    }

    #[test]
    fn the_earliest_due_entry_goes_and_future_ones_wait() {
        let now = DateTime::parse_from_rfc3339("2026-09-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let due = entry("due", "2026-09-11T11:59:00Z");
        let earlier = entry("earlier", "2026-09-10T09:00:00Z");
        let future = entry("future", "2026-09-11T12:01:00Z");
        assert_eq!(pick_due(&[due.clone(), future.clone(), earlier.clone()], now).unwrap().slug, "earlier");
        assert_eq!(pick_due(&[due, future.clone()], now).unwrap().slug, "due");
        assert!(pick_due(&[future], now).is_none());
    }

    #[test]
    fn rewrite_sets_date_and_drops_draft_only_at_top_level() {
        let post = "+++\ntitle = \"T\"\ndescription = \"D\"\ndate = 2026-09-04\ndraft = true\n\n[taxonomies]\ntags = [\"a\"]\n\n[extra]\ntoc = true\ndraft = true\n+++\n\nBody with date = 1.\n";
        let out = rewrite_frontmatter(post, NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()).unwrap();
        assert_eq!(
            out,
            "+++\ntitle = \"T\"\ndescription = \"D\"\ndate = 2026-10-06\n\n[taxonomies]\ntags = [\"a\"]\n\n[extra]\ntoc = true\ndraft = true\n+++\n\nBody with date = 1.\n"
        );
        let fm = frontmatter::parse(&out).unwrap();
        assert!(!fm.draft);
        assert_eq!(fm.date, "2026-10-06");
    }

    #[test]
    fn rewrite_adds_a_date_after_the_title_when_there_is_none() {
        let post = "+++\ntitle = \"T\"\ndraft = true\n[extra]\ntoc = true\n+++\nBody\n";
        let out = rewrite_frontmatter(post, NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()).unwrap();
        assert_eq!(
            out,
            "+++\ntitle = \"T\"\ndate = 2026-10-06\n[extra]\ntoc = true\n+++\nBody\n"
        );
        let post = "+++\ntitle = \"T\"\n+++\nBody\n";
        let out = rewrite_frontmatter(post, NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()).unwrap();
        assert_eq!(out, "+++\ntitle = \"T\"\ndate = 2026-10-06\n+++\nBody\n");
    }

    #[test]
    fn rewrite_keeps_crlf() {
        let post = "+++\r\ntitle = \"T\"\r\ndate = 2026-01-01\r\ndraft = true\r\n+++\r\nBody\r\n";
        let out = rewrite_frontmatter(post, NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()).unwrap();
        assert_eq!(out, "+++\r\ntitle = \"T\"\r\ndate = 2026-10-06\r\n+++\r\nBody\r\n");
    }

    #[test]
    fn post_info_reads_dates_drafts_and_series() {
        let a = post_info("a", "+++\ntitle = \"A\"\ndate = 2026-10-06\n+++\n").unwrap();
        let b = post_info(
            "b",
            "+++\ntitle = \"B\"\ndate = \"2026-10-09T10:00:00Z\"\ndraft = true\n+++\n",
        )
        .unwrap();
        let c = post_info(
            "c",
            "+++\ntitle = \"C\"\ndate = 2026-10-13\n[taxonomies]\nseries = [\"S\"]\n+++\n",
        )
        .unwrap();
        assert_eq!(a.date, NaiveDate::from_ymd_opt(2026, 10, 6));
        assert!(b.draft);
        assert_eq!(c.series, vec!["S".to_string()]);
    }

    #[test]
    fn config_parses_with_defaults_and_refuses_the_queue_keys() {
        let c = Config::parse("repo = \"/srv/x/site\"\n").unwrap();
        assert_eq!(c.repo, PathBuf::from("/srv/x/site"));
        assert_eq!(c.assets, PathBuf::from("/srv/lindfors-writing/assets"));
        assert_eq!(c.receipts, PathBuf::from("/srv/lindfors-publisher/receipts"));
        assert_eq!(
            c.send_command,
            vec!["sudo".to_string(), "/opt/lindfors-newsletter/send-issue".to_string()]
        );
        assert!(Config::parse("timezone = \"Mars/Olympus\"").is_err());
        assert!(Config::parse("queue_repo = \"/srv/q\"\n").err().unwrap().contains("queue_repo"));
        assert!(Config::parse("weekday = \"tuesday\"\n").is_err());
    }

    #[test]
    fn receipts_are_written_atomically() {
        let root = std::env::temp_dir().join(format!("publisher-receipt-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("test.json");
        write_receipt(&path, &serde_json::json!({"status":"deploying","commit":"abc"})).unwrap();
        write_receipt(&path, &serde_json::json!({"status":"published","commit":"abc"})).unwrap();
        let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["status"], "published");
        assert!(!path.with_extension("tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }

    // --- Publication receipt recovery -------------------------------------------------
    // Everything here runs against a throwaway git remote on disk, a local HTTP server
    // standing in for the deployed site, and a recording script standing in for the
    // newsletter. No network, no real remote, no mail, no database: recovery reads
    // receipts and the site clone only.

    struct Fixture {
        root: PathBuf,
        config: Config,
        sent_log: PathBuf,
        server: Option<std::process::Child>,
        /// Held for the fixture's life: every fixture serves the same slug on the
        /// same port range, and one answers another's readiness probe otherwise.
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(mut server) = self.server.take() {
                let _ = server.kill();
                let _ = server.wait();
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// One fixture at a time: they share a port range and the fake send's
    /// FAKE_SEND_STATUS, a process-wide variable.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl Fixture {
        /// A bare remote and a clone of it, and a receipts directory outside it.
        fn new(name: &str) -> Fixture {
            let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            // No shell metacharacters: the fake send script interpolates this path.
            let root = std::env::temp_dir().join(format!("publisher-recovery-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            let site = Fixture::repo(&root, "site");
            let sent_log = root.join("sent.log");
            let send = root.join("send-issue");
            fs::write(
                &send,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit ${{FAKE_SEND_STATUS:-0}}\n",
                    sent_log.display()
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&send, fs::Permissions::from_mode(0o755)).unwrap();
            }
            fs::create_dir_all(root.join("receipts")).unwrap();
            let config = Config {
                repo: site,
                assets: root.join("assets"),
                export: root.join("export"),
                receipts: root.join("receipts"),
                timezone: chrono_tz::Europe::Oslo,
                // No listener here, so every URL answers something other than 200.
                site_url: "http://127.0.0.1:1".into(),
                send_command: vec![send.to_string_lossy().into_owned()],
                wait_minutes: 0,
                remote: "origin".into(),
                branch: "main".into(),
            };
            Fixture { root, config, sent_log, server: None, _serial: serial }
        }

        fn repo(root: &Path, name: &str) -> PathBuf {
            let bare = root.join(format!("{name}.git"));
            let work = root.join(name);
            Command::new("git").args(["init", "--quiet", "--bare", "-b", "main"]).arg(&bare).status().unwrap();
            Command::new("git").args(["init", "--quiet", "-b", "main"]).arg(&work).status().unwrap();
            git(&work, &["remote", "add", "origin", &bare.to_string_lossy()]).unwrap();
            git(&work, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(&work, &["config", "user.name", "Test"]).unwrap();
            fs::write(work.join("README.md"), "fixture\n").unwrap();
            git(&work, &["add", "-A"]).unwrap();
            git(&work, &["commit", "--quiet", "-m", "init"]).unwrap();
            git(&work, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
            work
        }

        /// Serve `blog/<slug>/` and `newsletter/<slug>.md` so `wait_for` sees 200.
        fn serve(&mut self, slug: &str) {
            let docs = self.root.join("public");
            fs::create_dir_all(docs.join("blog").join(slug)).unwrap();
            fs::create_dir_all(docs.join("newsletter")).unwrap();
            fs::write(docs.join("blog").join(slug).join("index.html"), "<p>page</p>").unwrap();
            fs::write(docs.join("newsletter").join(format!("{slug}.md")), "issue").unwrap();
            for port in 45771u16..45871 {
                let child = Command::new("python3")
                    .args(["-m", "http.server", "--bind", "127.0.0.1", &port.to_string()])
                    .current_dir(&docs)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                self.server = Some(child);
                self.config.site_url = format!("http://127.0.0.1:{port}");
                for _ in 0..50 {
                    std::thread::sleep(Duration::from_millis(100));
                    if http_status(&format!("{}/blog/{slug}/", self.config.site_url)) == "200" {
                        return;
                    }
                }
                let mut server = self.server.take().unwrap();
                let _ = server.kill();
                let _ = server.wait();
            }
            panic!("no local port answered");
        }

        fn receipt(&self, slug: &str) -> PathBuf {
            self.config.receipts.join(format!("{slug}.json"))
        }
        fn write(&self, slug: &str, value: serde_json::Value) {
            write_receipt(&self.receipt(slug), &value).unwrap();
        }
        fn read(&self, slug: &str) -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(self.receipt(slug)).unwrap()).unwrap()
        }
        fn sends(&self) -> Vec<String> {
            fs::read_to_string(&self.sent_log)
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }
        /// A commit on the site clone that has been pushed, as the publisher would.
        fn land_commit(&self, slug: &str) -> String {
            fs::create_dir_all(self.config.repo.join("content/blog").join(slug)).unwrap();
            fs::write(
                self.config.repo.join("content/blog").join(slug).join("index.md"),
                "+++\ntitle = \"T\"\ndate = 2026-09-11\n+++\nBody\n",
            )
            .unwrap();
            git(&self.config.repo, &["add", "-A"]).unwrap();
            git(&self.config.repo, &["commit", "--quiet", "-m", "Publish"]).unwrap();
            let hash = git(&self.config.repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
            git(&self.config.repo, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
            hash
        }
    }

    #[test]
    fn a_started_send_is_never_repeated_after_a_restart() {
        let mut f = Fixture::new("resend");
        let commit = f.land_commit("post");
        f.serve("post");
        // The crash case: the send was started, and whether the mail went out is not
        // knowable from here. Recovery must hand it to a person, not try again.
        f.write(
            "post",
            serde_json::json!({"slug":"post","status":"deploying","send":true,"subject":null,
                               "commit":commit,"mail_started":true}),
        );
        recover_receipts(&f.config).unwrap();
        assert_eq!(f.read("post")["status"], "needs attention");
        assert!(f.read("post")["error"].as_str().unwrap().contains("uncertain"));
        assert!(f.sends().is_empty(), "recovery must not resend: {:?}", f.sends());
        // And a second run leaves the terminal state alone.
        recover_receipts(&f.config).unwrap();
        assert_eq!(f.read("post")["status"], "needs attention");
        assert!(f.sends().is_empty());
    }

    #[test]
    fn recovery_finishes_a_deployment_that_had_not_mailed_yet() {
        let mut f = Fixture::new("finish");
        let commit = f.land_commit("post");
        f.serve("post");
        f.write(
            "post",
            serde_json::json!({"slug":"post","status":"deploying","send":true,"subject":"Chosen",
                               "commit":commit,"mail_started":false}),
        );
        recover_receipts(&f.config).unwrap();
        let receipt = f.read("post");
        assert_eq!(receipt["status"], "published");
        assert_eq!(receipt["error"], serde_json::Value::Null);
        assert_eq!(receipt["mail_started"], true);
        assert_eq!(f.sends(), vec!["post --subject Chosen".to_string()]);
    }

    #[test]
    fn a_send_that_fails_is_reported_and_not_retried() {
        let mut f = Fixture::new("failsend");
        let commit = f.land_commit("post");
        f.serve("post");
        f.write(
            "post",
            serde_json::json!({"slug":"post","status":"deploying","send":true,"subject":null,
                               "commit":commit,"mail_started":false}),
        );
        std::env::set_var("FAKE_SEND_STATUS", "3");
        let result = recover_receipts(&f.config);
        std::env::remove_var("FAKE_SEND_STATUS");
        result.unwrap();
        assert_eq!(f.read("post")["status"], "needs attention");
        assert_eq!(f.sends().len(), 1);
        recover_receipts(&f.config).unwrap();
        assert_eq!(f.sends().len(), 1, "a failed send is never repeated automatically");
    }

    #[test]
    fn a_deployment_that_is_not_up_yet_is_retried_without_mailing() {
        let f = Fixture::new("waiting");
        let commit = f.land_commit("post");
        // site_url has no listener, so wait_for cannot see 200.
        f.write(
            "post",
            serde_json::json!({"slug":"post","status":"deploying","send":true,"subject":null,
                               "commit":commit,"mail_started":false}),
        );
        assert!(recover_receipts(&f.config).is_err());
        let receipt = f.read("post");
        assert_eq!(receipt["status"], "deploying", "still recoverable");
        assert_eq!(receipt["mail_started"], false);
        assert!(receipt["error"].as_str().unwrap().contains("did not answer 200"));
        assert!(f.sends().is_empty());
    }

    #[test]
    fn a_receipt_whose_commit_never_landed_is_discarded_so_the_approval_is_retried() {
        let f = Fixture::new("unlanded");
        // Committed locally but never pushed, exactly what reset_to_remote throws away.
        fs::write(f.config.repo.join("stray.txt"), "x").unwrap();
        git(&f.config.repo, &["add", "-A"]).unwrap();
        git(&f.config.repo, &["commit", "--quiet", "-m", "unpushed"]).unwrap();
        let orphan = git(&f.config.repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        reset_to_remote(&f.config, &f.config.repo).unwrap();
        f.write(
            "post",
            serde_json::json!({"slug":"post","status":"publishing","send":true,"subject":null,
                               "commit":orphan,"mail_started":false}),
        );
        recover_receipts(&f.config).unwrap();
        assert!(!f.receipt("post").exists(), "the receipt must not block a retry");
        assert!(f.sends().is_empty());
    }

    #[test]
    fn a_receipt_with_no_commit_is_discarded() {
        let f = Fixture::new("nocommit");
        f.write(
            "post",
            serde_json::json!({"slug":"post","status":"publishing","send":true,"subject":null,
                               "commit":null,"mail_started":false}),
        );
        recover_receipts(&f.config).unwrap();
        assert!(!f.receipt("post").exists());
        assert!(f.sends().is_empty());
    }

    #[test]
    fn settled_receipts_are_left_alone() {
        let mut f = Fixture::new("settled");
        let commit = f.land_commit("post");
        f.serve("post");
        for status in ["published", "needs attention"] {
            f.write(
                "post",
                serde_json::json!({"slug":"post","status":status,"send":true,"subject":null,
                                   "commit":commit,"mail_started":false}),
            );
            recover_receipts(&f.config).unwrap();
            assert_eq!(f.read("post")["status"], status);
            assert!(f.sends().is_empty(), "a settled receipt never mails");
        }
    }
}
