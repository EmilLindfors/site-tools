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
    let (_, body) = frontmatter::split(&content)?;
    let slug = frontmatter::slug_from_path(path);

    let project_root = crate::util::find_project_root(path)?;
    let post_dir = fs::canonicalize(path)
        .map_err(|e| format!("Failed to resolve {post_path}: {e}"))?;
    let post_dir = post_dir.parent().unwrap();

    println!("Generating PDF for: {slug}");

    // Create temp directory
    let temp_dir = tempdir(&slug)?;

    // Copy supported images (PNG, JPEG, GIF, SVG)
    copy_native_images(post_dir, &temp_dir)?;

    // Convert WebP images to PNG (skip thumbnails)
    convert_webp_images(post_dir, &temp_dir)?;

    // Preprocess markdown body
    let processed = preprocess_body(body);
    let content_path = temp_dir.join("content.md");
    fs::write(&content_path, &processed)
        .map_err(|e| format!("Failed to write content.md: {e}"))?;

    // Format date for display
    let date_display = format_date(&fm.date);

    // Resolve featured image (rewrite .webp -> .png)
    let featured_image = fm.featured_image.as_deref().map(|img| {
        if img.ends_with(".webp") {
            format!("{}.png", &img[..img.len() - 5])
        } else {
            img.to_string()
        }
    });

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

    let font_inter = project_root.join("fonts/inter");
    let font_literata = project_root.join("fonts/literata");

    let status = Command::new("typst")
        .arg("compile")
        .arg("--font-path")
        .arg(&font_inter)
        .arg("--font-path")
        .arg(&font_literata)
        .arg(&doc_path)
        .arg(&output_path)
        .status()
        .map_err(|e| format!("Failed to run typst: {e}"))?;

    if !status.success() {
        return Err(format!("typst compile failed with status {status}"));
    }

    // Clean up temp dir
    let _ = fs::remove_dir_all(&temp_dir);

    println!("Generated: {}", output_path.display());
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

/// Copy PNG, JPEG, GIF, SVG files from post directory to temp directory.
fn copy_native_images(post_dir: &Path, temp_dir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(post_dir)
        .map_err(|e| format!("Failed to read {}: {e}", post_dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        match ext.as_deref() {
            Some("png" | "jpg" | "jpeg" | "gif" | "svg") => {
                let dest = temp_dir.join(path.file_name().unwrap());
                fs::copy(&path, &dest).map_err(|e| {
                    format!("Failed to copy {}: {e}", path.display())
                })?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Convert WebP images to PNG using the `image` crate. Skip thumbnails.
fn convert_webp_images(post_dir: &Path, temp_dir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(post_dir)
        .map_err(|e| format!("Failed to read {}: {e}", post_dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        if ext.as_deref() != Some("webp") {
            continue;
        }

        let file_name = path.file_name().unwrap().to_string_lossy();

        // Skip thumbnails
        if file_name.contains("-thumb") {
            continue;
        }

        let stem = path.file_stem().unwrap().to_string_lossy();
        let out_name = format!("{stem}.png");

        match image::open(&path) {
            Ok(img) => {
                let out_path = temp_dir.join(&out_name);
                img.save(&out_path).map_err(|e| {
                    format!("Failed to save {out_name}: {e}")
                })?;
                println!("  Converted {file_name} -> {out_name}");
            }
            Err(e) => {
                eprintln!("  Warning: failed to convert {file_name}: {e}");
            }
        }
    }

    Ok(())
}

/// Preprocess markdown body for Typst compatibility.
fn preprocess_body(body: &str) -> String {
    let mut lines: Vec<String> = Vec::new();

    for line in body.lines() {
        // Strip <!-- more --> separator
        if line.trim() == "<!-- more -->" {
            continue;
        }

        let mut line = line.to_string();

        // Rewrite .webp image references to .png
        line = line.replace(".webp)", ".png)");

        // Convert citation links [N](#ref-...) to plain text
        line = replace_citation_links(&line);

        // Convert HTML reference paragraphs to markdown
        line = convert_html_references(&line);

        lines.push(line);
    }

    lines.join("\n")
}

/// Replace `[text](#ref-...)` links with just the text.
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

