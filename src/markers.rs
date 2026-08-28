//! Finding the citation markers in a post, and putting rendered citations back.
//!
//! Two forms, following pandoc's convention closely enough that nobody has to learn a
//! third one:
//!
//! * `@Christiansen2017` -- narrative, the citation carries the sentence.
//! * `[@Christiansen2017]` -- parenthetical, several joined with `;`.
//!
//! A marker is either a short key the post's `[extra.bib]` maps to a DOI, or a DOI
//! written out. A DOI has to be bracketed: it contains `.` and `/`, so a bare
//! `@10.1016/j.marpol.2016.10.020` at the end of a sentence has no way to know whether
//! the full stop is part of it. Brackets settle that, and a DOI worth writing narratively
//! is a DOI worth giving a name in `[extra.bib]`.
//!
//! Everything here runs on *masked* text (see `codemask`), so a marker inside a code
//! block or the frontmatter is not a marker.

/// One citation marker and where it sits in the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub start: usize,
    pub end: usize,
    pub keys: Vec<String>,
    /// `@key` rather than `[@key]`: the names belong in the sentence.
    pub narrative: bool,
}

/// Every citation marker in the body, in order.
pub fn scan(body: &str) -> Vec<Marker> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            // A `[@` that does not parse as a group is left whole, `@` included: half
            // a rendered group -- "[Smith (2017); b2020]" -- reads as a content mistake,
            // where the marker as written reads as the typo it is.
            b'[' if bytes.get(i + 1) == Some(&b'@') => match bracketed(body, i) {
                Some(marker) => {
                    i = marker.end;
                    out.push(marker);
                }
                None => i += 2,
            },
            b'@' if starts_word(body, i) => match narrative(body, i) {
                Some(marker) => {
                    i = marker.end;
                    out.push(marker);
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }

    out
}

/// Replace each marker with the text `render` gives for it.
///
/// A marker `render` declines is left exactly as written, which is what makes an
/// unresolved key a visible `@key` in the output rather than a hole.
pub fn apply(body: &str, markers: &[Marker], render: impl Fn(&Marker) -> Option<String>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0;

    for marker in markers {
        out.push_str(&body[cursor..marker.start]);
        match render(marker) {
            Some(text) => out.push_str(&text),
            None => out.push_str(&body[marker.start..marker.end]),
        }
        cursor = marker.end;
    }

    out.push_str(&body[cursor..]);
    out
}

/// A bare `@` that a DOI was written after, which this cannot read.
///
/// Worth saying out loud: the alternative is a marker that silently is not one, and the
/// post ships with a raw DOI in the middle of a sentence. `markers` is what `scan`
/// already found, so a DOI inside a group it read is not reported as loose.
pub fn unbracketed_dois(body: &str, markers: &[Marker]) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while let Some(at) = body[i..].find('@').map(|p| p + i) {
        i = at + 1;
        if markers.iter().any(|m| m.start <= at && at < m.end) {
            continue;
        }
        if !starts_word(body, at) || bytes.get(at + 1) != Some(&b'1') {
            continue;
        }
        let rest = &body[at + 1..];
        let doi: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ']' && *c != ')')
            .collect();
        if crate::bib::is_doi(doi.trim_end_matches(['.', ',', ';', ':'])) {
            out.push(doi);
        }
    }

    out
}

/// `[@a]`, or `[@a; @b]`, starting at the `[`.
fn bracketed(body: &str, start: usize) -> Option<Marker> {
    let rest = &body[start + 1..];
    let close = rest.find(']')?;
    let inner = &rest[..close];

    let mut keys = Vec::new();
    for part in inner.split(';') {
        let part = part.trim();
        let key = part.strip_prefix('@')?;
        if !valid_key(key) {
            return None;
        }
        keys.push(key.to_string());
    }

    (!keys.is_empty()).then(|| Marker {
        start,
        end: start + 1 + close + 1,
        keys,
        narrative: false,
    })
}

/// `@key`, starting at the `@`. Short keys only -- see the module note on DOIs.
fn narrative(body: &str, start: usize) -> Option<Marker> {
    let rest = &body[start + 1..];
    if !rest.starts_with(|c: char| c.is_alphabetic()) {
        return None;
    }

    let len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let key = &rest[..len];

    valid_key(key).then(|| Marker {
        start,
        end: start + 1 + len,
        keys: vec![key.to_string()],
        narrative: true,
    })
}

