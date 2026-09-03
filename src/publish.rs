//! Publish the next queued post. Runs on mail.lindfors.no as the `publisher` account,
//! from cron, against a clone of the site repo it can push to with a deploy key.
//!
//! The queue is `schedule.rs`'s output: one directory per post under the queue root,
//! holding `post/` (the page bundle), `static/` (audio and speech files, if any) and a
//! `schedule.toml` sidecar. A run is one decision and one sequence:
//!
//! - *Is it time?* The config names a weekday, an hour and a time zone. The slot for the
//!   current ISO week is that moment; before it, nothing happens. After it, the run
//!   proceeds if fewer than `max_per_week` posts in the repo carry a date in this week,
//!   which is what makes a run every hour idempotent and a missed hour catch up on its
//!   own, and what stops a second post going out in a week someone published by hand.
//! - *Which post?* An entry pinned to this week or an earlier one, oldest pin first;
//!   otherwise the earliest queued entry with no pin. Entries pinned to a later week wait.
//! - *The sequence.* Reset the clone to the remote branch, move the bundle in, write
//!   today's date into its frontmatter and drop `draft`, generate the derived files the
//!   build would, commit, push. Then, if the sidecar says so, wait for the page to answer
//!   200 and hand the slug to the newsletter binary over loopback. Every step fails
//!   closed: a failed push mails nobody, and the next run starts from the remote again.
//!
//! `date` is assigned here, not by the author. Series order is the date, so it is
//! publish order, and two posts in a series can never share one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

use crate::{frontmatter, markdown, newsletter, og, pdf, speech};

