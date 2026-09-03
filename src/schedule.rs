//! Hand a finished post to the publishing queue on mail.lindfors.no.
//!
//! The repo is public, so a post that is written but not yet out cannot sit in it, not
//! even as a draft. The queue is a directory of page bundles on the box that already
//! holds the newsletter's state, and `site-tools publish` there moves one into the
//! site on its day (see `publish.rs`). This is the workstation end: it stages the
//! bundle, its audio and speech files if they exist, and a `schedule.toml` sidecar,
//! and streams them over ssh into the queue as the `publisher` account.
//!
//! Nothing here touches the local copy. `git pull` after the publish overwrites it
//! with the published one at the same path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{codemask, frontmatter, markers, util};

/// Where the queue is. Every value is overridable from the environment or `.env`.
pub struct Remote {
    pub host: String,
    pub user: String,
    pub queue: String,
    pub bin: String,
}

impl Remote {
    fn from_settings(root: &Path) -> Remote {
        let get = |key: &str, default: &str| util::setting(root, key).unwrap_or_else(|| default.to_string());
        Remote {
            host: get("PUBLISH_HOST", "hetzner"),
            user: get("PUBLISH_USER", "publisher"),
            queue: get("PUBLISH_QUEUE", "/srv/lindfors-publisher/queue"),
            bin: get("PUBLISH_BIN", "/opt/lindfors-publisher/site-tools"),
        }
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let Some(first) = args.first() else {
        print_usage();
        return Err("missing argument".to_string());
    };

    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;
    let remote = Remote::from_settings(&root);

    match first.as_str() {
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        "list" => remote_publish(&remote, &["list"]),
        "remove" | "unschedule" => {
            let slug = args.get(1).ok_or("Usage: site-tools schedule remove <slug>")?;
            check_slug(slug)?;
            remote_publish(&remote, &["unqueue", slug])
        }
        slug => {
            let week = crate::parse_flag(&args[1..], "--week");
            let subject = crate::parse_flag(&args[1..], "--subject");
            let send = !args[1..].iter().any(|a| a == "--no-send");
            let twir = args[1..].iter().any(|a| a == "--twir");
            add(&root, &remote, slug, week.as_deref(), subject.as_deref(), send, twir)
        }
    }
}

fn print_usage() {
    eprintln!("site-tools schedule — Queue a post for publishing on the box");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  <slug> [--week YYYY-Www] [--no-send] [--subject ...] [--twir]");
    eprintln!("                    Copy content/blog/<slug>/ and its audio to the queue");
    eprintln!("                    --twir: submit it to This Week in Rust when it goes out");
    eprintln!("  list              What is queued, and what the next run would do");
    eprintln!("  remove <slug>     Take a post back out of the queue");
    eprintln!();
    eprintln!("The post must be a draft with its citations resolved. No --week means the");
    eprintln!("next free slot, in the order things were queued.");
    eprintln!();
    eprintln!("Settings (environment or .env): PUBLISH_HOST (hetzner), PUBLISH_USER (publisher),");
    eprintln!("PUBLISH_QUEUE (/srv/lindfors-publisher/queue), PUBLISH_BIN (/opt/lindfors-publisher/site-tools)");
}

fn check_slug(slug: &str) -> Result<(), String> {
    let ok = !slug.is_empty()
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!("{slug:?} is not a slug: lowercase letters, digits and hyphens only"))
    }
}

