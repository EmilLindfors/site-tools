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

/// Byte range of the TOML between the `+++` delimiters.
///
/// `split` trims what it returns, which is right for parsing and wrong for editing: a
/// caller that means to rewrite part of the frontmatter needs to know where it sits in
/// the file so the rest comes back byte for byte.
pub fn bounds(content: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut lines = content.split_inclusive('\n');

    let first = lines.next()?;
    if first.trim_end() != "+++" {
        return None;
    }
    offset += first.len();
    let start = offset;

    for line in lines {
        if line.trim_end() == "+++" {
            return Some((start, offset));
        }
        offset += line.len();
    }
    None
}

/// The `[extra.bib]` map of citation key to DOI.
///
/// This is the whole of what a post has to carry to cite something without a Zotero
/// library: a name for each source and the DOI it stands for.
pub fn bib_map(toml_str: &str) -> std::collections::BTreeMap<String, String> {
    let Ok(table) = toml_str.parse::<toml::Table>() else {
        return Default::default();
    };
    table
        .get("extra")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("bib"))
        .and_then(|v| v.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
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
    fn bounds_cover_the_toml_and_nothing_else() {
        let src = "+++\ntitle = \"T\"\n+++\nbody\n";
        let (start, end) = bounds(src).expect("frontmatter is there");
        assert_eq!(&src[start..end], "title = \"T\"\n");
        assert_eq!(&src[end..], "+++\nbody\n");
    }

    /// A `+++` later in the body is not a delimiter, and a file without frontmatter
    /// has no bounds at all.
    #[test]
    fn bounds_only_read_a_leading_block() {
        assert_eq!(bounds("body\n+++\na = 1\n+++\n"), None);
        assert_eq!(bounds("+++\nunterminated\n"), None);
        assert_eq!(bounds(""), None);
    }

    #[test]
    fn bib_map_reads_key_to_doi() {
        let toml_str = "title = \"T\"\n\n[extra.bib]\nChristiansen2017 = \"10.1016/j.marpol.2016.10.020\"\n";
        let map = bib_map(toml_str);
        assert_eq!(
            map.get("Christiansen2017").map(String::as_str),
            Some("10.1016/j.marpol.2016.10.020")
        );
    }

    #[test]
    fn bib_map_is_empty_when_absent_or_broken() {
        assert!(bib_map("title = \"T\"").is_empty());
        assert!(bib_map("not = = toml").is_empty());
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