/// A key is a DOI, or a bibtex-shaped name. Anything else is not a citation.
fn valid_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if crate::bib::is_doi(key) {
        return true;
    }
    key.starts_with(|c: char| c.is_alphabetic())
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// True if the `@` at `i` opens a word rather than sitting inside one.
///
/// This is what keeps `emil@lindfors.no` from reading as a citation. The character
/// before is read as a character, not a byte, so a `@` after a Norwegian word is judged
/// on the letter rather than on a UTF-8 continuation byte.
fn starts_word(body: &str, i: usize) -> bool {
    body[..i]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(body: &str) -> Vec<Vec<String>> {
        scan(body).into_iter().map(|m| m.keys).collect()
    }

    #[test]
    fn finds_a_narrative_marker() {
        let markers = scan("Research by @Christiansen2017 shows");
        assert_eq!(markers.len(), 1);
        assert!(markers[0].narrative);
        assert_eq!(markers[0].keys, vec!["Christiansen2017"]);
        assert_eq!(&"Research by @Christiansen2017 shows"[markers[0].start..markers[0].end], "@Christiansen2017");
    }

    #[test]
    fn finds_a_parenthetical_marker() {
        let markers = scan("a wicked problem [@osmundsen2017].");
        assert_eq!(markers.len(), 1);
        assert!(!markers[0].narrative);
        assert_eq!(markers[0].keys, vec!["osmundsen2017"]);
        assert_eq!(&"a wicked problem [@osmundsen2017]."[markers[0].start..markers[0].end], "[@osmundsen2017]");
    }

    #[test]
    fn a_bracketed_group_is_one_marker() {
        assert_eq!(
            keys_of("both [@a2017; @b2020] agree"),
            vec![vec!["a2017".to_string(), "b2020".to_string()]]
        );
    }

    /// The reason DOIs are bracketed at all.
    #[test]
    fn a_doi_is_a_key_inside_brackets() {
        assert_eq!(
            keys_of("shown in [@10.1016/j.marpol.2016.10.020]."),
            vec![vec!["10.1016/j.marpol.2016.10.020".to_string()]]
        );
    }

    #[test]
    fn a_bare_doi_is_not_a_marker() {
        assert!(scan("shown in @10.1016/j.marpol.2016.10.020.").is_empty());
    }

    /// ...but it is worth saying so, rather than shipping a raw DOI mid-sentence.
    #[test]
    fn a_bare_doi_is_reported() {
        let loose = "shown in @10.1016/j.marpol.2016.10.020.";
        assert_eq!(
            unbracketed_dois(loose, &scan(loose)),
            vec!["10.1016/j.marpol.2016.10.020.".to_string()]
        );

        let no_dois = "no dois here @Smith2020";
        assert!(unbracketed_dois(no_dois, &scan(no_dois)).is_empty());
    }

    /// A DOI the scanner did read, alone or in a group, is not loose.
    #[test]
    fn a_bracketed_doi_is_not_reported() {
        for body in [
            "shown in [@10.1016/j.marpol.2016.10.020]",
            "both [@Smith2020; @10.1080/13657305.2017.1262476] agree",
        ] {
            assert!(
                unbracketed_dois(body, &scan(body)).is_empty(),
                "reported a bracketed DOI in: {body}"
            );
        }
    }

    #[test]
    fn email_addresses_are_not_citations() {
        assert!(scan("write to emil@lindfors.no").is_empty());
        assert!(scan("`postmaster@lindfors.no` costs more").is_empty());
    }

    #[test]
    fn punctuation_ends_a_narrative_key() {
        assert_eq!(keys_of("by @Smith2020, who"), vec![vec!["Smith2020".to_string()]]);
        assert_eq!(keys_of("by @Smith2020."), vec![vec!["Smith2020".to_string()]]);
        assert_eq!(keys_of("(@Smith2020)"), vec![vec!["Smith2020".to_string()]]);
        assert_eq!(keys_of("@Smith2020"), vec![vec!["Smith2020".to_string()]]);
    }

    #[test]
    fn underscores_and_hyphens_are_part_of_a_key() {
        assert_eq!(
            keys_of("@Hopp_Coffay_Lindfors_2023 and @van-der-berg2019"),
            vec![
                vec!["Hopp_Coffay_Lindfors_2023".to_string()],
                vec!["van-der-berg2019".to_string()]
            ]
        );
    }

    /// A markdown link's `[` must not swallow the text after it.
    #[test]
    fn brackets_without_an_at_are_left_alone() {
        assert!(scan("see [the docs](https://example.com) for @more").len() == 1);
    }

    #[test]
    fn an_unclosed_bracket_is_not_a_marker() {
        assert!(scan("[@Smith2020 with no close").is_empty());
    }

    /// A group where one entry is malformed is not silently half-read.
    #[test]
    fn a_broken_group_is_not_a_marker() {
        assert!(scan("[@a2017; b2020]").is_empty());
        assert!(scan("[@]").is_empty());
    }

    #[test]
    fn apply_replaces_only_the_markers() {
        let body = "By @Smith2020 and [@Jones2019], see [docs](x).";
        let markers = scan(body);
        let out = apply(body, &markers, |m| Some(format!("<{}>", m.keys.join("+"))));
        assert_eq!(out, "By <Smith2020> and <Jones2019>, see [docs](x).");
    }

    #[test]
    fn apply_leaves_an_unresolved_marker_as_written() {
        let body = "By @Smith2020 and @Unknown1999.";
        let markers = scan(body);
        let out = apply(body, &markers, |m| {
            (m.keys[0] == "Smith2020").then(|| "Smith (2020)".to_string())
        });
        assert_eq!(out, "By Smith (2020) and @Unknown1999.");
    }

    #[test]
    fn apply_is_a_no_op_without_markers() {
        let body = "nothing to cite here.";
        assert_eq!(apply(body, &[], |_| Some("x".into())), body);
    }

    /// Byte offsets have to survive multi-byte characters either side of a marker.
    #[test]
    fn offsets_hold_across_non_ascii() {
        let body = "på norsk, se @Hansen2019 og [@Tveterås2020].";
        let markers = scan(body);
        assert_eq!(markers.len(), 2);
        assert_eq!(&body[markers[0].start..markers[0].end], "@Hansen2019");
        assert_eq!(&body[markers[1].start..markers[1].end], "[@Tveterås2020]");
    }
}
