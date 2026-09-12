//! Hide code from the citation processor.
//!
//! `zotero-cite` rewrites every `@citekey` it finds, and it does not know what a code
//! block is. That is fine until a post is *about* citations: the examples teaching the
//! `@citekey` syntax are indistinguishable from citations using it, so the processor
//! rewrites the thing it is documenting. `extra.skip_citations` was the stopgap, and it
//! is blunt -- a post that opts out cannot cite anything either.
//!
//! So the code comes out before the processor sees the text and goes back in after.
//! Each fenced block and each inline code span is replaced by a placeholder wrapped in
//! NUL bytes, which cannot occur in a post and carry no `@`.
//!
//! The TOML frontmatter is masked too. It is not markdown, nothing in it is ever a
//! citation, and the processor writes its output into the body and a trailing
//! `## References` section rather than into the frontmatter -- verified against
//! `zotero-cite` before this was added.
//!
//! Indented code blocks are deliberately not masked. Four leading spaces mean "code"
//! only outside a list item and "continuation" inside one, and getting that wrong would
//! hide real prose from the processor -- a silent failure, where the current behaviour
//! is a visible one. Every code block in this repo is fenced.

const SENTINEL: char = '\u{0}';

/// Replace every code span and fenced block with a placeholder.
///
/// Returns the masked text and the original snippets, indexed by their placeholder.
pub fn mask(content: &str) -> (String, Vec<String>) {
    let mut spans: Vec<String> = Vec::new();
    let mut out = String::with_capacity(content.len());
    let mut fence: Option<(char, usize)> = None;
    let mut block = String::new();

    let body = match frontmatter_end(content) {
        Some(end) => {
            push_block(&mut out, &mut spans, content[..end].to_string());
            &content[end..]
        }
        None => content,
    };

    for line in body.split_inclusive('\n') {
        match fence {
            Some((ch, len)) => {
                block.push_str(line);
                if closes_fence(line, ch, len) {
                    push_block(&mut out, &mut spans, std::mem::take(&mut block));
                    fence = None;
                }
            }
            None => match opening_fence(line) {
                Some(f) => {
                    fence = Some(f);
                    block.push_str(line);
                }
                None => mask_inline(line, &mut out, &mut spans),
            },
        }
    }

    // An unterminated fence runs to the end of the file. Markdown renderers treat the
    // rest as code, so this does too rather than handing it back to the processor.
    if !block.is_empty() {
        push_block(&mut out, &mut spans, block);
    }

    (out, spans)
}

/// Put the original code back where `mask` took it out.
pub fn unmask(content: &str, spans: &[String]) -> String {
    if spans.is_empty() {
        return content.to_string();
    }

    let mut out = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(start) = rest.find(SENTINEL) {
        out.push_str(&rest[..start]);
        let after = &rest[start + SENTINEL.len_utf8()..];

        // A lone sentinel is not a placeholder. Nothing writes one, but treating it as
        // literal text is the only behaviour that cannot lose content.
        let Some(end) = after.find(SENTINEL) else {
            out.push(SENTINEL);
            rest = after;
            continue;
        };

        match after[..end].parse::<usize>().ok().and_then(|i| spans.get(i)) {
            Some(code) => out.push_str(code),
            None => {
                out.push(SENTINEL);
                out.push_str(&after[..end]);
                out.push(SENTINEL);
            }
        }
        rest = &after[end + SENTINEL.len_utf8()..];
    }

    out.push_str(rest);
    out
}

/// Store a fenced block and emit its placeholder on a line of its own.
///
/// The block's trailing newline is emitted rather than stored, so the masked text keeps
/// the same line structure as the original and paragraphs around it still parse.
fn push_block(out: &mut String, spans: &mut Vec<String>, mut block: String) {
    let had_newline = block.ends_with('\n');
    if had_newline {
        block.pop();
        if block.ends_with('\r') {
            block.pop();
            out.push_str(&placeholder(spans, block));
            out.push_str("\r\n");
            return;
        }
    }
    out.push_str(&placeholder(spans, block));
    if had_newline {
        out.push('\n');
    }
}

