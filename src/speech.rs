//! Derive a spoken script from each blog post, for text-to-speech.
//!
//! The markdown a post is written in cannot be read aloud as-is: roughly a third of
//! this blog by volume is code fences, tables, ASCII diagrams and a raw-HTML reference
//! list, none of which produce listenable audio. This strips those to short spoken
//! markers ("Rust code block, 14 lines"), flattens the inline markup, and writes
//! `static/speech/<slug>.txt`.
//!
//! The script is committed rather than generated on the fly so it can be reviewed and
//! diffed — when the audio sounds wrong, this file is where to look. Blocks are
//! separated by one blank line for a paragraph gap and two for a section gap, which is
//! how `audio` decides how much silence to insert.
//!
//! Runs after `cite`, so in-text citations are already rendered to their final form.

use std::fs;
use std::path::{Path, PathBuf};

use crate::frontmatter;

const AUTHOR: &str = "Emil Lindfors";

/// Longest block handed to the TTS API in one request.
///
/// Splitting at sentence boundaries below this keeps each request short enough that an
/// autoregressive model does not drift, and makes a one-paragraph edit re-synthesise
/// one paragraph.
const MAX_BLOCK_CHARS: usize = 600;

/// Spoken out where the written form would be read letter by letter or mispronounced.
///
/// Deliberately short. Anything post-specific belongs in `speech-lexicon.toml`.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("et al.", "and others"),
    ("e.g.", "for example"),
    ("i.e.", "that is"),
    ("vs.", "versus"),
    ("etc.", "and so on"),
    ("cf.", "compare"),
];

/// One spoken block, and whether a section-level pause belongs before it.
struct Block {
    text: String,
    section: bool,
}

/// True if the line opens or closes a fenced code block.
fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// The info string of an opening fence: "```rust" -> "rust".
fn fence_lang(line: &str) -> String {
    line.trim_start()
        .trim_start_matches(['`', '~'])
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Spoken name for a fence language. None when the fence is unlabelled or unknown.
fn language_name(lang: &str) -> Option<&'static str> {
    let name = match lang {
        "rust" | "rs" => "Rust",
        "bash" | "sh" | "shell" | "console" | "zsh" => "Shell",
        "toml" => "TOML",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "html" => "HTML",
        "css" | "scss" | "sass" => "CSS",
        "js" | "javascript" | "ts" | "typescript" => "JavaScript",
        "python" | "py" => "Python",
        "sql" => "SQL",
        "jinja" | "jinja2" | "tera" | "j2" => "Template",
        "nginx" => "Nginx config",
        "typ" | "typst" => "Typst",
        "md" | "markdown" => "Markdown",
        "diff" | "patch" => "Diff",
        "dockerfile" | "docker" => "Dockerfile",
        "ini" | "conf" | "cfg" => "Config",
        _ => return None,
    };
    Some(name)
}

/// True when a fence body is mostly box-drawing characters.
///
/// Several posts draw architecture diagrams inside an unlabelled fence. Calling those
/// "code block" is wrong in a way a listener notices.
fn looks_like_diagram(body: &str) -> bool {
    let drawing = body
        .chars()
        .filter(|c| matches!(c, '─' | '│' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' | '▶' | '◀' | '▲' | '▼' | '━' | '┃' | '╭' | '╮' | '╰' | '╯'))
        .count();

    drawing >= 8 && drawing * 20 >= body.chars().filter(|c| !c.is_whitespace()).count()
}

/// "Rust code block, 14 lines. See the article."
fn code_marker(lang: &str, body: &str, lines: usize) -> String {
    let unit = if lines == 1 { "line" } else { "lines" };

    if looks_like_diagram(body) {
        return format!("Diagram, {lines} {unit}. See the article.");
    }

    match language_name(lang) {
        Some(name) => format!("{name} code block, {lines} {unit}. See the article."),
        None => format!("Code block, {lines} {unit}. See the article."),
    }
}

/// True for a markdown table row. The separator row is counted as layout, not content.
fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Heading text, if the line is an ATX heading.
fn heading_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return None;
    }
    let rest = t.trim_start_matches('#');
    // "#hashtag" is not a heading; ATX requires a space.
    if !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim())
}

