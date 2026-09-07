//! Hand a finished post to the publishing queue.
//!
//! The site repo is public, so a post that is written but not yet out cannot sit in it,
//! not even as a draft. The queue is `queue/` in the private lindfors-services repo,
//! checked out beside this one: one directory per post holding `post/` (the page
//! bundle), `static/` (audio and speech files, if any) and a `schedule.toml` sidecar.
//! Committing and pushing that repo is what queues the post; the publisher on
//! mail.lindfors.no keeps a clone of it and `site-tools publish` there moves one entry
//! into the site on its day, then removes it from the queue with a commit of its own
//! (see `publish.rs`). This is the workstation end: it stages the entry into the queue
//! directory and says what to commit.
//!
//! Nothing here touches the local copy of the post. `git pull` after the publish
//! overwrites it with the published one at the same path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{codemask, frontmatter, markers, publish, util};

/// The queue directory, `QUEUE_DIR` in the environment or `.env`; otherwise the
/// lindfors-services checkout beside the site.
pub fn queue_dir(root: &Path) -> PathBuf {
    match util::setting(root, "QUEUE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => root.parent().map(|p| p.join("lindfors-services")).unwrap_or_else(|| root.join("..")).join("queue"),
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let Some(first) = args.first() else {
        print_usage();
        return Err("missing argument".to_string());
    };

    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;
    let queue = queue_dir(&root);

    match first.as_str() {
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        "list" => list(&queue),
        "remove" | "unschedule" => {
            let slug = args.get(1).ok_or("Usage: site-tools schedule remove <slug>")?;
            check_slug(slug)?;
            remove(&queue, slug)
        }
        slug => {
            let week = crate::parse_flag(&args[1..], "--week");
            let subject = crate::parse_flag(&args[1..], "--subject");
            let send = !args[1..].iter().any(|a| a == "--no-send");
            let twir = args[1..].iter().any(|a| a == "--twir");
            add(&root, &queue, slug, week.as_deref(), subject.as_deref(), send, twir)
        }
    }
}

fn print_usage() {
    eprintln!("site-tools schedule — Queue a post for publishing");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  <slug> [--week YYYY-Www] [--no-send] [--subject ...] [--twir]");
    eprintln!("                    Copy content/blog/<slug>/ and its audio into the queue");
    eprintln!("                    --twir: submit it to This Week in Rust when it goes out");
    eprintln!("  list              What is queued, and what is not yet committed");
    eprintln!("  remove <slug>     Take a post back out of the queue");
    eprintln!();
    eprintln!("The post must be a draft with its citations resolved. No --week means the");
    eprintln!("next free slot, in the order things were queued. The queue is the queue/");
    eprintln!("directory of the lindfors-services repo; commit and push it to queue the post.");
    eprintln!();
    eprintln!("Settings (environment or .env): QUEUE_DIR (../lindfors-services/queue)");
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

/// The queue directory must be inside a checkout: the commit is the queueing.
/// Returns the checkout's root.
fn check_queue(queue: &Path) -> Result<PathBuf, String> {
    let repo = queue.parent().ok_or_else(|| format!("{} has no parent", queue.display()))?;
    if !repo.join(".git").exists() {
        return Err(format!(
            "{} is not inside a git checkout. Clone lindfors-services beside the site, or set QUEUE_DIR.",
            shown(queue)
        ));
    }
    fs::canonicalize(repo).map_err(|e| format!("Failed to resolve {}: {e}", repo.display()))
}

/// A path for a message: Windows' canonical `\\?\` prefix does not belong in one.
fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").map(String::from).unwrap_or(text)
}

fn add(root: &Path, queue: &Path, slug: &str, week: Option<&str>, subject: Option<&str>, send: bool, twir: bool) -> Result<(), String> {
    check_slug(slug)?;
    if let Some(week) = week {
        publish::parse_week(week)?;
    }
    let repo = check_queue(queue)?;

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

    // The entry as the queue holds it: post/, static/, schedule.toml. A re-queue
    // replaces the old copy whole instead of merging into it.
    let entry = queue.join(slug);
    if entry.exists() {
        fs::remove_dir_all(&entry).map_err(|e| format!("Failed to clear {}: {e}", entry.display()))?;
    }
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

    println!("Queued {slug}: {}", fm.title);
    match week {
        Some(w) => println!("  week: {w}"),
        None => println!("  week: next free slot"),
    }
    println!("  newsletter: {}", if send { "yes" } else { "no" });
    println!("  this week in rust: {}", if twir { "yes" } else { "no" });
    for rel in &extras {
        println!("  with {rel}");
    }
    println!("  in {}", shown(&entry));
    println!();
    println!("It is queued once it is pushed:");
    let r = shown(&repo);
    println!("  git -C {r} add queue/{slug} && git -C {r} commit -m 'Queue: {slug}' && git -C {r} push");
    Ok(())
}

fn remove(queue: &Path, slug: &str) -> Result<(), String> {
    let repo = check_queue(queue)?;
    let entry = queue.join(slug);
    if !entry.join("schedule.toml").is_file() {
        return Err(format!("{slug} is not in the queue ({})", shown(queue)));
    }
    fs::remove_dir_all(&entry).map_err(|e| format!("Failed to remove {}: {e}", entry.display()))?;
    println!("Removed {slug} from {}.", shown(queue));
    println!("It is out of the queue once that is pushed:");
    let r = shown(&repo);
    println!("  git -C {r} add -A queue && git -C {r} commit -m 'Unqueue: {slug}' && git -C {r} push");
    Ok(())
}

/// The queue as the checkout holds it, with a mark on what the box cannot see yet.
fn list(queue: &Path) -> Result<(), String> {
    let repo = check_queue(queue)?;
    let entries = publish::read_queue(queue)?;
    let pending = uncommitted(&repo, queue);
    println!("Queue ({}):", shown(queue));
    if entries.is_empty() {
        println!("  (empty)");
    }
    for e in &entries {
        println!(
            "  {:<40} {:<16} {}  {}  queued {}{}{}",
            e.slug,
            e.slot_text(),
            if e.send { "newsletter" } else { "no mail   " },
            if e.twir { "twir" } else { "    " },
            e.queued_at,
            e.subject.as_ref().map(|s| format!("  subject: {s}")).unwrap_or_default(),
            if pending.iter().any(|p| p == &e.slug) { "  NOT PUSHED" } else { "" }
        );
    }
    if !pending.is_empty() {
        println!();
        println!("NOT PUSHED: changed here, not yet in a commit the box can see.");
    }
    println!();
    println!("The box publishes the first pinned entry whose week has come, else the oldest");
    println!("unpinned one, after the week's slot (Tuesday 08:00 Oslo), one a week.");
    Ok(())
}

/// Slugs under the queue with uncommitted changes, or not tracked at all.
fn uncommitted(repo: &Path, queue: &Path) -> Vec<String> {
    let rel = queue.strip_prefix(repo).map(|p| p.to_path_buf()).unwrap_or_else(|_| PathBuf::from("queue"));
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all", "--"])
        .arg(&rel)
        .current_dir(repo)
        .output();
    let Ok(output) = output else { return Vec::new() };
    let prefix = format!("{}/", rel.to_string_lossy().replace('\\', "/"));
    let mut slugs: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.get(3..))
        .filter_map(|path| path.strip_prefix(&prefix))
        .filter_map(|rest| rest.split('/').next())
        .map(String::from)
        .collect();
    slugs.sort();
    slugs.dedup();
    slugs
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
    fn slugs_are_checked() {
        assert!(check_slug("newsletter-on-my-own-server").is_ok());
        assert!(check_slug("Bad Slug").is_err());
        assert!(check_slug("../etc").is_err());
        assert!(check_slug("").is_err());
    }

    #[test]
    fn the_queue_defaults_to_the_services_checkout_beside_the_site() {
        // No QUEUE_DIR in this test's environment or the site's .env.
        std::env::remove_var("QUEUE_DIR");
        let root = Path::new("/home/me/dev/lindfors-site");
        assert_eq!(queue_dir(root), PathBuf::from("/home/me/dev/lindfors-services/queue"));
    }
}