/// Byte offset just past the closing `+++` line of the frontmatter, if there is one.
///
/// The delimiters are inside the masked span, so a body that happens to contain a `+++`
/// line later on is not mistaken for a frontmatter block.
fn frontmatter_end(content: &str) -> Option<usize> {
    let mut lines = content.split_inclusive('\n');

    let first = lines.next()?;
    if first.trim_end() != "+++" {
        return None;
    }
    let mut offset = first.len();

    for line in lines {
        offset += line.len();
        if line.trim_end() == "+++" {
            return Some(offset);
        }
    }
    None
}

fn placeholder(spans: &mut Vec<String>, code: String) -> String {
    spans.push(code);
    format!("{SENTINEL}{}{SENTINEL}", spans.len() - 1)
}

/// The fence character and length if this line opens a fenced block.
///
/// Up to three leading spaces, then three or more backticks or tildes. A backtick fence
/// may not carry a backtick in its info string, which is what keeps `` `a` `b` `` from
/// reading as a fence.
fn opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }

    let ch = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    if ch == '`' && trimmed[len..].contains('`') {
        return None;
    }
    Some((ch, len))
}

/// Whether this line closes a fence opened with `ch` repeated `len` times.
///
/// A closing fence is at least as long as the opening one and carries nothing else.
fn closes_fence(line: &str, ch: char, len: usize) -> bool {
    let trimmed = line.trim_start_matches(' ');
    let run = trimmed.chars().take_while(|c| *c == ch).count();
    run >= len && trimmed[run..].trim().is_empty()
}

/// Mask the inline code spans in one line, copying everything else through.
///
/// A span opens on a run of N backticks and closes on the next run of exactly N. A run
/// with no partner is literal text, which is what a markdown renderer does with it too.
fn mask_inline(line: &str, out: &mut String, spans: &mut Vec<String>) {
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'`' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'`' {
                i += 1;
            }
            out.push_str(&line[start..i]);
            continue;
        }

        let open = i;
        let run = bytes[i..].iter().take_while(|b| **b == b'`').count();
        i += run;

        match closing_run(bytes, i, run) {
            Some(close) => {
                out.push_str(&placeholder(spans, line[open..close + run].to_string()));
                i = close + run;
            }
            None => out.push_str(&line[open..i]),
        }
    }
}

