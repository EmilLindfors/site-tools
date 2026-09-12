//! Share images: `static/og/<slug>.png`, 1200x630, one per published post.
//!
//! The templates point `og:image` and `twitter:image` at that path unconditionally, so
//! this has to produce a file for every post, not only the ones with pictures. Three
//! sources, in order of preference:
//!
//! 1. `card.webp` next to the post, drawn by the model with the title in it
//!    (`site-tools hero card`). Re-rendered through Typst, which normalises whatever
//!    size and format the model returned into the PNG the social networks expect.
//! 2. The featured image, with the title, the site and the date composed over a scrim.
//! 3. Neither: the title on the dark palette.
//!
//! Typst does the drawing because it is already a build dependency for the PDFs, reads
//! WebP and JPEG, exports PNG, and has the site's fonts on its `--font-path`. The
//! output is deterministic, so an unchanged post produces a byte-identical file and
//! nothing dirties the repo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::frontmatter;
use crate::hero;
use crate::util;

const WIDTH: u32 = 1200;
const HEIGHT: u32 = 630;

/// Where the picture comes from. The strings are file names inside the temp dir.
enum Source {
    Card(String),
    Hero(String),
    Plain,
}

pub fn gen(post_path: &str) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;
    gen_inner(&root, Path::new(post_path)).map(|_| ())
}

/// One card per published post, and no card for anything else.
pub fn gen_all() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;
    let blog = root.join("content/blog");

    let mut posts: Vec<PathBuf> = fs::read_dir(&blog)
        .map_err(|e| format!("Failed to read {}: {e}", blog.display()))?
        .flatten()
        .map(|e| e.path().join("index.md"))
        .filter(|p| p.is_file())
        .collect();
    posts.sort();

    let mut keep = Vec::new();
    let mut failures = 0;
    for post in &posts {
        match gen_inner(&root, post) {
            Ok(Some(slug)) => keep.push(slug),
            Ok(None) => {}
            Err(e) => {
                eprintln!("  ERROR {}: {e}", post.display());
                failures += 1;
            }
        }
    }

    prune(&root, &keep)?;

    if failures > 0 {
        return Err(format!("{failures} card(s) failed"));
    }
    Ok(())
}

/// Returns the slug when a card was written, None for a skipped draft.
fn gen_inner(root: &Path, post_path: &Path) -> Result<Option<String>, String> {
    let content = fs::read_to_string(post_path)
        .map_err(|e| format!("Failed to read {}: {e}", post_path.display()))?;
    let fm = frontmatter::parse(&content)?;
    let slug = frontmatter::slug_from_path(post_path);

    if fm.draft && std::env::var("INCLUDE_DRAFTS").is_err() {
        return Ok(None);
    }

    let post_dir = post_path
        .parent()
        .ok_or_else(|| format!("{} has no parent", post_path.display()))?;

    let temp_dir = std::env::temp_dir().join(format!("site-tools-og-{slug}"));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Failed to create {}: {e}", temp_dir.display()))?;

    // Pick the source and copy it in next to the .typ, which is where Typst resolves
    // image paths from.
    let source = if let Some(card) = hero::existing_card(post_dir) {
        let name = card.file_name().unwrap().to_string_lossy().into_owned();
        fs::copy(&card, temp_dir.join(&name))
            .map_err(|e| format!("Failed to copy {}: {e}", card.display()))?;
        Source::Card(name)
    } else if let Some(featured) = fm.featured_image.as_deref().filter(|f| !f.is_empty()) {
        let path = post_dir.join(featured);
        if path.is_file() {
            let name = Path::new(featured)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            fs::copy(&path, temp_dir.join(&name))
                .map_err(|e| format!("Failed to copy {}: {e}", path.display()))?;
            Source::Hero(name)
        } else {
            eprintln!("  {slug}: featured image {featured} is missing, composing without it");
            Source::Plain
        }
    } else {
        Source::Plain
    };

    let typ = build_typ(&source, &fm.title, &format_date(&fm.date), "lindfors.no");
    let typ_path = temp_dir.join("card.typ");
    fs::write(&typ_path, typ).map_err(|e| format!("Failed to write card.typ: {e}"))?;

    let out_dir = root.join("static/og");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;
    let out_path = out_dir.join(format!("{slug}.png"));

    // Same reason as pdf.rs: on Windows a file another process has mapped cannot be
    // overwritten in place, but it can be unlinked and created fresh.
    if out_path.exists() {
        fs::remove_file(&out_path)
            .map_err(|e| format!("Failed to replace {}: {e}", out_path.display()))?;
    }

    let status = Command::new("typst")
        .arg("compile")
        .arg("--font-path")
        .arg(root.join("fonts"))
        .args(["--format", "png", "--ppi", "72"])
        .arg(&typ_path)
        .arg(&out_path)
        .status()
        .map_err(|e| format!("Failed to run typst: {e}"))?;
    if !status.success() {
        return Err(format!("typst compile failed with status {status}"));
    }

    if std::env::var_os("SITE_TOOLS_KEEP_TEMP").is_none() {
        let _ = fs::remove_dir_all(&temp_dir);
    } else {
        println!("  temp kept: {}", temp_dir.display());
    }

    let how = match source {
        Source::Card(_) => "from card",
        Source::Hero(_) => "composed over hero",
        Source::Plain => "composed, no image",
    };
    println!("Generated: {} ({how})", out_path.display());
    Ok(Some(slug))
}

