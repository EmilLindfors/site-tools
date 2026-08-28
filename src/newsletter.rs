use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::process::Command;

use crate::frontmatter;

const SITE_URL: &str = "https://lindfors.no";

/// Clean markdown body for email: strip shortcodes, math blocks, etc.
fn clean_body(body: &str) -> String {
    let mut result = String::with_capacity(body.len());

    for line in body.lines() {
        let mut line = line.to_string();

        // Strip figure shortcodes: {{ figure(...) }}
        while let Some(start) = line.find("{{") {
            if let Some(end) = line[start..].find("}}") {
                let inner = &line[start + 2..start + end];
                if inner.trim_start().starts_with("figure(") {
                    line = format!(
                        "{}[Image - view on site]{}",
                        &line[..start],
                        &line[start + end + 2..]
                    );
                    continue;
                }
                if inner.trim_start().starts_with("katex(") {
                    line = format!(
                        "{}[Math equation - view on site]{}",
                        &line[..start],
                        &line[start + end + 2..]
                    );
                    continue;
                }
            }
            break;
        }

        // Strip block-level Tera tags: {% katex ... %}, {% end ... %}
        if line.trim_start().starts_with("{%") && line.contains("%}") {
            continue;
        }

        // Replace $$ math blocks (single-line)
        if line.contains("$$") {
            let mut s = line.as_str();
            let mut out = String::new();
            while let Some(start) = s.find("$$") {
                out.push_str(&s[..start]);
                let rest = &s[start + 2..];
                if let Some(end) = rest.find("$$") {
                    out.push_str("[Math equation - view on site]");
                    s = &rest[end + 2..];
                } else {
                    out.push_str("$$");
                    s = rest;
                }
            }
            out.push_str(s);
            line = out;
        }

        result.push_str(&line);
        result.push('\n');
    }

    result
}

/// Generate a newsletter markdown file from a blog post.
pub fn gen(post_path: &str) -> Result<(), String> {
    let path = Path::new(post_path);
    if !path.exists() {
        return Err(format!("File not found: {post_path}"));
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {post_path}: {e}"))?;

    let fm = frontmatter::parse(&content)?;
    let (_, body) = frontmatter::split(&content)?;
    let slug = frontmatter::slug_from_path(path);
    let post_url = format!("{SITE_URL}/blog/{slug}/");

    let cleaned = clean_body(body);

    // Build output
    let mut output = String::new();
    output.push_str("---\n");
    output.push_str(&format!("title: \"{}\"\n", fm.title));
    output.push_str(&format!("date: \"{}\"\n", fm.date));
    output.push_str(&format!("description: \"{}\"\n", fm.description));
    output.push_str(&format!("url: \"{post_url}\"\n"));
    output.push_str("---\n");
    output.push_str(&cleaned);

    // Append footer with link to full post
    output.push_str("\n---\n\n");
    output.push_str(&format!(
        "*[Read the full post on the site]({post_url}) for math equations, citations, and interactive features.*\n"
    ));

    // Write to static/newsletter/<slug>.md
    let project_root = crate::util::find_project_root(path)?;
    let out_dir = project_root.join("static/newsletter");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    let out_path = out_dir.join(format!("{slug}.md"));
    fs::write(&out_path, &output)
        .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;

    println!("Newsletter generated: {}", out_path.display());
    println!("Slug: {slug}");
    println!();
    println!("Next steps:");
    println!("  1. Deploy site so the .md is available online");
    println!("  2. site-tools newsletter send {slug}");

    Ok(())
}

/// Send a newsletter via the API.
pub fn send(slug: &str, subject: Option<&str>) -> Result<(), String> {
    let project_root = std::env::current_dir()
        .map_err(|e| format!("Failed to get cwd: {e}"))?;
    let env_path = find_env_file(&project_root)?;

    let admin_key = read_env_var(&env_path, "ADMIN_KEY")?;

    // Build JSON body
    let body = match subject {
        Some(s) => format!(r#"{{"slug":"{slug}","subject":"{s}"}}"#),
        None => format!(r#"{{"slug":"{slug}"}}"#),
    };

    println!("Newsletter: {slug}");
    if let Some(s) = subject {
        println!("Subject override: {s}");
    }
    println!();
    eprint!("Send to all subscribers? [y/N] ");

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)
        .map_err(|e| format!("Failed to read stdin: {e}"))?;

    let confirm = line.trim();
    if confirm != "y" && confirm != "Y" {
        println!("Aborted.");
        return Ok(());
    }

    println!("Sending...");

    let output = Command::new("curl")
        .args([
            // --fail-with-body, not bare -s. `curl -s` exits 0 on an HTTP 500, so a
            // send that the Worker rejected outright was reported here as success --
            // and now that a send can *partially* fail (the Worker answers 502 with
            // the addresses it could not reach), silently exiting 0 would hide the
            // one case that most needs a human. This still prints the body.
            "-s", "--fail-with-body", "-X", "POST",
            &format!("{SITE_URL}/api/send-newsletter"),
            "-H", &format!("Authorization: Bearer {admin_key}"),
            "-H", "Content-Type: application/json",
            "-d", &body,
        ])
        .output()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        // Pretty-print JSON if possible
        println!("{stdout}");
    }
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }

    if !output.status.success() {
        // The body is already printed above and carries the Worker's own explanation --
        // a 409 from the send log says the issue was sent and how to clear the claim.
        // Repeating curl's exit code alone would bury that.
        return Err(format!(
            "Send failed ({}); the response above says why",
            output.status
        ));
    }

    Ok(())
}