/// Index of the next run of exactly `run` backticks at or after `from`.
fn closing_run(bytes: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let here = bytes[i..].iter().take_while(|b| **b == b'`').count();
        if here == run {
            return Some(i);
        }
        i += here;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: a citekey inside code is invisible to the processor, one in
    /// prose is not.
    #[test]
    fn hides_code_and_keeps_prose() {
        let src = "Cite @Smith2020 like `@Smith2020`.\n";
        let (masked, spans) = mask(src);
        assert!(masked.contains("@Smith2020 like"));
        assert_eq!(masked.matches('@').count(), 1);
        assert_eq!(spans, vec!["`@Smith2020`"]);
        assert_eq!(unmask(&masked, &spans), src);
    }

    #[test]
    fn hides_fenced_blocks() {
        let src = "before\n\n```markdown\nResearch by @Christiansen2017 shows\n```\n\nafter\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(spans.len(), 1);
        assert!(spans[0].contains("@Christiansen2017"));
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// Masking must not join the lines around a block, or the paragraphs either side
    /// of it merge into one.
    #[test]
    fn a_masked_block_keeps_its_own_line() {
        let (masked, _) = mask("a\n```\ncode\n```\nb\n");
        assert_eq!(masked.lines().count(), 3);
        assert_eq!(masked.lines().next(), Some("a"));
        assert_eq!(masked.lines().last(), Some("b"));
    }

    #[test]
    fn tilde_fences_work_too() {
        let src = "~~~\n@key\n~~~\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// A backtick fence cannot be closed by a tilde one, and vice versa.
    #[test]
    fn fences_close_on_their_own_character() {
        let src = "```\n@a\n~~~\n@b\n```\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(spans.len(), 1);
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// Fences inside a longer fence are content, not delimiters -- the pattern every
    /// post that documents markdown uses.
    #[test]
    fn a_longer_fence_contains_shorter_ones() {
        let src = "````\n```\n@key\n```\n````\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(spans.len(), 1);
        assert_eq!(unmask(&masked, &spans), src);
    }

    #[test]
    fn info_strings_are_allowed() {
        let src = "```rust,ignore\n@key\n```\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// Two code spans on one line are two spans, not one span swallowing the prose
    /// between them.
    #[test]
    fn adjacent_spans_do_not_merge() {
        let src = "`@a` and @b and `@c`\n";
        let (masked, spans) = mask(src);
        assert_eq!(spans.len(), 2);
        assert!(masked.contains("and @b and"));
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// A double-backtick span may contain single backticks.
    #[test]
    fn longer_span_delimiters_win() {
        let src = "``a `@key` b``\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(spans, vec!["``a `@key` b``"]);
    }

    /// An unmatched backtick is literal text in markdown, so it must not swallow the
    /// rest of the line here either.
    #[test]
    fn unmatched_backtick_is_literal() {
        let src = "a ` b @key c\n";
        let (masked, spans) = mask(src);
        assert!(spans.is_empty());
        assert_eq!(masked, src);
    }

    /// An unclosed fence is code to every renderer, so it is code here.
    #[test]
    fn unterminated_fence_runs_to_the_end() {
        let src = "text\n```\n@key\nmore\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(spans.len(), 1);
        assert_eq!(unmask(&masked, &spans), src);
    }

    #[test]
    fn round_trips_text_without_code() {
        let src = "Just prose with @Smith2020 in it.\n";
        let (masked, spans) = mask(src);
        assert_eq!(masked, src);
        assert!(spans.is_empty());
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// Frontmatter is TOML, not markdown. A `@` in a comment or a value there is never
    /// a citation -- and one of them is what kept `citations-on-a-static-site` opted out
    /// even after every code block in it was covered.
    #[test]
    fn hides_frontmatter() {
        let src = "+++\ntitle = \"T\"\n# teaches @citekey syntax\n+++\n\nCite @Smith2020.\n";
        let (masked, spans) = mask(src);
        assert_eq!(masked.matches('@').count(), 1);
        assert!(masked.contains("Cite @Smith2020."));
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// Only a leading `+++` opens frontmatter, and only the first `+++` after it closes.
    #[test]
    fn frontmatter_must_lead_the_file() {
        assert_eq!(frontmatter_end("+++\na = 1\n+++\nbody\n"), Some(14));
        assert_eq!(frontmatter_end("text\n+++\na = 1\n+++\n"), None);
        assert_eq!(frontmatter_end("+++\nunterminated\n"), None);
        assert_eq!(frontmatter_end(""), None);
    }

    /// The processor rewrites the text between placeholders; the placeholders have to
    /// survive that and land back on the original code.
    #[test]
    fn unmask_after_the_text_around_it_changed() {
        let (masked, spans) = mask("see @Smith2020 and `@Smith2020`\n");
        let processed = masked.replace("@Smith2020", "Smith (2020)");
        assert_eq!(
            unmask(&processed, &spans),
            "see Smith (2020) and `@Smith2020`\n"
        );
    }

    /// Nothing emits a bare NUL, but if one reaches `unmask` it must come out as it
    /// went in rather than eating the text after it.
    #[test]
    fn stray_sentinels_survive() {
        let spans = vec!["`x`".to_string()];
        assert_eq!(unmask("a\u{0}b", &spans), "a\u{0}b");
        assert_eq!(unmask("a\u{0}9\u{0}b", &spans), "a\u{0}9\u{0}b");
        assert_eq!(unmask("a\u{0}0\u{0}b", &spans), "a`x`b");
    }

    #[test]
    fn crlf_blocks_round_trip() {
        let src = "a\r\n```\r\n@key\r\n```\r\nb\r\n";
        let (masked, spans) = mask(src);
        assert!(!masked.contains('@'));
        assert_eq!(unmask(&masked, &spans), src);
    }

    /// Multi-byte characters must not shift a placeholder or split a span.
    #[test]
    fn handles_non_ascii() {
        let src = "på norsk `@Hansen2019` og @Hansen2019\n";
        let (masked, spans) = mask(src);
        assert_eq!(masked.matches('@').count(), 1);
        assert_eq!(unmask(&masked, &spans), src);
    }
}