const DEFAULT_CONFIG: &str = "/etc/lindfors-publisher.toml";
const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub struct Config {
    pub repo: PathBuf,
    pub queue: PathBuf,
    /// Where a published entry is moved to, so the run leaves an audit trail.
    pub archive: PathBuf,
    pub weekday: Weekday,
    pub hour: u32,
    pub minute: u32,
    pub timezone: Tz,
    pub max_per_week: usize,
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

        let weekday_text = s("weekday", "tuesday");
        let weekday: Weekday = weekday_text
            .parse()
            .map_err(|_| format!("config: weekday {weekday_text:?} is not a day of the week"))?;
        let tz_text = s("timezone", "Europe/Oslo");
        let timezone: Tz = tz_text.parse().map_err(|_| format!("config: timezone {tz_text:?} is unknown"))?;
        let hour = n("hour", 8);
        let minute = n("minute", 0);
        if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
            return Err("config: hour must be 0-23 and minute 0-59".to_string());
        }
        let send_command: Vec<String> = table
            .get("send_command")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["sudo".into(), "/opt/lindfors-newsletter/send-issue".into()]);
        if send_command.is_empty() {
            return Err("config: send_command is empty".to_string());
        }

        let queue = PathBuf::from(s("queue", "/srv/lindfors-publisher/queue"));
        let archive = table
            .get("archive")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| queue.parent().map(|p| p.join("published")).unwrap_or_else(|| queue.join("published")));

        Ok(Config {
            repo: PathBuf::from(s("repo", "/srv/lindfors-publisher/site")),
            queue,
            archive,
            weekday,
            hour: hour as u32,
            minute: minute as u32,
            timezone,
            max_per_week: n("max_per_week", 1).max(0) as usize,
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

/// One queued post, as its sidecar describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub slug: String,
    pub title: String,
    pub queued_at: String,
    /// `(iso_year, iso_week)` when pinned.
    pub week: Option<(i32, u32)>,
    pub send: bool,
    pub subject: Option<String>,
    /// Submit to This Week in Rust: a trailer on the publish commit, acted on by the
    /// `twir` workflow in the repo, not by anything on this box.
    pub twir: bool,
    pub dir: PathBuf,
}

/// The commit trailer the `twir` workflow greps for; the two must agree.
pub const TWIR_TRAILER: &str = "Syndicate: this-week-in-rust";

impl Entry {
    fn parse(dir: &Path, sidecar: &str) -> Result<Entry, String> {
        let table: toml::Table = sidecar.parse().map_err(|e| format!("{}: {e}", dir.display()))?;
        let get = |key: &str| table.get(key).and_then(|v| v.as_str()).map(String::from);
        let slug = get("slug").ok_or_else(|| format!("{}: sidecar has no slug", dir.display()))?;
        let week = match get("week") {
            Some(w) => Some(parse_week(&w)?),
            None => None,
        };
        Ok(Entry {
            slug,
            title: get("title").unwrap_or_default(),
            queued_at: get("queued_at").unwrap_or_default(),
            week,
            send: table.get("send").and_then(|v| v.as_bool()).unwrap_or(true),
            subject: get("subject"),
            twir: table.get("twir").and_then(|v| v.as_bool()).unwrap_or(false),
            dir: dir.to_path_buf(),
        })
    }

    fn slot_text(&self) -> String {
        match self.week {
            Some((y, w)) => format!("{y}-W{w:02}"),
            None => "next free slot".to_string(),
        }
    }
}

/// `2026-W41` -> `(2026, 41)`, checked against the calendar.
pub fn parse_week(text: &str) -> Result<(i32, u32), String> {
    let bad = || format!("{text:?} is not an ISO week like 2026-W41");
    let (year, week) = text.split_once("-W").ok_or_else(bad)?;
    let year: i32 = year.parse().map_err(|_| bad())?;
    let week: u32 = week.parse().map_err(|_| bad())?;
    NaiveDate::from_isoywd_opt(year, week, Weekday::Mon).ok_or_else(bad)?;
    Ok((year, week))
}

/// The entry the next run would publish, given the current ISO week.
///
/// A pin to this week or an earlier one goes first (earliest pin, then queue order), so
/// a post that missed its week goes out at the next slot rather than never. Then the
/// unpinned, in the order they were queued. Later pins wait.
pub fn pick<'a>(entries: &'a [Entry], current: (i32, u32)) -> Option<&'a Entry> {
    let mut pinned: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.week.is_some_and(|w| w <= current))
        .collect();
    pinned.sort_by(|a, b| a.week.cmp(&b.week).then_with(|| a.queued_at.cmp(&b.queued_at)));
    if let Some(first) = pinned.first() {
        return Some(first);
    }
    let mut free: Vec<&Entry> = entries.iter().filter(|e| e.week.is_none()).collect();
    free.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));
    free.first().copied()
}

/// This week's slot: the configured weekday at the configured hour, in the week `now`
/// falls in. A run before it does nothing; after it, the week's post goes out on the
/// first run, whichever hour that turns out to be.
pub fn slot(now: DateTime<Tz>, weekday: Weekday, hour: u32, minute: u32) -> DateTime<Tz> {
    let week = now.iso_week();
    let date = NaiveDate::from_isoywd_opt(week.year(), week.week(), weekday).expect("a weekday in a real week");
    let local = date.and_hms_opt(hour, minute, 0).expect("a valid time");
    // A time a DST transition skipped resolves to the moment after the gap.
    now.timezone()
        .from_local_datetime(&local)
        .earliest()
        .unwrap_or_else(|| now.timezone().from_utc_datetime(&(local + chrono::Duration::hours(1))))
}

fn week_of(date: NaiveDate) -> (i32, u32) {
    let w = date.iso_week();
    (w.year(), w.week())
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
                let newline = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
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

/// What the repo already holds, as far as the slot and series rules care.
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
        toml::Value::Datetime(d) => d.date.map(|d| NaiveDate::from_ymd_opt(d.year as i32, d.month as u32, d.day as u32)).flatten(),
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
    Ok(Post { slug: slug.to_string(), date, series, draft })
}

fn published_in_week(posts: &[Post], week: (i32, u32)) -> Vec<&Post> {
    posts
        .iter()
        .filter(|p| !p.draft && p.date.is_some_and(|d| week_of(d) == week))
        .collect()
}

