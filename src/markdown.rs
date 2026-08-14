//! Emit a plain-markdown representation of each blog post for content negotiation.
//!
//! The source under `content/blog/` is not servable as-is: it carries TOML
//! frontmatter, Zola component tags, and image paths that are relative to the post
//! directory. This rewrites those into a self-contained document with YAML
//! frontmatter and absolute URLs, written to `static/blog/<slug>.md` so Zola copies
//! it into the build output.
//!
//! Output lands in `static/` rather than `public/` because `public/` is gitignored
//! and Cloudflare Pages runs its own `zola build` — anything generated here has to be
//! committed to survive the trip, the same way post PDFs are.

use std::fs;
use std::path::{Path, PathBuf};

use crate::frontmatter;

const SITE_URL: &str = "https://lindfors.no";

/// Rewrite one markdown link or image target to an absolute URL.
///
/// `post_base` is the post's own URL, used to resolve targets like `hero.webp`
/// that are relative to the post directory.
fn absolutize(target: &str, post_base: &str) -> String {
    let t = target.trim();

    // Already absolute, a fragment, or a non-http scheme: leave alone.
    if t.is_empty()
        || t.starts_with('#')
        || t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("mailto:")
        || t.starts_with("//")
    {
        return target.to_string();
    }

    if let Some(rooted) = t.strip_prefix('/') {
        return format!("{SITE_URL}/{rooted}");
    }

    format!("{post_base}{t}")
}

/// Rewrite every `](target)` in a line. Titles (`](url "title")`) are preserved.
fn rewrite_links(line: &str, post_base: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(open) = rest.find("](") {
        let (before, after) = rest.split_at(open + 2);
        out.push_str(before);

        // Inline targets cannot contain an unescaped ')', so the first one closes it.
        let Some(close) = after.find(')') else {
            out.push_str(after);
            return out;
        };

        let inner = &after[..close];

        // Split off an optional title so it survives untouched.
        let (target, title) = match inner.find(char::is_whitespace) {
            Some(sp) => (&inner[..sp], &inner[sp..]),
            None => (inner, ""),
        };

        out.push_str(&absolutize(target, post_base));
        out.push_str(title);
        out.push(')');

        rest = &after[close + 1..];
    }

    out.push_str(rest);
    out
}

