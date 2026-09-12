use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::frontmatter;

/// Generate a PDF from a blog post using Typst.
pub fn gen(post_path: &str) -> Result<(), String> {
    let path = Path::new(post_path);
    if !path.exists() {
        return Err(format!("File not found: {post_path}"));
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {post_path}: {e}"))?;

    let fm = frontmatter::parse(&content)?;
    let (toml_str, body) = frontmatter::split(&content)?;
    let references = crate::bib::references_markdown(toml_str);
    let slug = frontmatter::slug_from_path(path);

    // Drafts are unpublished, but static/pdf/<slug>.pdf is served at a guessable URL,
    // so generating one would put unpublished writing on the public site.
    if fm.draft && std::env::var_os("INCLUDE_DRAFTS").is_none() {
        println!("Skipping draft: {slug}");
        return Ok(());
    }

    let project_root = crate::util::find_project_root(path)?;
    let post_dir = fs::canonicalize(path)
        .map_err(|e| format!("Failed to resolve {post_path}: {e}"))?;
    let post_dir = post_dir.parent().unwrap();

    println!("Generating PDF for: {slug}");

    // Create temp directory
    let temp_dir = tempdir(&slug)?;

    // Copy images. Typst reads WebP natively, so nothing is converted.
    copy_images(post_dir, &temp_dir)?;

    // Preprocess markdown body. References live in the frontmatter, where Typst cannot
    // see them, so the section the web page renders from a template is appended here.
    let processed = preprocess_body(body, references.as_deref());
    let content_path = temp_dir.join("content.md");
    fs::write(&content_path, &processed)
        .map_err(|e| format!("Failed to write content.md: {e}"))?;

    // Format date for display
    let date_display = format_date(&fm.date);

    // Featured image is used as-is; Typst handles WebP.
    let featured_image = fm.featured_image.clone();

    // Check if featured image exists in temp dir
    let use_featured = featured_image
        .as_ref()
        .map(|img| temp_dir.join(img).exists())
        .unwrap_or(false);

    // Write document.typ
    let document_typ = build_document_typ(&fm.title, &date_display, if use_featured {
        featured_image.as_deref()
    } else {
        None
    });
    let doc_path = temp_dir.join("document.typ");
    fs::write(&doc_path, &document_typ)
        .map_err(|e| format!("Failed to write document.typ: {e}"))?;

    // Copy academic.typ template
    let template_src = project_root.join("templates/pdf/academic.typ");
    if !template_src.exists() {
        return Err(format!("Template not found: {}", template_src.display()));
    }
    fs::copy(&template_src, temp_dir.join("academic.typ"))
        .map_err(|e| format!("Failed to copy template: {e}"))?;

    // Run typst compile
    let output_dir = project_root.join("static/pdf");
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create {}: {e}", output_dir.display()))?;

    let output_path = output_dir.join(format!("{slug}.pdf"));

    // Remove the previous PDF before writing the new one.
    //
    // On Windows a process that has the file memory-mapped -- the search indexer or a
    // virus scanner, seconds after the last build wrote it -- makes typst fail with
    // "cannot be performed on a file with a user-mapped section open" (os error 1224).
    // Creating the file fresh is allowed where overwriting it in place is not.
    //
    // This matters more than a retry would: `build.sh` downgrades a PDF failure to a
    // warning, so the visible result was a build that said "Done!" while shipping the
    // previous PDF for a post that had changed. An unlinked file that then fails to
    // regenerate is at least an obviously missing one.
    if output_path.exists() {
        fs::remove_file(&output_path)
            .map_err(|e| format!("Failed to replace {}: {e}", output_path.display()))?;
    }

    // One recursive path covers inter, literata, jetbrains-mono and libertinus.
    // Listing subdirectories individually is how JetBrains Mono and Libertinus Serif
    // silently fell back to system fonts.
    let font_path = project_root.join("fonts");

    let mut cmd = Command::new("typst");
    cmd.arg("compile")
        .arg("--font-path")
        .arg(&font_path)
        .arg(&doc_path)
        .arg(&output_path);

    // Typst stamps a CreationDate, so an unchanged post would still produce a
    // different file on every run and dirty the repo. Pin it to the post's own date.
    if let Some(epoch) = date_to_epoch(&fm.date) {
        cmd.env("SOURCE_DATE_EPOCH", epoch.to_string());
    }

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to run typst: {e}"))?;

    if !status.success() {
        return Err(format!("typst compile failed with status {status}"));
    }

    // Clean up temp dir. SITE_TOOLS_KEEP_TEMP=1 leaves the generated content.md and
    // document.typ in place, which is the only way to see what Typst was actually fed.
    if std::env::var_os("SITE_TOOLS_KEEP_TEMP").is_none() {
        let _ = fs::remove_dir_all(&temp_dir);
    } else {
        println!("  temp kept: {}", temp_dir.display());
    }

    println!("Generated: {}", output_path.display());
    Ok(())
}

/// Generate PDFs for every post under content/blog/.
///
/// Drafts are skipped by `gen`, so the loop needs no filter of its own.
pub fn gen_all() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = crate::util::find_project_root(&cwd)?;
    let blog = root.join("content/blog");

    let mut posts: Vec<PathBuf> = fs::read_dir(&blog)
        .map_err(|e| format!("Failed to read {}: {e}", blog.display()))?
        .flatten()
        .map(|e| e.path().join("index.md"))
        .filter(|p| p.is_file())
        .collect();
    posts.sort();

    if posts.is_empty() {
        return Err(format!("No posts found under {}", blog.display()));
    }

    for post in &posts {
        gen(&post.to_string_lossy())?;
    }

    Ok(())
}