fn read_queue(queue: &Path) -> Result<Vec<Entry>, String> {
    if !queue.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(queue).map_err(|e| format!("Failed to read {}: {e}", queue.display()))? {
        let dir = entry.map_err(|e| e.to_string())?.path();
        let sidecar = dir.join("schedule.toml");
        if !sidecar.is_file() {
            continue;
        }
        let text = fs::read_to_string(&sidecar).map_err(|e| format!("Failed to read {}: {e}", sidecar.display()))?;
        let entry = Entry::parse(&dir, &text)?;
        if !dir.join("post/index.md").is_file() {
            return Err(format!("{}: no post/index.md", dir.display()));
        }
        if dir.file_name().and_then(|n| n.to_str()) != Some(entry.slug.as_str()) {
            return Err(format!("{}: sidecar says slug {}", dir.display(), entry.slug));
        }
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));
    Ok(entries)
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
            let now_flag = args.iter().any(|a| a == "--now");
            let force = args.iter().any(|a| a == "--force");
            let dry = sub == "next" || args.iter().any(|a| a == "--dry-run");
            publish(&config, now_flag, force, dry)
        }
        "list" => {
            let config = Config::load(Path::new(&config_path))?;
            list(&config)
        }
        "unqueue" => {
            let config = Config::load(Path::new(&config_path))?;
            let slug = args.get(1).ok_or("Usage: site-tools publish unqueue <slug>")?;
            let dir = config.queue.join(slug);
            if !dir.join("schedule.toml").is_file() {
                return Err(format!("{slug} is not in the queue"));
            }
            fs::remove_dir_all(&dir).map_err(|e| format!("Failed to remove {}: {e}", dir.display()))?;
            println!("Removed {slug} from the queue.");
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
    eprintln!("site-tools publish — Publish the next queued post (runs on the box, from cron)");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  run [--now] [--force] [--dry-run]   Publish if the week's slot has passed and the week is free");
    eprintln!("                                      --now ignores the hour, --force the one-per-week rule");
    eprintln!("  next                                What `run` would do, changing nothing");
    eprintln!("  list                                The queue, then what `next` says");
    eprintln!("  unqueue <slug>                      Remove an entry");
    eprintln!();
    eprintln!("  --config <path>                     Default {DEFAULT_CONFIG}");
}

fn list(config: &Config) -> Result<(), String> {
    let entries = read_queue(&config.queue)?;
    if entries.is_empty() {
        println!("Queue is empty ({}).", config.queue.display());
    } else {
        println!("Queue ({}):", config.queue.display());
        for e in &entries {
            println!(
                "  {:<40} {:<16} {}  {}  queued {}{}",
                e.slug,
                e.slot_text(),
                if e.send { "newsletter" } else { "no mail   " },
                if e.twir { "twir" } else { "    " },
                e.queued_at,
                e.subject.as_ref().map(|s| format!("  subject: {s}")).unwrap_or_default()
            );
        }
    }
    println!();
    publish(config, false, false, true)
}