/// True for a thematic break: `---`, `***`, `___`.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3
        && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

/// Replace every `![alt](src)` with a spoken figure marker.
///
/// Runs before link flattening: an image is a link with a `!` in front, so the other
/// order would leave a stray `!` and the alt text in place of the marker.
fn flatten_images(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(open) = rest.find("![") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];

        let Some(alt_end) = after.find(']') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let alt = after[..alt_end].trim();

        // The target follows immediately as `(...)`; anything else is not an image.
        let tail = &after[alt_end + 1..];
        let Some(target_end) = tail.strip_prefix('(').and_then(|t| t.find(')')) else {
            out.push_str(&rest[open..open + 2]);
            rest = after;
            continue;
        };

        if !alt.is_empty() {
            out.push_str(&format!("Figure: {}.", alt.trim_end_matches('.')));
        }

        rest = &tail[target_end + 2..];
    }

    out.push_str(rest);
    out
}

/// Replace every `[text](target)` with just the text.
fn flatten_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];

        let Some(text_end) = after.find(']') else {
            out.push_str(&rest[open..]);
            return out;
        };

        let tail = &after[text_end + 1..];
        let Some(target_end) = tail.strip_prefix('(').and_then(|t| t.find(')')) else {
            // A bare `[...]` is not a link. Keep it and move past the bracket.
            out.push('[');
            rest = after;
            continue;
        };

        out.push_str(after[..text_end].trim());
        rest = &tail[target_end + 2..];
    }

    out.push_str(rest);
    out
}

/// Spoken form of an inline code span.
///
/// `client_max_body_size` has to keep its word boundaries — deleting the underscores
/// leaves "clientmaxbodysize", which is unintelligible. Template and HTML syntax is
/// replaced outright: several posts quote Tera inline, and reading `{{<citation
/// key="smith2024" />}}` aloud is noise however it is pronounced.
fn spoken_code(span: &str) -> String {
    if span.contains(['{', '}', '<', '>']) {
        return "this tag".to_string();
    }
    span.replace('_', " ")
}

/// Drop inline markup that carries no sound, and speak inline code.
fn strip_inline_markup(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // Code span: consume to the closing tick and hand the content to
            // spoken_code. An unterminated tick falls through as plain text.
            '`' => {
                let mut span = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '`' {
                        closed = true;
                        break;
                    }
                    span.push(next);
                }
                if closed {
                    out.push_str(&spoken_code(&span));
                } else {
                    out.push_str(&span);
                }
            }
            // Emphasis and leftover link brackets carry no sound.
            '*' | '[' | ']' => continue,
            // `~~struck~~` is markup; a lone `~` is a home directory.
            '~' => {
                if chars.peek() == Some(&'~') {
                    chars.next();
                } else {
                    out.push('~');
                }
            }
            // Emphasis at a word boundary; a word separator inside one.
            '_' => {
                let inside = out.chars().next_back().is_some_and(|p| p.is_alphanumeric())
                    && chars.peek().is_some_and(|n| n.is_alphanumeric());
                if inside {
                    out.push(' ');
                }
            }
            '\\' => {
                // Keep the escaped character, drop the backslash.
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            _ => out.push(c),
        }
    }

    out
}

/// Replace `$$…$$` and `$…$` with a spoken marker.
fn flatten_math(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(open) = rest.find('$') {
        out.push_str(&rest[..open]);
        let delim = if rest[open..].starts_with("$$") { "$$" } else { "$" };
        let after = &rest[open + delim.len()..];

        let Some(close) = after.find(delim) else {
            out.push_str(&rest[open..]);
            return out;
        };

        out.push_str("Equation.");
        rest = &after[close + delim.len()..];
    }

    out.push_str(rest);
    out
}

/// Strip a list bullet or ordered marker from the start of a line.
fn strip_list_marker(line: &str) -> &str {
    let t = line.trim_start();

    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
        return rest;
    }

    // "12. item"
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = t[digits..].strip_prefix(". ") {
            return rest;
        }
    }

    t
}

/// True when the line opens a list item.
fn is_list_item(line: &str) -> bool {
    strip_list_marker(line) != line.trim_start()
}