/// Remove cards for posts that no longer exist or are drafts again.
fn prune(root: &Path, keep: &[String]) -> Result<(), String> {
    let out_dir = root.join("static/og");
    let Ok(entries) = fs::read_dir(&out_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("png") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !keep.iter().any(|k| *k == stem) {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
            println!("Pruned: {}", path.display());
        }
    }
    Ok(())
}

/// The Typst document for one card. One page, 1pt = 1px at 72 ppi.
fn build_typ(source: &Source, title: &str, date: &str, site: &str) -> String {
    let mut doc = String::new();
    doc.push_str(&format!(
        "#set page(width: {WIDTH}pt, height: {HEIGHT}pt, margin: 0pt, fill: rgb(\"#0E1A20\"))\n"
    ));
    doc.push_str("#set text(font: \"Literata\", fill: rgb(\"#E8F0F0\"))\n\n");

    match source {
        Source::Card(name) => {
            // The model already set the title. Cover-fit crops a 16:9 frame to the
            // slightly wider 1200x630, a few percent off the top and bottom.
            doc.push_str(&format!(
                "#place(top + left, image({}, width: {WIDTH}pt, height: {HEIGHT}pt, fit: \"cover\"))\n",
                typst_str(name)
            ));
        }
        Source::Hero(name) => {
            doc.push_str(&format!(
                "#place(top + left, image({}, width: {WIDTH}pt, height: {HEIGHT}pt, fit: \"cover\"))\n",
                typst_str(name)
            ));
            // Deep Sea, transparent at the top and nearly opaque at the bottom, so the
            // title reads over any picture.
            doc.push_str(&format!(
                "#place(top + left, rect(width: {WIDTH}pt, height: {HEIGHT}pt, \
                 fill: gradient.linear(angle: 90deg, rgb(28, 50, 64, 0), rgb(28, 50, 64, 120), rgb(28, 50, 64, 240))))\n"
            ));
            doc.push_str(&text_block(title, date, site));
        }
        Source::Plain => {
            // A thin coral rule where the picture would have been.
            doc.push_str(&format!(
                "#place(top + left, dx: 72pt, dy: 72pt, rect(width: 96pt, height: 6pt, fill: rgb(\"#F2A07B\")))\n"
            ));
            doc.push_str(&text_block(title, date, site));
        }
    }
    doc
}

fn text_block(title: &str, date: &str, site: &str) -> String {
    format!(
        "#place(bottom + left, dx: 72pt, dy: -64pt, block(width: {}pt)[\n\
         #text(font: \"Inter\", size: 22pt, weight: \"semibold\", fill: rgb(\"#F2A07B\"), {})\n\
         #v(14pt)\n\
         #text(size: {}pt, weight: \"bold\", {})\n\
         #v(16pt)\n\
         #text(font: \"Inter\", size: 20pt, fill: rgb(\"#8BA5A8\"), {})\n\
         ])\n",
        WIDTH - 2 * 72,
        typst_str(site),
        title_size(title),
        typst_str(title),
        typst_str(date),
    )
}

/// Long titles get a smaller face so four lines still fit above the date.
fn title_size(title: &str) -> u32 {
    match title.chars().count() {
        0..=40 => 64,
        41..=70 => 54,
        _ => 46,
    }
}

/// A Typst string literal. Passed as an argument, not inline markup, so `#`, `*`, `_`
/// and `@` in a title are just characters.
fn typst_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `2026-02-23` -> `23 February 2026`. Anything else is passed through.
fn format_date(date: &str) -> String {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return date.to_string();
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        parts[0].parse::<u32>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) else {
        return date.to_string();
    };
    const MONTHS: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August",
        "September", "October", "November", "December",
    ];
    match MONTHS.get((month as usize).wrapping_sub(1)) {
        Some(name) => format!("{day} {name} {year}"),
        None => date.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_string_literals_not_markup() {
        assert_eq!(typst_str("a \"b\" \\ #c *d*"), "\"a \\\"b\\\" \\\\ #c *d*\"");
    }

    #[test]
    fn a_card_source_places_only_the_image() {
        let doc = build_typ(&Source::Card("card.webp".into()), "Title", "1 May 2026", "lindfors.no");
        assert!(doc.contains("image(\"card.webp\""));
        assert!(!doc.contains("Title"), "a model card already carries the title");
    }

    #[test]
    fn a_hero_source_composes_title_site_and_date() {
        let doc = build_typ(&Source::Hero("hero.webp".into()), "Zola has no plugins", "11 February 2026", "lindfors.no");
        assert!(doc.contains("image(\"hero.webp\""));
        assert!(doc.contains("gradient.linear"));
        assert!(doc.contains("\"Zola has no plugins\""));
        assert!(doc.contains("\"11 February 2026\""));
        assert!(doc.contains("\"lindfors.no\""));
    }

    #[test]
    fn no_image_means_no_image_call() {
        let doc = build_typ(&Source::Plain, "T", "d", "s");
        assert!(!doc.contains("image("));
        assert!(doc.contains("\"T\""));
    }

    #[test]
    fn long_titles_shrink() {
        assert_eq!(title_size("Short"), 64);
        assert_eq!(title_size(&"x".repeat(60)), 54);
        assert_eq!(title_size(&"x".repeat(90)), 46);
    }

    #[test]
    fn dates_are_spelled_out() {
        assert_eq!(format_date("2026-02-23"), "23 February 2026");
        assert_eq!(format_date("2026-13-01"), "2026-13-01");
        assert_eq!(format_date("soon"), "soon");
    }
}