/// The sidecar that rides with the bundle. `publish` reads it and strips it.
///
/// `twir` is the one promotion decision made here: the publisher turns it into a
/// `Syndicate: this-week-in-rust` trailer on the publish commit, and the `twir`
/// workflow in `.github/workflows/` opens the pull request on that push. A `rust` tag
/// does not make a post Rust content (the analytics post has one and is a JavaScript
/// loader), so this is a flag per post, not a rule.
pub fn sidecar(slug: &str, title: &str, queued_at: &str, week: Option<&str>, subject: Option<&str>, send: bool, twir: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("slug = {}\n", toml_string(slug)));
    out.push_str(&format!("title = {}\n", toml_string(title)));
    out.push_str(&format!("queued_at = {}\n", toml_string(queued_at)));
    if let Some(week) = week {
        out.push_str(&format!("week = {}\n", toml_string(week)));
    }
    out.push_str(&format!("send = {send}\n"));
    if let Some(subject) = subject {
        out.push_str(&format!("subject = {}\n", toml_string(subject)));
    }
    if twir {
        out.push_str("twir = true\n");
    }
    out
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn add(root: &Path, remote: &Remote, slug: &str, week: Option<&str>, subject: Option<&str>, send: bool, twir: bool) -> Result<(), String> {
    check_slug(slug)?;
    if let Some(week) = week {
        crate::publish::parse_week(week)?;
    }

    let bundle = root.join("content/blog").join(slug);
    let index = bundle.join("index.md");
    if !index.is_file() {
        return Err(format!("{} does not exist", index.display()));
    }
    let content = fs::read_to_string(&index).map_err(|e| format!("Failed to read {}: {e}", index.display()))?;
    let fm = frontmatter::parse(&content)?;

    // Three refusals, all about what would otherwise go out wrong.
    if !fm.draft {
        return Err(format!(
            "{slug} is not a draft. A post to be scheduled has `draft = true`; the publisher removes it on the day."
        ));
    }
    let (_, body) = frontmatter::split(&content)?;
    let (masked, _) = codemask::mask(body);
    let pending = markers::scan(&masked);
    if !pending.is_empty() {
        return Err(format!(
            "{slug} still has {} unresolved citation marker(s); run `site-tools cite all` first. \
             The publisher is built without the cite feature.",
            pending.len()
        ));
    }
    if fm.title.is_empty() || fm.description.is_empty() {
        return Err(format!("{slug} needs a title and a description before it is queued"));
    }

    // The repo is public. A post already tracked by git is already out, whatever
    // `draft` says; scheduling it changes nothing about that.
    if tracked_in_git(root, &bundle) {
        eprintln!(
            "Warning: content/blog/{slug}/ is tracked by git. Queueing it does not take it out of the \
             repo's history; do that by hand before pushing."
        );
    }

    let queued_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let sidecar = sidecar(slug, &fm.title, &queued_at, week, subject, send, twir);

    // Stage the entry as the queue will hold it: post/, static/, schedule.toml.
    let staging = std::env::temp_dir().join(format!("site-tools-schedule-{slug}"));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("Failed to clean {}: {e}", staging.display()))?;
    }
    let entry = staging.join(slug);
    copy_dir(&bundle, &entry.join("post"))?;
    let mut extras = Vec::new();
    for rel in [
        format!("static/audio/{slug}.mp3"),
        format!("static/audio/{slug}.json"),
        format!("static/speech/{slug}.txt"),
    ] {
        let src = root.join(&rel);
        if src.is_file() {
            let dst = entry.join(&rel);
            fs::create_dir_all(dst.parent().unwrap()).map_err(|e| format!("Failed to create {}: {e}", dst.display()))?;
            fs::copy(&src, &dst).map_err(|e| format!("Failed to copy {rel}: {e}"))?;
            extras.push(rel);
        }
    }
    fs::write(entry.join("schedule.toml"), &sidecar).map_err(|e| format!("Failed to write the sidecar: {e}"))?;

    println!("Queueing {slug}: {}", fm.title);
    match week {
        Some(w) => println!("  week: {w}"),
        None => println!("  week: next free slot"),
    }
    println!("  newsletter: {}", if send { "yes" } else { "no" });
    println!("  this week in rust: {}", if twir { "yes" } else { "no" });
    for rel in &extras {
        println!("  with {rel}");
    }
    println!("  to {}@{}:{}/{slug}", remote.user, remote.host, remote.queue);

    // tar | ssh sudo -u publisher tar. Two processes rather than scp, so the entry
    // lands owned by the account that will move it, and a re-queue replaces the old
    // copy whole instead of merging into it.
    let mut tar = Command::new("tar")
        .args(["-cf", "-", slug])
        .current_dir(&staging)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run tar: {e}"))?;
    let tar_out = tar.stdout.take().unwrap();

    let script = "set -e; mkdir -p \"$1\"; rm -rf \"$1/$2\"; tar -xf - -C \"$1\"";
    let remote_cmd = format!(
        "sudo -u {} sh -c '{}' sh {} {}",
        shell_word(&remote.user)?,
        script,
        shell_word(&remote.queue)?,
        slug
    );
    let status = Command::new("ssh")
        .arg(&remote.host)
        .arg(&remote_cmd)
        .stdin(Stdio::from(tar_out))
        .status()
        .map_err(|e| format!("Failed to run ssh: {e}"))?;
    let tar_status = tar.wait().map_err(|e| format!("tar did not finish: {e}"))?;
    let _ = fs::remove_dir_all(&staging);

    if !tar_status.success() {
        return Err(format!("tar failed with {tar_status}"));
    }
    if !status.success() {
        return Err(format!("ssh to {} failed with {status}", remote.host));
    }

    println!("Queued. `site-tools schedule list` shows the queue and the next run's pick.");
    Ok(())
}

