use std::path::Path;

/// Parsed Zola frontmatter fields.
pub struct Frontmatter {
    pub title: String,
    pub date: String,
    pub description: String,
    pub featured_image: Option<String>,
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

    Ok(Frontmatter {
        title,
        date,
        description,
        featured_image,
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