fn publish(config: &Config, now_flag: bool, force: bool, dry: bool) -> Result<(), String> {
    let now = Utc::now().with_timezone(&config.timezone);
    let current = (now.iso_week().year(), now.iso_week().week());
    let slot = slot(now, config.weekday, config.hour, config.minute);
    let week_text = format!("{}-W{:02}", current.0, current.1);

    println!("now {}  slot {}", now.format("%Y-%m-%d %H:%M %Z"), slot.format("%a %Y-%m-%d %H:%M"));

    if now < slot && !now_flag {
        println!("{week_text}: before this week's slot; nothing to do.");
        return Ok(());
    }

    if !dry {
        // The clone is a robot's. Whatever it holds, the run starts from the branch it
        // is going to push to.
        git(&config.repo, &["fetch", "--quiet", &config.remote, &config.branch])?;
        git(&config.repo, &["reset", "--hard", "--quiet", &format!("{}/{}", config.remote, config.branch)])?;
        git(&config.repo, &["clean", "-fdq"])?;
    }

    let posts = read_posts(&config.repo)?;
    let taken = published_in_week(&posts, current);
    if taken.len() >= config.max_per_week && !force {
        let names: Vec<&str> = taken.iter().map(|p| p.slug.as_str()).collect();
        println!("{week_text}: already has {} ({}); nothing to do.", taken.len(), names.join(", "));
        return Ok(());
    }

    let entries = read_queue(&config.queue)?;
    let Some(entry) = pick(&entries, current) else {
        if entries.is_empty() {
            println!("{week_text}: queue is empty; nothing to do.");
        } else {
            println!("{week_text}: every queued post is pinned to a later week; nothing to do.");
        }
        return Ok(());
    };

    println!(
        "{week_text}: {} would go out{}{}{}",
        entry.slug,
        if entry.send { " with a newsletter" } else { ", no newsletter" },
        if entry.twir { ", submitted to This Week in Rust" } else { "" },
        if dry { " (dry run)" } else { "" }
    );
    if dry {
        return Ok(());
    }

    // --- Move the bundle in and date it -------------------------------------------
    let date = now.date_naive();
    let dest = config.repo.join("content/blog").join(&entry.slug);
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|e| format!("Failed to clear {}: {e}", dest.display()))?;
    }
    crate::schedule::copy_dir(&entry.dir.join("post"), &dest)?;
    let statics = entry.dir.join("static");
    if statics.is_dir() {
        crate::schedule::copy_dir(&statics, &config.repo.join("static"))?;
    }
    let _ = fs::remove_file(dest.join("schedule.toml"));

    let index = dest.join("index.md");
    let content = fs::read_to_string(&index).map_err(|e| format!("Failed to read {}: {e}", index.display()))?;
    let rewritten = rewrite_frontmatter(&content, date)?;
    let info = post_info(&entry.slug, &rewritten)?;
    for other in posts.iter().filter(|p| p.slug != entry.slug && !p.draft) {
        if other.date == Some(date) && other.series.iter().any(|s| info.series.contains(s)) {
            return Err(format!(
                "{} is in the same series as {} and would share its date {date}; series order is the date",
                entry.slug, other.slug
            ));
        }
    }
    fs::write(&index, &rewritten).map_err(|e| format!("Failed to write {}: {e}", index.display()))?;
    println!("dated {} {}", entry.slug, date);

    // --- Derived files, as build.sh makes them, minus audio and citations ---------
    std::env::set_current_dir(&config.repo).map_err(|e| format!("Failed to enter {}: {e}", config.repo.display()))?;
    let step = |name: &str, r: Result<(), String>| r.map_err(|e| format!("{name} failed, nothing pushed (the next run resets the clone): {e}"));
    step("markdown all", markdown::gen_all())?;
    step("speech all", speech::gen_all())?;
    step("pdf all", pdf::gen_all())?;
    step("og all", og::gen_all())?;
    if entry.send {
        step("newsletter gen", newsletter::gen(&index.to_string_lossy()))?;
    }

    // --- Commit and push ------------------------------------------------------------
    git(&config.repo, &["add", "-A"])?;
    let message = commit_message(entry);
    git(&config.repo, &["commit", "--quiet", "-m", &message])?;
    let hash = git(&config.repo, &["rev-parse", "--short", "HEAD"])?;
    git(&config.repo, &["push", "--quiet", &config.remote, &format!("HEAD:{}", config.branch)])?;
    println!("pushed {} as {}", entry.slug, hash.trim());

    // The entry is out of the queue from here: the post is on its way whatever happens
    // to the mail, and the next run must not publish it twice.
    fs::create_dir_all(&config.archive).map_err(|e| format!("Failed to create {}: {e}", config.archive.display()))?;
    let archived = config.archive.join(&entry.slug);
    if archived.exists() {
        fs::remove_dir_all(&archived).map_err(|e| format!("Failed to clear {}: {e}", archived.display()))?;
    }
    fs::rename(&entry.dir, &archived).map_err(|e| format!("Failed to archive the queue entry: {e}"))?;

    if !entry.send {
        println!("published {} {} commit={} newsletter=no", entry.slug, date, hash.trim());
        return Ok(());
    }

    // --- Wait for Pages, then send ----------------------------------------------------
    let page = format!("{}/blog/{}/", config.site_url, entry.slug);
    let issue = format!("{}/newsletter/{}.md", config.site_url, entry.slug);
    let by_hand = format!(
        "send it by hand once the page is up: {} {}",
        config.send_command.join(" "),
        entry.slug
    );
    wait_for(&[&page, &issue], config.wait_minutes).map_err(|e| format!("published {} but {e}; {by_hand}", entry.slug))?;

    let mut send = Command::new(&config.send_command[0]);
    send.args(&config.send_command[1..]).arg(&entry.slug);
    if let Some(subject) = &entry.subject {
        send.arg("--subject").arg(subject);
    }
    let status = send.status().map_err(|e| format!("published {} but the send command did not run: {e}; {by_hand}", entry.slug))?;
    if !status.success() {
        return Err(format!(
            "published {} but the send exited with {status}; the newsletter's own log says why",
            entry.slug
        ));
    }
    println!("published {} {} commit={} newsletter=sent", entry.slug, date, hash.trim());
    Ok(())
}

