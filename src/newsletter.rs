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
            "-s", "-X", "POST",
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
        return Err(format!("curl exited with status {}", output.status));
    }

    Ok(())
}

use crate::util::{find_env_file, read_env_var};