/// True if the line opens or closes a fenced code block.
///
/// Tracked because several posts quote Tera and markdown syntax inside fences. A
/// stripper that ignored fences would rewrite the examples the posts exist to show.
fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Strip Zola-specific syntax and absolutize URLs, leaving fenced code untouched.
fn clean_body(body: &str, post_base: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_fence = false;

    for line in body.lines() {
        if is_fence(line) {
            in_fence = !in_fence;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_fence {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        let trimmed = line.trim();

        // Zola's excerpt separator has no meaning outside the site.
        if trimmed == "<!-- more -->" {
            continue;
        }

        // Whole-line component/block tags: `{% component ... %}`, `{{< form />}}`.
        if (trimmed.starts_with("{%") && trimmed.ends_with("%}"))
            || (trimmed.starts_with("{{") && trimmed.ends_with("}}"))
        {
            continue;
        }

        out.push_str(&rewrite_links(line, post_base));
        out.push('\n');
    }

    out
}

/// Escape a string for a double-quoted YAML scalar.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render the full markdown document for one post.
fn render(fm: &frontmatter::Frontmatter, body: &str, slug: &str) -> String {
    let post_url = format!("{SITE_URL}/blog/{slug}/");
    let mut out = String::new();

    out.push_str("---\n");
    out.push_str(&format!("title: {}\n", yaml_quote(&fm.title)));
    if !fm.description.is_empty() {
        out.push_str(&format!("description: {}\n", yaml_quote(&fm.description)));
    }
    if !fm.date.is_empty() {
        out.push_str(&format!("date: {}\n", fm.date));
    }
    if !fm.tags.is_empty() {
        let tags: Vec<String> = fm.tags.iter().map(|t| yaml_quote(t)).collect();
        out.push_str(&format!("tags: [{}]\n", tags.join(", ")));
    }
    out.push_str(&format!("author: {}\n", yaml_quote("Emil Lindfors")));
    out.push_str(&format!("canonical: {post_url}\n"));
    out.push_str("---\n\n");

    // The HTML page renders the title from frontmatter, so the body starts at h2.
    // A standalone document needs the h1 put back.
    out.push_str(&format!("# {}\n\n", fm.title));

    out.push_str(clean_body(body, &post_url).trim());
    out.push('\n');

    out
}

/// A published post, as listed in llms.txt.
struct PostRef {
    slug: String,
    title: String,
    description: String,
    date: String,
}

/// Generate `static/blog/<slug>.md` for one post. Drafts are skipped.
pub fn gen(post_path: &str) -> Result<(), String> {
    gen_inner(post_path).map(|_| ())
}

/// Returns the post's index entry, or None when it was skipped as a draft.
fn gen_inner(post_path: &str) -> Result<Option<PostRef>, String> {
    let path = Path::new(post_path);
    if !path.exists() {
        return Err(format!("File not found: {post_path}"));
    }

    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {post_path}: {e}"))?;

    let fm = frontmatter::parse(&content)?;
    let slug = frontmatter::slug_from_path(path);

    if fm.draft && std::env::var("INCLUDE_DRAFTS").is_err() {
        println!("Skipping {slug} (draft)");
        return Ok(None);
    }

    let (_, body) = frontmatter::split(&content)?;
    let document = render(&fm, body, &slug);

    let root = crate::util::find_project_root(path)?;
    let out_dir = root.join("static/blog");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    let out_path = out_dir.join(format!("{slug}.md"));
    fs::write(&out_path, &document)
        .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;

    println!("Markdown: {}", out_path.display());

    Ok(Some(PostRef {
        slug,
        title: fm.title,
        description: fm.description,
        date: fm.date,
    }))
}

/// Generate markdown for every post, and prune files for posts that no longer exist.
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

    let mut published = Vec::new();
    for post in &posts {
        if let Some(entry) = gen_inner(&post.to_string_lossy())? {
            published.push(entry);
        }
    }

    // Newest first, matching the blog listing.
    published.sort_by(|a, b| b.date.cmp(&a.date));

    prune(&root, &posts)?;
    write_llms_txt(&root, &published)?;
    Ok(())
}

/// Write `static/llms.txt` — the index described at https://llmstxt.org/.
///
/// Entries link to the `.md` representations rather than the HTML pages, which is
/// what the proposal asks for when a markdown version exists.
fn write_llms_txt(root: &Path, posts: &[PostRef]) -> Result<(), String> {
    let mut out = String::new();

    out.push_str("# lindfors.no\n\n");
    out.push_str(
        "> Personal site of Emil Lindfors — PhD in technological innovation and senior \
         software engineer in Norway. Writing on Rust, aquaculture technology, sensor \
         systems, and self-hosted infrastructure.\n\n",
    );
    out.push_str(
        "Every post is available as markdown: append `.md` to its slug, or request the \
         post URL with `Accept: text/markdown`. Content may be used as a reference with \
         attribution; it is not licensed for model training.\n\n",
    );

    out.push_str("## Blog posts\n\n");
    for post in posts {
        out.push_str(&format!(
            "- [{}]({SITE_URL}/blog/{}.md)",
            post.title, post.slug
        ));
        if !post.description.is_empty() {
            out.push_str(&format!(": {}", post.description));
        }
        out.push('\n');
    }

    out.push_str("\n## Pages\n\n");
    out.push_str(&format!("- [About]({SITE_URL}/about/): Background, research, and contact details.\n"));
    out.push_str(&format!("- [Blog index]({SITE_URL}/blog/): All posts, newest first.\n"));
    out.push_str(&format!("- [Atom feed]({SITE_URL}/atom.xml): Full-text feed of new posts.\n"));

    let out_path = root.join("static/llms.txt");
    fs::write(&out_path, &out)
        .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;

    println!("Index: {} ({} posts)", out_path.display(), posts.len());
    Ok(())
}

/// Delete generated markdown whose source post is gone or has become a draft.
///
/// Without this an unpublished post keeps serving its full text at the `.md` URL
/// long after the HTML page stops existing.
fn prune(root: &Path, posts: &[PathBuf]) -> Result<(), String> {
    let out_dir = root.join("static/blog");
    if !out_dir.is_dir() {
        return Ok(());
    }

    let live: Vec<String> = posts
        .iter()
        .filter(|p| {
            fs::read_to_string(p)
                .ok()
                .and_then(|c| frontmatter::parse(&c).ok())
                .is_some_and(|fm| !fm.draft)
        })
        .map(|p| frontmatter::slug_from_path(p))
        .collect();

    for entry in fs::read_dir(&out_dir)
        .map_err(|e| format!("Failed to read {}: {e}", out_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        if !live.contains(&stem) {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
            println!("Removed stale markdown: {}", path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://lindfors.no/blog/a-post/";

    #[test]
    fn relative_image_resolves_against_the_post_directory() {
        let line = "![Rig](sensor-rig.webp)";
        assert_eq!(
            rewrite_links(line, BASE),
            "![Rig](https://lindfors.no/blog/a-post/sensor-rig.webp)"
        );
    }

    #[test]
    fn root_relative_link_gets_the_site_origin() {
        assert_eq!(
            rewrite_links("see [that](/blog/other/)", BASE),
            "see [that](https://lindfors.no/blog/other/)"
        );
    }

    #[test]
    fn absolute_and_fragment_targets_are_untouched() {
        let line = "[ext](https://example.com/x) and [ref](#ref-Smith2020)";
        assert_eq!(rewrite_links(line, BASE), line);
    }

    #[test]
    fn link_title_survives_rewriting() {
        assert_eq!(
            rewrite_links("![x](hero.webp \"A caption\")", BASE),
            "![x](https://lindfors.no/blog/a-post/hero.webp \"A caption\")"
        );
    }

    #[test]
    fn multiple_links_on_one_line_all_rewrite() {
        assert_eq!(
            rewrite_links("[a](/one/) then [b](two.webp)", BASE),
            "[a](https://lindfors.no/one/) then [b](https://lindfors.no/blog/a-post/two.webp)"
        );
    }

    /// The Zola and citations posts quote Tera inside fences. Stripping those tags
    /// would delete the examples the posts are written to show.
    #[test]
    fn tera_inside_a_fence_is_preserved_verbatim() {
        let body = "text\n\n```jinja\n{% component bib.reference(entry) %}\n{{ entry.title }}\n```\n\nmore\n";
        let out = clean_body(body, BASE);
        assert!(out.contains("{% component bib.reference(entry) %}"));
        assert!(out.contains("{{ entry.title }}"));
    }

    #[test]
    fn tera_outside_a_fence_is_stripped() {
        let body = "before\n{{< newsletter.form variant=\"post\" />}}\nafter\n";
        let out = clean_body(body, BASE);
        assert!(!out.contains("newsletter.form"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    #[test]
    fn more_separator_is_dropped() {
        let out = clean_body("intro\n\n<!-- more -->\n\nrest\n", BASE);
        assert!(!out.contains("<!-- more -->"));
        assert!(out.contains("intro") && out.contains("rest"));
    }

    /// A relative link written inside a fence is example text, not a real link.
    #[test]
    fn links_inside_a_fence_are_not_absolutized() {
        let body = "```md\n[example](relative.md)\n```\n";
        assert!(clean_body(body, BASE).contains("[example](relative.md)"));
    }

    #[test]
    fn unclosed_fence_does_not_swallow_the_rest_silently() {
        // Everything after an unterminated fence is treated as code, which is the
        // conservative choice: it copies through rather than being rewritten.
        let body = "```\ncode\n[a](b.webp)\n";
        assert!(clean_body(body, BASE).contains("[a](b.webp)"));
    }

    #[test]
    fn yaml_quotes_are_escaped() {
        assert_eq!(yaml_quote(r#"a "b" c"#), r#""a \"b\" c""#);
    }

    #[test]
    fn document_has_frontmatter_and_restored_h1() {
        let fm = frontmatter::Frontmatter {
            title: "A post".into(),
            date: "2026-02-25".into(),
            description: "Something".into(),
            featured_image: None,
            draft: false,
            tags: vec!["rust".into(), "zola".into()],
        };
        let doc = render(&fm, "\n## Section\n\nBody.\n", "a-post");

        assert!(doc.starts_with("---\ntitle: \"A post\"\n"));
        assert!(doc.contains("tags: [\"rust\", \"zola\"]"));
        assert!(doc.contains("canonical: https://lindfors.no/blog/a-post/"));
        assert!(doc.contains("\n# A post\n"));
        assert!(doc.contains("## Section"));
    }
}