/// Turn one markdown line into spoken text. Returns None when nothing is left to say.
fn spoken_line(line: &str) -> Option<String> {
    let trimmed = line.trim();

    if trimmed.is_empty() || trimmed == "<!-- more -->" || is_rule(trimmed) {
        return None;
    }

    // Raw HTML: the rendered reference list, and the occasional inline block.
    if trimmed.starts_with('<') {
        return None;
    }

    // Whole-line Tera component and block tags.
    if (trimmed.starts_with("{%") && trimmed.ends_with("%}"))
        || (trimmed.starts_with("{{") && trimmed.ends_with("}}"))
    {
        return None;
    }

    let mut text = trimmed.trim_start_matches("> ").trim_start_matches('>').to_string();
    text = strip_list_marker(&text).to_string();
    text = flatten_math(&text);
    text = flatten_images(&text);
    text = flatten_links(&text);
    text = strip_inline_markup(&text);

    // Some posts write an em dash as `--`, which no TTS model reads as a pause. Only
    // the spaced form: `--profile` is a command-line flag, not punctuation.
    text = text.replace(" -- ", ", ");

    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }

    Some(text)
}

/// A list item without terminal punctuation runs into the next one when spoken.
fn end_sentence(text: &str) -> String {
    if text.ends_with(['.', '!', '?', ':', ';', ',']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

/// Split a paragraph into sentences, keeping the terminating punctuation.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();

    for (i, (offset, c)) in bytes.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }

        // Only a boundary when whitespace follows; "0.158.0" must stay intact.
        let mut end = offset + c.len_utf8();
        let mut j = i + 1;
        while let Some((next_off, next_c)) = bytes.get(j) {
            if matches!(next_c, '"' | '\'' | ')' | '”' | '’') {
                end = next_off + next_c.len_utf8();
                j += 1;
                continue;
            }
            break;
        }

        let followed_by_space = bytes.get(j).is_none_or(|(_, c)| c.is_whitespace());
        if !followed_by_space {
            continue;
        }

        let piece = text[start..end].trim();
        if !piece.is_empty() {
            out.push(piece);
        }
        start = end;
    }

    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }

    out
}