/// Run `site-tools publish <args>` on the box as the publisher account and stream
/// its output back.
fn remote_publish(remote: &Remote, args: &[&str]) -> Result<(), String> {
    let mut cmd = format!("sudo -u {} {}", shell_word(&remote.user)?, shell_word(&remote.bin)?);
    cmd.push_str(" publish");
    for a in args {
        cmd.push(' ');
        cmd.push_str(&shell_word(a)?);
    }
    let status = Command::new("ssh")
        .arg(&remote.host)
        .arg(&cmd)
        .status()
        .map_err(|e| format!("Failed to run ssh: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ssh to {} failed with {status}", remote.host))
    }
}

/// A word safe to hand to a remote `sh` unquoted. Paths and account names here are
/// plain; anything else is refused rather than escaped.
fn shell_word(s: &str) -> Result<String, String> {
    let ok = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '@' | ':'));
    if ok {
        Ok(s.to_string())
    } else {
        Err(format!("{s:?} contains characters this tool will not pass to a shell"))
    }
}

fn tracked_in_git(root: &Path, dir: &Path) -> bool {
    Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(dir)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

    #[test]
    fn sidecar_carries_the_slot_and_the_send_flag() {
        let s = sidecar("a-post", "A \"quoted\" title", "2026-09-03T20:00:00Z", Some("2026-W41"), None, true, false);
        let table: toml::Table = s.parse().unwrap();
        assert_eq!(table["slug"].as_str(), Some("a-post"));
        assert_eq!(table["title"].as_str(), Some("A \"quoted\" title"));
        assert_eq!(table["week"].as_str(), Some("2026-W41"));
        assert_eq!(table["send"].as_bool(), Some(true));
        assert!(table.get("subject").is_none());
        assert!(table.get("twir").is_none());
    }

    #[test]
    fn sidecar_without_a_week_means_next_free_slot() {
        let s = sidecar("a-post", "T", "2026-09-03T20:00:00Z", None, Some("From the archive"), false, true);
        let table: toml::Table = s.parse().unwrap();
        assert!(table.get("week").is_none());
        assert_eq!(table["send"].as_bool(), Some(false));
        assert_eq!(table["subject"].as_str(), Some("From the archive"));
        assert_eq!(table["twir"].as_bool(), Some(true));
    }

    #[test]
    fn slugs_and_shell_words_are_checked() {
        assert!(check_slug("newsletter-on-my-own-server").is_ok());
        assert!(check_slug("Bad Slug").is_err());
        assert!(check_slug("../etc").is_err());
        assert!(shell_word("/srv/lindfors-publisher/queue").is_ok());
        assert!(shell_word("a b").is_err());
        assert!(shell_word("x;rm").is_err());
    }
}