use crate::util::{find_env_file, read_env_var};

/// Verify that the send log's compare-and-swap actually works, without sending mail.
///
/// The whole idempotency guard rests on one behaviour: Stalwart answering a `PUT` with
/// `If-None-Match: *` with 412 when the file is already there. That is a claim about
/// someone else's server, it is only exercised on a real send, and the cost of it being
/// wrong is mailing everyone twice. So it gets a probe that can be run on demand — after
/// a Stalwart upgrade, or the first time the collection is set up.
///
/// Writes and deletes one throwaway file. Nothing is sent.
pub fn check_sendlog() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let env_path = find_env_file(&cwd)?;

    let base = read_env_var(&env_path, "SEND_LOG_URL")?;
    let user = read_env_var(&env_path, "JMAP_LIST_USER")?;
    let password = read_env_var(&env_path, "JMAP_LIST_PASSWORD")?;

    if user.is_empty() || password.is_empty() {
        return Err(format!(
            "JMAP_LIST_USER and JMAP_LIST_PASSWORD must be set in {} to run this check",
            env_path.display()
        ));
    }

    let base = base.trim_end_matches('/');
    let slug = "zz-sendlog-probe";
    let url = format!("{base}/{slug}.json");

    println!("Send log: {base}");
    println!();

    // 1. Claim a slug nothing has claimed.
    let first = dav(&url, "PUT", &user, &password, Some(r#"{"probe":true}"#), true)?;
    report("claim an unused slug", &["201", "204"], &first);

    // 2. The same claim again. This is the one that matters.
    let second = dav(&url, "PUT", &user, &password, Some(r#"{"probe":true}"#), true)?;
    report("refuse a slug already claimed", &["412"], &second);

    // 3. Clean up, so the probe can run again.
    let cleanup = dav(&url, "DELETE", &user, &password, None, false)?;
    report("delete the probe file", &["200", "204"], &cleanup);

    println!();
    let ok = matches!(first.as_str(), "201" | "204")
        && second == "412"
        && matches!(cleanup.as_str(), "200" | "204");

    if ok {
        println!("Send log is working: a second claim on the same slug is refused.");
        Ok(())
    } else {
        Err("Send log did not behave as the Worker expects. A send would be \
             unguarded, or refused outright — see the statuses above."
            .to_string())
    }
}

/// One authenticated request to the send log. Returns the HTTP status as a string.
fn dav(
    url: &str,
    method: &str,
    user: &str,
    password: &str,
    body: Option<&str>,
    if_none_match: bool,
) -> Result<String, String> {
    // curl writes the body somewhere and the status to stdout. `/dev/null` is not a
    // path on Windows, and `-s` hides the error it produces, so this silently failed
    // with an empty message until it was pinned to the platform's null device.
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };

    let mut cmd = Command::new("curl");
    cmd.args([
        // Status only: the body is not interesting, and a 412 is an expected outcome
        // here rather than an error, so --fail would be exactly wrong.
        "-s", "-o", null_device, "-w", "%{http_code}",
        "-X", method,
        "-u", &format!("{user}:{password}"),
        url,
    ]);
    if if_none_match {
        cmd.args(["-H", "If-None-Match: *"]);
    }
    if let Some(body) = body {
        cmd.args(["-H", "Content-Type: application/json", "-d", body]);
    }

    let output = cmd.output().map_err(|e| format!("Failed to run curl: {e}"))?;
    if !output.status.success() {
        // The exit code matters when stderr is empty, which `-s` makes common.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("curl exited with {} and said nothing", output.status)
        } else {
            format!("curl failed: {detail}")
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn report(what: &str, want: &[&str], got: &str) {
    let ok = want.contains(&got);
    println!(
        "  [{}] {what}  (wanted {}, got {got})",
        if ok { "ok" } else { "FAIL" },
        want.join(" or "),
    );
    if !ok && (got == "401" || got == "403") {
        println!("        JMAP_LIST_USER does not have DAV access to this collection.");
    }
    if !ok && got == "409" {
        println!("        The collection does not exist yet. The Worker MKCOLs it on");
        println!("        first send; create it by hand to probe first.");
    }
}
