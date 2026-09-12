use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::process::Command;

use crate::frontmatter;

const SITE_URL: &str = "https://lindfors.no";

/// Where the newsletter service lives: the send is an operator action, so it is
/// reached through the admin name, behind ADMIN_KEY, and never through the public
/// newsletter.lindfors.no vhost, which routes only subscribe, confirm and unsubscribe.
const ADMIN_URL: &str = "https://admin.lindfors.no";

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
    let (toml_str, body) = frontmatter::split(&content)?;
    let slug = frontmatter::slug_from_path(path);
    let post_url = format!("{SITE_URL}/blog/{slug}/");

    // Same treatment the PDF and the plain-markdown copy already get. An inline citation
    // is an `<a href="#ref-...">`, and an email has no reference list to jump to — so
    // without this the reader gets "Christiansen & Jakobsen (2017)" as a link to nowhere
    // and no way to find out what it refers to.
    let cleaned = tag_links(&crate::bib::strip_citation_anchors(&clean_body(body)), &slug);
    let references = crate::bib::references_markdown(toml_str);
    let tagged_url = tag_url(&post_url, &slug);

    // Build output
    let mut output = String::new();
    output.push_str("---\n");
    output.push_str(&format!("title: \"{}\"\n", fm.title));
    output.push_str(&format!("date: \"{}\"\n", fm.date));
    output.push_str(&format!("description: \"{}\"\n", fm.description));
    output.push_str(&format!("url: \"{tagged_url}\"\n"));
    output.push_str("---\n");
    output.push_str(&cleaned);

    if let Some(references) = &references {
        output.push('\n');
        output.push_str(references);
    }

    // Append footer with link to full post
    output.push_str("\n---\n\n");
    output.push_str(&format!(
        "*[Read the full post on the site]({tagged_url}) for math equations, citations, and interactive features.*\n"
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

/// The query parameter every link into the site carries in an issue: `?issue=<slug>`.
///
/// Feedback per issue, not per reader. A visit that starts from the mail shows up in
/// the RUM `view` rows with the issue in `view_url` and nothing else about who
/// clicked: no open pixel, no per-recipient token, the same link for everyone. The
/// reader-analytics post promised exactly this much and no more.
const ISSUE_PARAM: &str = "issue";

/// `post_url` with the issue parameter, keeping any fragment where it was.
pub fn tag_url(url: &str, issue: &str) -> String {
    let (base, fragment) = match url.find('#') {
        Some(i) => (&url[..i], &url[i..]),
        None => (url, ""),
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}{ISSUE_PARAM}={issue}{fragment}")
}

/// Rewrite the markdown link targets that point into the site: root-relative ones
/// become absolute, since mail has no base URL to resolve `/blog/...` against, and
/// every link to lindfors.no gets the issue parameter. Other hosts, mailto and
/// anchors alone are left as they are; fenced code is masked first.
pub fn tag_links(body: &str, issue: &str) -> String {
    let (masked, spans) = crate::codemask::mask(body);
    let mut out = String::with_capacity(masked.len() + 64);
    let mut rest = masked.as_str();
    while let Some(start) = rest.find("](") {
        let target_start = start + 2;
        let Some(len) = rest[target_start..].find(')') else { break };
        let target = &rest[target_start..target_start + len];
        out.push_str(&rest[..target_start]);
        out.push_str(&retarget(target, issue));
        rest = &rest[target_start + len..];
    }
    out.push_str(rest);
    crate::codemask::unmask(&out, &spans)
}

fn retarget(target: &str, issue: &str) -> String {
    // A title after the URL (`[x](url "title")`) rides along untouched.
    let (url, title) = match target.find(' ') {
        Some(i) => (&target[..i], &target[i..]),
        None => (target, ""),
    };
    let absolute = if url.starts_with('/') && !url.starts_with("//") {
        format!("{SITE_URL}{url}")
    } else {
        url.to_string()
    };
    let ours = absolute.starts_with(&format!("{SITE_URL}/")) || absolute == SITE_URL
        || absolute.starts_with("https://www.lindfors.no/");
    if ours {
        format!("{}{title}", tag_url(&absolute, issue))
    } else {
        target.to_string()
    }
}

/// Send a newsletter via the API.
pub fn send(slug: &str, subject: Option<&str>, catch_up: bool) -> Result<(), String> {
    let project_root = std::env::current_dir()
        .map_err(|e| format!("Failed to get cwd: {e}"))?;
    let env_path = find_env_file(&project_root)?;

    let admin_key = read_env_var(&env_path, "ADMIN_KEY")?;

    // Build JSON body
    // Serialised rather than formatted: a subject with a quote in it must not break
    // the request.
    let mut body = serde_json::json!({ "slug": slug });
    if let Some(s) = subject {
        body["subject"] = serde_json::Value::String(s.to_string());
    }
    if catch_up {
        body["mode"] = serde_json::Value::String("catch-up".to_string());
    }
    let body = body.to_string();

    println!("Newsletter: {slug}");
    if let Some(s) = subject {
        println!("Subject override: {s}");
    }
    if catch_up {
        println!("Catch-up: only subscribers who have not received this issue.");
    }
    println!();
    if catch_up {
        eprint!("Send to the subscribers who have not had it? [y/N] ");
    } else {
        eprint!("Send to all subscribers? [y/N] ");
    }

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
            // and now that a send can *partially* fail (the service answers 502 with
            // the addresses it could not reach), silently exiting 0 would hide the
            // one case that most needs a human. This still prints the body.
            "-s", "--fail-with-body", "-X", "POST",
            &format!("{ADMIN_URL}/api/send-newsletter"),
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
        // The body is already printed above and carries the service's own explanation --
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_post_url_gets_the_issue_parameter() {
        assert_eq!(tag_url("https://lindfors.no/blog/a/", "a"), "https://lindfors.no/blog/a/?issue=a");
        assert_eq!(tag_url("https://lindfors.no/blog/a/#s", "a"), "https://lindfors.no/blog/a/?issue=a#s");
        assert_eq!(tag_url("https://lindfors.no/x?y=1", "a"), "https://lindfors.no/x?y=1&issue=a");
    }

    #[test]
    fn site_links_are_absolute_and_tagged_and_others_are_left_alone() {
        let body = "See [part one](/blog/one/) and [the site](https://lindfors.no/) and \
                    [a paper](https://doi.org/10.1/x) and [mail](mailto:a@b.c) and [here](#top).\n\
                    Also [with title](/blog/two/ \"Two\").\n";
        let out = tag_links(body, "issue-slug");
        assert!(out.contains("[part one](https://lindfors.no/blog/one/?issue=issue-slug)"));
        assert!(out.contains("[the site](https://lindfors.no/?issue=issue-slug)"));
        assert!(out.contains("[a paper](https://doi.org/10.1/x)"));
        assert!(out.contains("[mail](mailto:a@b.c)"));
        assert!(out.contains("[here](#top)"));
        assert!(out.contains("[with title](https://lindfors.no/blog/two/?issue=issue-slug \"Two\")"));
    }

    #[test]
    fn links_inside_code_fences_are_not_touched() {
        let body = "```md\n[x](/blog/one/)\n```\n\n[y](/blog/one/)\n";
        let out = tag_links(body, "s");
        assert!(out.contains("```md\n[x](/blog/one/)\n```"));
        assert!(out.contains("[y](https://lindfors.no/blog/one/?issue=s)"));
    }
}