/// Create a temp directory for the build.
fn tempdir(slug: &str) -> Result<PathBuf, String> {
    let base = std::env::temp_dir().join(format!("site-tools-pdf-{slug}"));
    if base.exists() {
        fs::remove_dir_all(&base)
            .map_err(|e| format!("Failed to clean temp dir: {e}"))?;
    }
    fs::create_dir_all(&base)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    Ok(base)
}

/// Copy images from the post directory to the temp directory.
///
/// Typst reads WebP natively, so there is no conversion step and no ImageMagick or
/// `image` crate dependency. Thumbnails are for the website, not the PDF.
fn copy_images(post_dir: &Path, temp_dir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(post_dir)
        .map_err(|e| format!("Failed to read {}: {e}", post_dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        if !matches!(
            ext.as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "svg" | "webp")
        ) {
            continue;
        }

        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        if file_name.contains("-thumb") {
            continue;
        }

        fs::copy(&path, temp_dir.join(&file_name))
            .map_err(|e| format!("Failed to copy {}: {e}", path.display()))?;
    }

    Ok(())
}

/// Convert a `YYYY-MM-DD` date to a Unix timestamp (midnight UTC).
///
/// Hand-rolled rather than pulling in a date crate: the only input is a Zola
/// frontmatter date, and this feeds SOURCE_DATE_EPOCH where any stable value derived
/// from the post works. Returns None if the date is not parseable.
fn date_to_epoch(date: &str) -> Option<i64> {
    let head = date.split(|c| c == 'T' || c == ' ').next()?;
    let mut parts = head.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Days since 1970-01-01, via the civil-from-days algorithm (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    Some(days * 86_400)
}

/// Preprocess markdown body for Typst compatibility.
fn preprocess_body(body: &str, references: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::new();

    for line in body.lines() {
        // Strip <!-- more --> separator
        if line.trim() == "<!-- more -->" {
            continue;
        }

        let mut line = line.to_string();

        // Citation links, in both the HTML and the older markdown form, to plain text
        line = crate::bib::strip_citation_anchors(&line);
        line = replace_citation_links(&line);

        // Convert HTML reference paragraphs to markdown
        line = convert_html_references(&line);

        lines.push(line);
    }

    let mut out = lines.join("\n");
    if let Some(references) = references {
        out.push_str("\n\n");
        out.push_str(references);
    }
    out
}

/// Replace `[text](#ref-...)` links with just the text.
///
/// The markdown form is what `cite` used to write. Kept so a post resolved before the
/// switch to HTML anchors still renders a clean PDF.
fn replace_citation_links(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(bracket_start) = rest.find('[') {
        result.push_str(&rest[..bracket_start]);
        let after_bracket = &rest[bracket_start + 1..];

        if let Some(bracket_end) = after_bracket.find("](#ref-") {
            let text = &after_bracket[..bracket_end];
            // Find the closing )
            let link_rest = &after_bracket[bracket_end + 7..]; // skip "](#ref-"
            if let Some(paren_end) = link_rest.find(')') {
                result.push_str(text);
                rest = &link_rest[paren_end + 1..];
                continue;
            }
        }

        // Not a citation link, keep the bracket
        result.push('[');
        rest = after_bracket;
    }

    result.push_str(rest);
    result
}

/// Convert HTML reference paragraphs to plain markdown.
fn convert_html_references(line: &str) -> String {
    if !line.contains("class=\"reference\"") {
        return line.to_string();
    }

    let mut s = line.to_string();

    // <p id="..." class="reference"> -> "- "
    if let Some(start) = s.find("<p ") {
        if let Some(end) = s[start..].find('>') {
            s = format!("- {}", &s[start + end + 1..]);
        }
    }

    // Strip </p>
    s = s.replace("</p>", "");

    // <em>text</em> -> *text*
    s = s.replace("<em>", "*");
    s = s.replace("</em>", "*");

    // <a href="url">text</a> -> [text](url)
    while let Some(a_start) = s.find("<a href=\"") {
        let after = &s[a_start + 9..]; // skip `<a href="`
        if let Some(href_end) = after.find('"') {
            let href = &after[..href_end];
            let rest = &after[href_end + 1..];
            if let Some(tag_end) = rest.find('>') {
                let inner_rest = &rest[tag_end + 1..];
                if let Some(close) = inner_rest.find("</a>") {
                    let text = &inner_rest[..close];
                    let after_close = &inner_rest[close + 4..];
                    s = format!(
                        "{}[{text}]({href}){after_close}",
                        &s[..a_start]
                    );
                    continue;
                }
            }
        }
        break;
    }

    s
}

/// Build the document.typ content.
fn build_document_typ(title: &str, date: &str, featured_image: Option<&str>) -> String {
    let mut doc = String::new();
    doc.push_str("#import \"academic.typ\": academic\n");
    doc.push_str("#import \"@preview/cmarker:0.1.8\"\n");
    doc.push_str("#import \"@preview/mitex:0.2.6\": mitex\n\n");
    doc.push_str("#show: academic.with(\n");
    doc.push_str(&format!("  title: \"{title}\",\n"));
    doc.push_str("  author: \"Emil Lindfors\",\n");
    doc.push_str(&format!("  date: \"{date}\",\n"));

    if let Some(img) = featured_image {
        doc.push_str(&format!("  featured-image: \"{img}\",\n"));
    }

    doc.push_str(")\n\n");
    doc.push_str("#cmarker.render(\n");
    doc.push_str("  read(\"content.md\"),\n");
    doc.push_str("  math: mitex,\n");
    doc.push_str("  smart-punctuation: true,\n");
    doc.push_str(")\n");

    doc
}

/// Format a date string for display (best effort).
fn format_date(date: &str) -> String {
    // Try to parse YYYY-MM-DD and format as "Month DD, YYYY"
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(year), Ok(month), Ok(day)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            let month_name = match month {
                1 => "January",
                2 => "February",
                3 => "March",
                4 => "April",
                5 => "May",
                6 => "June",
                7 => "July",
                8 => "August",
                9 => "September",
                10 => "October",
                11 => "November",
                12 => "December",
                _ => return date.to_string(),
            };
            return format!("{month_name} {day:02}, {year}");
        }
    }

    date.to_string()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_matches_known_dates() {
        assert_eq!(date_to_epoch("1970-01-01"), Some(0));
        assert_eq!(date_to_epoch("2000-03-01"), Some(951_868_800));
        assert_eq!(date_to_epoch("2026-02-25"), Some(1_771_977_600));
    }

    #[test]
    fn epoch_ignores_a_time_component() {
        assert_eq!(date_to_epoch("2026-02-25T13:45:00Z"), date_to_epoch("2026-02-25"));
    }

    #[test]
    fn epoch_rejects_junk() {
        assert_eq!(date_to_epoch(""), None);
        assert_eq!(date_to_epoch("not-a-date"), None);
        assert_eq!(date_to_epoch("2026-13-01"), None);
        assert_eq!(date_to_epoch("2026-01-99"), None);
    }

    /// The same input must always yield the same timestamp, or PDFs stop being
    /// reproducible and every build dirties the repo.
    #[test]
    fn epoch_is_deterministic() {
        assert_eq!(date_to_epoch("2026-02-25"), date_to_epoch("2026-02-25"));
    }

    #[test]
    fn strips_the_more_separator() {
        let out = preprocess_body("intro\n<!-- more -->\nrest\n", None);
        assert!(!out.contains("<!-- more -->"));
        assert!(out.contains("intro") && out.contains("rest"));
    }

    /// WebP references must survive: Typst reads WebP, and rewriting them to .png
    /// pointed at files that no longer get created.
    #[test]
    fn leaves_webp_references_alone() {
        let out = preprocess_body("![hero](hero.webp)\n", None);
        assert!(out.contains("hero.webp"), "got: {out}");
        assert!(!out.contains(".png"));
    }

    #[test]
    fn citation_links_become_plain_text() {
        assert_eq!(replace_citation_links("see [1](#ref-smith2024) here"), "see 1 here");
    }

    #[test]
    fn ordinary_links_are_untouched() {
        let line = "see [the docs](https://example.com) here";
        assert_eq!(replace_citation_links(line), line);
    }

    #[test]
    fn multiple_citations_on_one_line() {
        assert_eq!(
            replace_citation_links("[1](#ref-a) and [2](#ref-b)"),
            "1 and 2"
        );
    }

    #[test]
    fn unmatched_bracket_is_preserved() {
        assert_eq!(replace_citation_links("a [ b"), "a [ b");
    }

    #[test]
    fn html_reference_becomes_markdown() {
        let line = r#"<p id="ref-x" class="reference">Smith. <em>Title</em>. <a href="https://doi.org/10.1/2">doi:10.1/2</a></p>"#;
        let out = convert_html_references(line);
        assert!(out.starts_with("- "), "got: {out}");
        assert!(out.contains("*Title*"));
        assert!(out.contains("[doi:10.1/2](https://doi.org/10.1/2)"));
        assert!(!out.contains("</p>"));
    }

    #[test]
    fn non_reference_html_is_untouched() {
        let line = "<p>ordinary paragraph</p>";
        assert_eq!(convert_html_references(line), line);
    }

    #[test]
    fn date_display_formatting() {
        assert_eq!(format_date("2026-02-25"), "February 25, 2026");
        assert_eq!(format_date("garbage"), "garbage");
    }

    #[test]
    fn document_typ_includes_featured_image_only_when_given() {
        let with = build_document_typ("T", "D", Some("hero.webp"));
        assert!(with.contains("featured-image: \"hero.webp\""));
        let without = build_document_typ("T", "D", None);
        assert!(!without.contains("featured-image"));
    }
}