/// Break an over-long paragraph into blocks at sentence boundaries.
fn split_block(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_BLOCK_CHARS {
        return vec![text.to_string()];
    }

    let mut out = Vec::new();
    let mut current = String::new();

    for sentence in split_sentences(text) {
        if !current.is_empty() && current.chars().count() + sentence.chars().count() + 1 > MAX_BLOCK_CHARS {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(sentence);
    }

    if !current.is_empty() {
        out.push(current);
    }

    out
}

/// Accumulates blocks, carrying the pending section gap onto the next one emitted.
#[derive(Default)]
struct Emitter {
    out: Vec<Block>,
    section: bool,
}

impl Emitter {
    fn push(&mut self, text: String) {
        self.out.push(Block {
            text,
            section: self.section,
        });
        self.section = false;
    }

    fn open_section(&mut self) {
        self.section = true;
    }
}

/// The paragraph being accumulated. Lines of a wrapped paragraph join into one block;
/// a list item is its own block so the listener gets a pause between items.
#[derive(Default)]
struct Para {
    lines: Vec<String>,
    list: bool,
}

impl Para {
    fn flush(&mut self, em: &mut Emitter) {
        if self.lines.is_empty() {
            return;
        }

        let text = self.lines.join(" ");
        let text = if self.list { end_sentence(&text) } else { text };
        self.lines.clear();
        self.list = false;

        for piece in split_block(&text) {
            em.push(piece);
        }
    }
}

/// Walk the post body, emitting spoken blocks.
fn blocks(body: &str) -> Vec<Block> {
    let mut em = Emitter::default();
    let mut para = Para::default();
    let mut fence: Option<(String, String, usize)> = None; // (lang, body, lines)
    let mut table_rows = 0usize;

    // Flushing is deferred rather than done inline so a paragraph interrupted by a
    // fence, table or heading is emitted before the marker that interrupted it.
    fn flush_table(rows: &mut usize, em: &mut Emitter) {
        if *rows == 0 {
            return;
        }
        let unit = if *rows == 1 { "row" } else { "rows" };
        em.push(format!("Table, {rows} {unit}. See the article."));
        *rows = 0;
    }

    for line in body.lines() {
        if is_fence(line) {
            match fence.take() {
                Some((lang, fence_body, lines)) => {
                    em.push(code_marker(&lang, &fence_body, lines));
                }
                None => {
                    para.flush(&mut em);
                    flush_table(&mut table_rows, &mut em);
                    fence = Some((fence_lang(line), String::new(), 0));
                }
            }
            continue;
        }

        if let Some((_, fence_body, lines)) = fence.as_mut() {
            fence_body.push_str(line);
            fence_body.push('\n');
            *lines += 1;
            continue;
        }

        if is_table_row(line) {
            para.flush(&mut em);
            if !is_table_separator(line) {
                table_rows += 1;
            }
            continue;
        }
        flush_table(&mut table_rows, &mut em);

        if let Some(text) = heading_text(line) {
            para.flush(&mut em);

            // The reference list is rendered HTML with DOIs and page ranges. Nothing
            // after this heading is worth hearing, and it is always last.
            if text.eq_ignore_ascii_case("References") {
                break;
            }

            // The gap belongs before the heading, not before the paragraph that
            // follows it. An empty heading passes the gap on to the next block.
            em.open_section();
            if let Some(spoken) = spoken_line(text) {
                em.push(end_sentence(&spoken));
            }
            continue;
        }

        if line.trim().is_empty() {
            para.flush(&mut em);
            continue;
        }

        // A new list item ends the previous one, whether or not a blank line separates
        // them. Wrapped continuation lines fall through and join the item.
        if is_list_item(line) {
            para.flush(&mut em);
            para.list = true;
        }

        match spoken_line(line) {
            Some(text) => para.lines.push(text),
            None => para.flush(&mut em),
        }
    }

    para.flush(&mut em);
    flush_table(&mut table_rows, &mut em);

    em.out
}

/// Replace `from` with `to` where `from` is not part of a longer word.
fn replace_word(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(pos) = rest.find(from) {
        let before_ok = rest[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after = &rest[pos + from.len()..];
        let after_ok = after.chars().next().is_none_or(|c| !c.is_alphanumeric());

        out.push_str(&rest[..pos]);
        if before_ok && after_ok {
            out.push_str(to);
        } else {
            out.push_str(from);
        }
        rest = after;
    }

    out.push_str(rest);
    out
}

/// Literal pronunciation overrides from `speech-lexicon.toml`, if present.
///
/// ```toml
/// [say]
/// nginx = "engine ex"
/// ```
fn load_lexicon(root: &Path) -> Vec<(String, String)> {
    let path = root.join("speech-lexicon.toml");
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };

    let Ok(table) = content.parse::<toml::Table>() else {
        eprintln!("  Warning: {} is not valid TOML, ignoring", path.display());
        return Vec::new();
    };

    table
        .get("say")
        .and_then(|v| v.as_table())
        .map(|say| {
            say.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Apply the built-in abbreviations and then the project lexicon.
fn apply_lexicon(text: &str, lexicon: &[(String, String)]) -> String {
    let mut out = text.to_string();

    for (from, to) in ABBREVIATIONS {
        out = out.replace(from, to);
    }
    for (from, to) in lexicon {
        out = replace_word(&out, from, to);
    }

    out
}

/// Render the full spoken script for one post.
fn render(fm: &frontmatter::Frontmatter, body: &str, lexicon: &[(String, String)]) -> String {
    let title = fm.title.trim_end_matches('.');

    let mut blocks = vec![Block {
        text: format!("{title}. By {AUTHOR}."),
        section: false,
    }];
    blocks.extend(self::blocks(body));
    blocks.push(Block {
        text: format!(
            "That was {title}, by {AUTHOR}. The full article, with code and references, \
             is at lindfors dot no."
        ),
        section: true,
    });

    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str(if block.section { "\n\n\n" } else { "\n\n" });
        }
        out.push_str(apply_lexicon(&block.text, lexicon).trim());
    }
    out.push('\n');

    out
}

/// Rough spoken length, for the build log. 150 words per minute is a common narration
/// pace and close enough to sanity-check a script against.
fn estimate_seconds(script: &str) -> usize {
    script.split_whitespace().count() * 60 / 150
}

/// True when the post opts out of audio with `extra.skip_audio`.
///
/// A post whose subject is syntax reads badly however carefully the script is derived:
/// the Zola and citations posts are half template fragments, and every one of them
/// comes out as "this tag". Mirrors the `extra.skip_citations` opt-out in `cite`.
fn skip_audio(content: &str) -> bool {
    let Ok((toml_str, _)) = frontmatter::split(content) else {
        return false;
    };
    let Ok(table) = toml_str.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("extra")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("skip_audio"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Generate `static/speech/<slug>.txt` for one post. Drafts are skipped.
pub fn gen(post_path: &str) -> Result<(), String> {
    gen_inner(post_path).map(|_| ())
}

fn gen_inner(post_path: &str) -> Result<Option<String>, String> {
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

    if skip_audio(&content) {
        println!("Skipping {slug} (extra.skip_audio)");
        return Ok(None);
    }

    let (_, body) = frontmatter::split(&content)?;
    let root = crate::util::find_project_root(path)?;
    let script = render(&fm, body, &load_lexicon(&root));

    let out_dir = root.join("static/speech");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    let out_path = out_dir.join(format!("{slug}.txt"));
    fs::write(&out_path, &script)
        .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;

    let seconds = estimate_seconds(&script);
    println!(
        "Speech: {} ({} blocks, ~{}:{:02})",
        out_path.display(),
        script.split("\n\n").filter(|b| !b.trim().is_empty()).count(),
        seconds / 60,
        seconds % 60
    );

    Ok(Some(slug))
}

/// Generate scripts for every post, and prune those whose post is gone or is a draft.
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

    let mut live = Vec::new();
    for post in &posts {
        if let Some(slug) = gen_inner(&post.to_string_lossy())? {
            live.push(slug);
        }
    }

    prune(&root, &live)
}

/// Delete scripts with no published post behind them.
///
/// Left alone, a script outlives its post and `audio` keeps paying to synthesise a page
/// that no longer exists.
fn prune(root: &Path, live: &[String]) -> Result<(), String> {
    let out_dir = root.join("static/speech");
    if !out_dir.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(&out_dir)
        .map_err(|e| format!("Failed to read {}: {e}", out_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
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
            println!("Removed stale speech script: {}", path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script_of(body: &str) -> String {
        let blocks = blocks(body);
        blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn code_fence_becomes_a_marker_naming_the_language() {
        let out = script_of("Intro.\n\n```rust\nfn main() {}\nlet x = 1;\n```\n\nAfter.\n");
        assert!(out.contains("Rust code block, 2 lines. See the article."));
        assert!(!out.contains("fn main"));
    }

    #[test]
    fn unlabelled_fence_is_still_announced() {
        let out = script_of("```\nsome output\n```\n");
        assert!(out.contains("Code block, 1 line. See the article."));
    }

    /// Several posts draw architecture diagrams in an unlabelled fence. "Code block"
    /// is audibly wrong for those.
    #[test]
    fn box_drawing_fence_is_called_a_diagram() {
        let body = "```\n┌──────────────┐\n│ Codex CLI    │──OTLP──┐\n└──────────────┘        │\n```\n";
        assert!(script_of(body).contains("Diagram, 3 lines."));
    }

    /// The Zola and citations posts quote Tera inside fences. Those must not be
    /// mistaken for real component tags and stripped as such — the whole fence goes.
    #[test]
    fn tera_inside_a_fence_never_reaches_the_script() {
        let body = "Text.\n\n```jinja\n{% component bib.reference(entry) %}\n```\n";
        let out = script_of(body);
        assert!(out.contains("Template code block, 1 line."));
        assert!(!out.contains("component"));
    }

    #[test]
    fn table_becomes_a_row_count() {
        let body = "| Component | Role |\n|---|---|\n| OpenObserve | Store |\n| nginx | TLS |\n";
        let out = script_of(body);
        assert!(out.contains("Table, 3 rows. See the article."), "{out}");
        assert!(!out.contains("OpenObserve"));
    }

    #[test]
    fn heading_is_its_own_block_and_opens_a_section() {
        let out = blocks("## The stack\n\nBody text.\n");
        assert_eq!(out[0].text, "The stack.");
        assert!(out[0].section);
        assert_eq!(out[1].text, "Body text.");
        assert!(!out[1].section);
    }

    /// The rendered reference list is raw HTML with DOIs and page ranges. Reading it
    /// aloud is unlistenable, and it is always the last section.
    #[test]
    fn references_section_and_everything_after_it_is_dropped() {
        let body = "Body.\n\n## References\n\n<p id=\"ref-x\" class=\"reference\">Smith, J. (2020).</p>\n";
        let out = script_of(body);
        assert!(out.contains("Body."));
        assert!(!out.contains("References"));
        assert!(!out.contains("Smith"));
    }

    #[test]
    fn inline_citation_keeps_the_year_and_drops_the_anchor() {
        let out = script_of("Research by Christiansen ([2017](#ref-Christiansen2017)) found it.\n");
        assert_eq!(out, "Research by Christiansen (2017) found it.");
    }

    #[test]
    fn et_al_is_spoken_out() {
        let script = render(
            &frontmatter::Frontmatter {
                title: "T".into(),
                date: String::new(),
                description: String::new(),
                featured_image: None,
                draft: false,
                tags: vec![],
            },
            "Osmundsen et al. describe it.\n",
            &[],
        );
        assert!(script.contains("Osmundsen and others describe it."), "{script}");
    }

    #[test]
    fn image_becomes_a_figure_marker_and_drops_the_path() {
        let out = script_of("![A sensor rig on deck](sensor-rig.webp)\n");
        assert_eq!(out, "Figure: A sensor rig on deck.");
    }

    #[test]
    fn image_without_alt_text_says_nothing() {
        assert_eq!(script_of("Before. ![](hero.webp) After.\n"), "Before. After.");
    }

    #[test]
    fn links_keep_their_text_and_lose_their_target() {
        let out = script_of("See [the fork post](/blog/forking-codex-for-any-endpoint) for it.\n");
        assert_eq!(out, "See the fork post for it.");
    }

    #[test]
    fn emphasis_and_code_ticks_are_stripped() {
        let out = script_of("The **collector** runs `otelcol-contrib` in *release* mode.\n");
        assert_eq!(out, "The collector runs otelcol-contrib in release mode.");
    }

    #[test]
    fn list_items_get_terminal_punctuation() {
        // Without this the items run together into one breathless sentence.
        let out = script_of("1. **Incremental improvements** in feed\n2. Radical innovations\n");
        assert!(out.contains("Incremental improvements in feed"), "{out}");
    }

    /// Deleting the underscores leaves "clientmaxbodysize", which no listener can
    /// reconstruct.
    #[test]
    fn underscores_in_inline_code_become_word_breaks() {
        let out = script_of("Set `client_max_body_size 32m` or it 413s.\n");
        assert_eq!(out, "Set client max body size 32m or it 413s.");
    }

    #[test]
    fn emphasis_underscores_are_still_stripped() {
        assert_eq!(script_of("An _emphasised_ word.\n"), "An emphasised word.");
    }

    /// Two posts quote Tera inline. Reading the braces aloud is noise.
    #[test]
    fn inline_template_syntax_is_replaced_wholesale() {
        let out = script_of("Write `{{<citation key=\"smith2024\" />}}` in a post.\n");
        assert_eq!(out, "Write this tag in a post.");
    }

    #[test]
    fn home_directory_tilde_survives() {
        let out = script_of("Config lives in `~/.fraktal/config.toml` by default.\n");
        assert!(out.contains("~/.fraktal/config.toml"), "{out}");
    }

    #[test]
    fn strikethrough_markers_are_dropped() {
        assert_eq!(script_of("This is ~~wrong~~ right.\n"), "This is wrong right.");
    }

    /// Run together, list items become one breathless sentence. Each is its own block
    /// so the audio step puts a pause between them.
    #[test]
    fn each_list_item_is_its_own_block() {
        let out = blocks("- Incremental improvements\n- Radical innovations\n- Systemic changes\n");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "Incremental improvements.");
        assert_eq!(out[2].text, "Systemic changes.");
    }

    #[test]
    fn a_wrapped_list_item_stays_one_block() {
        let out = blocks("- An item that\n  wraps over two lines\n- Second item\n");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "An item that wraps over two lines.");
    }

    #[test]
    fn spaced_double_hyphen_becomes_a_pause() {
        assert_eq!(script_of("The syntax -- all of it -- changed.\n"), "The syntax, all of it, changed.");
    }

    /// `--profile` is a flag, not punctuation.
    #[test]
    fn command_line_flags_survive_the_dash_rewrite() {
        let out = script_of("Run fraktal --profile openrouter to opt in.\n");
        assert!(out.contains("--profile"), "{out}");
    }

    #[test]
    fn skip_audio_is_read_from_extra() {
        let src = "+++\ntitle = \"T\"\ndate = 2026-01-01\n[extra]\nskip_audio = true\n+++\n\nbody\n";
        assert!(skip_audio(src));
        assert!(!skip_audio("+++\ntitle = \"T\"\ndate = 2026-01-01\n+++\n\nbody\n"));
    }

    #[test]
    fn math_becomes_a_spoken_marker() {
        assert_eq!(script_of("The result $$x = y^2$$ follows.\n"), "The result Equation. follows.");
    }

    #[test]
    fn more_separator_and_rules_are_dropped() {
        let out = script_of("Intro.\n\n<!-- more -->\n\n---\n\nRest.\n");
        assert!(!out.contains("more"));
        assert!(out.contains("Intro.") && out.contains("Rest."));
    }

    #[test]
    fn wrapped_paragraph_lines_join_into_one_block() {
        let out = blocks("A sentence that\nwraps across lines.\n\nNext one.\n");
        assert_eq!(out[0].text, "A sentence that wraps across lines.");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn long_paragraph_splits_at_sentence_boundaries() {
        let sentence = "This is a sentence of some length that carries on for a while. ";
        let body = sentence.repeat(20);
        let out = blocks(&body);

        assert!(out.len() > 1, "should split");
        for block in &out {
            assert!(block.text.chars().count() <= MAX_BLOCK_CHARS, "{}", block.text);
            assert!(block.text.ends_with('.'));
        }
    }

    /// Version strings are full of periods that are not sentence ends.
    #[test]
    fn decimal_numbers_do_not_split_sentences() {
        assert_eq!(split_sentences("Version 0.158.0 shipped."), vec!["Version 0.158.0 shipped."]);
    }

    #[test]
    fn lexicon_replaces_whole_words_only() {
        let lex = vec![("nginx".to_string(), "engine ex".to_string())];
        assert_eq!(apply_lexicon("nginx terminates TLS", &lex), "engine ex terminates TLS");
        assert_eq!(apply_lexicon("nginxfoo stays", &lex), "nginxfoo stays");
    }

    #[test]
    fn script_opens_with_the_title_and_closes_with_a_pointer_back() {
        let fm = frontmatter::Frontmatter {
            title: "Measuring what a coding agent costs".into(),
            date: "2026-08-13".into(),
            description: String::new(),
            featured_image: None,
            draft: false,
            tags: vec![],
        };
        let script = render(&fm, "\n## Section\n\nBody.\n", &[]);

        assert!(script.starts_with("Measuring what a coding agent costs. By Emil Lindfors."));
        assert!(script.trim_end().ends_with("is at lindfors dot no."));
    }

    /// One blank line is a paragraph gap, two is a section gap. `audio` reads the
    /// difference to decide how long to pause.
    #[test]
    fn section_gaps_are_two_blank_lines() {
        let fm = frontmatter::Frontmatter {
            title: "T".into(),
            date: String::new(),
            description: String::new(),
            featured_image: None,
            draft: false,
            tags: vec![],
        };
        let script = render(&fm, "First.\n\n## A heading\n\nSecond.\n", &[]);

        assert!(script.contains("First.\n\n\nA heading."), "{script}");
        assert!(script.contains("A heading.\n\nSecond."), "{script}");
    }
}
