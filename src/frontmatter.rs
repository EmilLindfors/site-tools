use std::path::Path;

/// Parsed Zola frontmatter fields.
pub struct Frontmatter {
    pub title: String,
    pub date: String,
    pub description: String,
    pub featured_image: Option<String>,
    /// Zola treats a missing `draft` as published.
    pub draft: bool,
    pub tags: Vec<String>,
}

/// Split a Zola markdown file on `+++` delimiters.
/// Returns (toml_string, body_string).
pub fn split(content: &str) -> Result<(&str, &str), String> {
    // Zola uses +++ as delimiter. The file starts with +++, then TOML, then +++, then body.
    let mut parts = content.splitn(3, "+++");

    // First part is before the first +++ (should be empty or whitespace)
    let _before = parts.next().unwrap_or("");

    let toml_str = parts
        .next()
        .ok_or("Missing opening +++ delimiter")?
        .trim();

    let body = parts
        .next()
        .ok_or("Missing closing +++ delimiter")?;

    Ok((toml_str, body))
}

/// Parse Zola +++ TOML frontmatter from a markdown file.
pub fn parse(content: &str) -> Result<Frontmatter, String> {
    let (toml_str, _) = split(content)?;

    let table: toml::Table = toml_str
        .parse()
        .map_err(|e| format!("TOML parse error: {e}"))?;

    let title = table
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled")
        .to_string();

    let date = table
        .get("date")
        .map(|v| match v {
            toml::Value::Datetime(d) => d.to_string(),
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();

    let description = table
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let featured_image = table
        .get("extra")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("featured_image"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let draft = table
        .get("draft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tags = table
        .get("taxonomies")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("tags"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    Ok(Frontmatter {
        title,
        date,
        description,
        featured_image,
        draft,
        tags,
    })
}

/// Derive a slug from the post path.
/// `content/blog/my-post/index.md` -> `my-post`
/// `content/blog/my-post.md` -> `my-post`
pub fn slug_from_path(path: &Path) -> String {
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if parent_name == "blog" || parent_name == "content" {
        // Flat file: content/blog/my-post.md
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        // Directory-based: content/blog/my-post/index.md
        parent_name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POST: &str = r#"+++
title = "A post"
date = 2026-02-25
description = "Something"
draft = true

[extra]
featured_image = "hero.webp"
+++

Body text here.
"#;

    #[test]
    fn parses_all_fields() {
        let fm = parse(POST).expect("should parse");
        assert_eq!(fm.title, "A post");
        assert_eq!(fm.date, "2026-02-25");
        assert_eq!(fm.description, "Something");
        assert_eq!(fm.featured_image.as_deref(), Some("hero.webp"));
        assert!(fm.draft);
    }

    #[test]
    fn missing_draft_means_published() {
        let src = "+++\ntitle = \"T\"\ndate = 2026-01-01\n+++\n\nbody\n";
        assert!(!parse(src).unwrap().draft, "absent draft must mean published");
    }

    #[test]
    fn draft_false_is_published() {
        let src = "+++\ntitle = \"T\"\ndate = 2026-01-01\ndraft = false\n+++\n\nbody\n";
        assert!(!parse(src).unwrap().draft);
    }

    /// A quoted "true" is a string, not a bool. Treating it as a draft would be a
    /// guess; Zola itself would reject the file. Make the fallback explicit instead.
    #[test]
    fn non_bool_draft_falls_back_to_published() {
        let src = "+++\ntitle = \"T\"\ndate = 2026-01-01\ndraft = \"true\"\n+++\n\nbody\n";
        assert!(!parse(src).unwrap().draft);
    }

    #[test]
    fn body_is_everything_after_the_second_delimiter() {
        let (toml_str, body) = split(POST).unwrap();
        assert!(toml_str.contains("title = \"A post\""));
        assert_eq!(body.trim(), "Body text here.");
    }

    #[test]
    fn body_containing_plus_delimiters_is_not_truncated() {
        let src = "+++\ntitle = \"T\"\ndate = 2026-01-01\n+++\n\na += b\n\n+++ not frontmatter\n";
        let (_, body) = split(src).unwrap();
        assert!(body.contains("a += b"));
        assert!(body.contains("+++ not frontmatter"));
    }

    #[test]
    fn missing_delimiters_error() {
        assert!(parse("no frontmatter here").is_err());
    }

    #[test]
    fn slug_from_directory_post() {
        assert_eq!(
            slug_from_path(Path::new("content/blog/my-post/index.md")),
            "my-post"
        );
    }

    #[test]
    fn slug_from_flat_post() {
        assert_eq!(slug_from_path(Path::new("content/blog/my-post.md")), "my-post");
    }
}