/// `Publish: <title>`, plus the syndication trailer when the sidecar asked for it.
/// The trailer is what the `twir` workflow reads off the push; nothing here talks to
/// GitHub's API, so the box needs no token beyond its deploy key.
fn commit_message(entry: &Entry) -> String {
    let title = if entry.title.is_empty() { &entry.slug } else { &entry.title };
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
            return Err(format!("the site did not answer 200 within {minutes} minutes ({})", pending.join(", ")));
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
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-m", "20", "-H", "Cache-Control: no-cache", &busted])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("curl failed: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slug: &str, queued_at: &str, week: Option<(i32, u32)>) -> Entry {
        Entry {
            slug: slug.into(),
            title: slug.into(),
            queued_at: queued_at.into(),
            week,
            send: true,
            subject: None,
            twir: false,
            dir: PathBuf::from(slug),
        }
    }

    #[test]
    fn commit_message_carries_the_trailer_only_when_asked() {
        let mut e = entry("a-post", "2026-09-01T00:00:00Z", None);
        e.title = "A title".into();
        assert_eq!(commit_message(&e), "Publish: A title");
        e.twir = true;
        assert_eq!(commit_message(&e), "Publish: A title\n\nSyndicate: this-week-in-rust\n");
    }

    #[test]
    fn weeks_parse_and_reject() {
        assert_eq!(parse_week("2026-W41").unwrap(), (2026, 41));
        assert_eq!(parse_week("2026-W01").unwrap(), (2026, 1));
        assert!(parse_week("2026-41").is_err());
        assert!(parse_week("2026-W54").is_err());
        assert!(parse_week("2026-W0").is_err());
    }

    #[test]
    fn pinned_to_this_week_or_earlier_beats_the_free_queue() {
        let entries = vec![
            entry("free-first", "2026-09-01T00:00:00Z", None),
            entry("pinned-later", "2026-09-02T00:00:00Z", Some((2026, 45))),
            entry("pinned-now", "2026-09-03T00:00:00Z", Some((2026, 41))),
            entry("pinned-missed", "2026-09-04T00:00:00Z", Some((2026, 40))),
        ];
        assert_eq!(pick(&entries, (2026, 41)).unwrap().slug, "pinned-missed");
        assert_eq!(pick(&entries, (2026, 39)).unwrap().slug, "free-first");
        assert_eq!(pick(&entries[..2], (2026, 41)).unwrap().slug, "free-first");
        assert_eq!(pick(&entries[1..2], (2026, 41)), None);
    }

    #[test]
    fn free_queue_goes_in_queued_order() {
        let entries = vec![
            entry("second", "2026-09-02T00:00:00Z", None),
            entry("first", "2026-09-01T00:00:00Z", None),
        ];
        assert_eq!(pick(&entries, (2026, 41)).unwrap().slug, "first");
    }

    #[test]
    fn slot_is_the_weekday_in_the_current_iso_week() {
        let tz: Tz = "Europe/Oslo".parse().unwrap();
        // Thursday 2026-10-08 is in ISO week 41, whose Tuesday is 2026-10-06.
        let now = tz.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap();
        let s = slot(now, Weekday::Tue, 8, 0);
        assert_eq!(s.format("%Y-%m-%d %H:%M").to_string(), "2026-10-06 08:00");
        assert!(now > s);
        // Monday of the same week, before the slot.
        let monday = tz.with_ymd_and_hms(2026, 10, 5, 9, 0, 0).unwrap();
        assert!(monday < slot(monday, Weekday::Tue, 8, 0));
        // A Sunday belongs to the week that ends on it, not the one that starts after.
        let sunday = tz.with_ymd_and_hms(2026, 10, 11, 9, 0, 0).unwrap();
        assert_eq!(slot(sunday, Weekday::Tue, 8, 0).day(), 6);
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
        assert_eq!(out, "+++\ntitle = \"T\"\ndate = 2026-10-06\n[extra]\ntoc = true\n+++\nBody\n");
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
    fn posts_in_a_week_count_dated_non_drafts() {
        let a = post_info("a", "+++\ntitle = \"A\"\ndate = 2026-10-06\n+++\n").unwrap();
        let b = post_info("b", "+++\ntitle = \"B\"\ndate = \"2026-10-09T10:00:00Z\"\ndraft = true\n+++\n").unwrap();
        let c = post_info("c", "+++\ntitle = \"C\"\ndate = 2026-10-13\n[taxonomies]\nseries = [\"S\"]\n+++\n").unwrap();
        let posts = vec![a, b, c];
        let names: Vec<&str> = published_in_week(&posts, (2026, 41)).iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(names, vec!["a"]);
        assert_eq!(posts[2].series, vec!["S".to_string()]);
        assert!(published_in_week(&posts, (2026, 42)).len() == 1);
    }

    #[test]
    fn config_parses_with_defaults_and_rejects_nonsense() {
        let c = Config::parse("repo = \"/srv/x/site\"\nweekday = \"wed\"\nhour = 7\n").unwrap();
        assert_eq!(c.weekday, Weekday::Wed);
        assert_eq!(c.hour, 7);
        assert_eq!(c.max_per_week, 1);
        assert_eq!(c.archive, PathBuf::from("/srv/lindfors-publisher/published"));
        assert_eq!(c.send_command, vec!["sudo".to_string(), "/opt/lindfors-newsletter/send-issue".to_string()]);
        assert!(Config::parse("weekday = \"someday\"").is_err());
        assert!(Config::parse("timezone = \"Mars/Olympus\"").is_err());
        assert!(Config::parse("hour = 25").is_err());
    }

    #[test]
    fn sidecar_round_trips_through_entry() {
        let text = crate::schedule::sidecar("a-post", "A", "2026-09-03T20:00:00Z", Some("2026-W41"), Some("Subj"), false, true);
        let e = Entry::parse(Path::new("/q/a-post"), &text).unwrap();
        assert_eq!(e.slug, "a-post");
        assert_eq!(e.week, Some((2026, 41)));
        assert!(!e.send);
        assert!(e.twir);
        assert_eq!(e.subject.as_deref(), Some("Subj"));
        assert_eq!(e.slot_text(), "2026-W41");
        // A sidecar from before the key existed is not a submission.
        let old = crate::schedule::sidecar("b", "B", "2026-09-03T20:00:00Z", None, None, true, false);
        assert!(!Entry::parse(Path::new("/q/b"), &old).unwrap().twir);
    }
}
