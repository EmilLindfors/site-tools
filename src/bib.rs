//! The reference record, and the two shapes a citation takes on the page.
//!
//! A resolved citation is stored in the post's own frontmatter as
//! `[[extra.references]]`, which is the shape `templates/components.html` already
//! renders through `bib.reference`. That makes the committed post the cache: a build
//! needs no network and no local library, and a reference cannot drift from the post
//! that cites it.

/// One entry in a post's reference list.
///
/// The field names are the ones `bib.reference` reads, so adding a field here means
/// adding it to the component too. `key` is the HTML anchor rather than the citekey:
/// a DOI cited inline has no key of its own, so it gets a slugified one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reference {
    pub key: String,
    pub kind: String,
    pub author: String,
    pub title: String,
    pub year: String,
    pub journal: Option<String>,
    pub volume: Option<String>,
    pub number: Option<String>,
    pub pages: Option<String>,
    pub publisher: Option<String>,
    pub booktitle: Option<String>,
    pub school: Option<String>,
    pub isbn: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    /// Surnames only, in citation order -- what the inline forms are built from.
    pub families: Vec<String>,
}

impl Reference {
    /// The inline form for a citation that carries the sentence:
    /// `Christiansen & Jakobsen (<a href="#ref-Christiansen2017">2017</a>)`.
    pub fn narrative(&self) -> String {
        format!("{} ({})", self.author_label(), self.link())
    }

    /// The inline form for a citation in brackets, without the surrounding parens so
    /// several can be joined into one.
    pub fn parenthetical(&self) -> String {
        format!("{}, {}", self.author_label(), self.link())
    }

    /// The year, linked to the reference entry.
    ///
    /// Raw HTML rather than `[2017](#ref-key)` on purpose. The reference list is
    /// rendered by `templates/page.html` from `[[extra.references]]`, so its `id`
    /// attributes are not part of `page.content` -- and zola resolves a markdown
    /// `#anchor` against the page's own content and fails the build when it cannot find
    /// it. Raw HTML passes through unchecked. Nothing is lost by that: a marker is only
    /// ever rewritten once its reference has been stored, so the target always exists.
    fn link(&self) -> String {
        format!(r##"<a href="#ref-{}">{}</a>"##, self.key, self.year)
    }

    /// `Smith`, `Smith & Jones`, `Smith et al.` -- APA's rule for in-text names.
    fn author_label(&self) -> String {
        match self.families.as_slice() {
            [] => self.author.clone(),
            [one] => one.clone(),
            [one, two] => format!("{one} & {two}"),
            [one, ..] => format!("{one} et al."),
        }
    }

    /// The record as a `[[extra.references]]` block.
    ///
    /// Written by hand rather than by re-serializing the whole frontmatter, because
    /// round-tripping the file through `toml` would reorder and reformat everything the
    /// author wrote.
    pub fn to_toml_block(&self) -> String {
        let mut out = String::from("[[extra.references]]\n");
        let mut push = |k: &str, v: &str| {
            out.push_str(&format!("{k} = {}\n", toml_string(v)));
        };

        push("key", &self.key);
        push("type", &self.kind);
        push("author", &self.author);
        push("title", &self.title);
        push("year", &self.year);
        for (k, v) in self.optional_fields() {
            push(k, &v);
        }
        out
    }

    /// The reference as one markdown list item.
    ///
    /// The web page renders `[[extra.references]]` through the `bib.reference`
    /// component; the PDF and the plain-markdown representation do not go through Tera
    /// at all, so they render from here. The two are kept in the same order and shape
    /// on purpose -- a reader comparing the PDF against the page should not find the
    /// references formatted differently.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("- ");
        if !self.author.is_empty() {
            out.push_str(&self.author);
            // "Christiansen, E. A., & Jakobsen, S." already ends in a period, and APA
            // does not write a second one after an initial.
            if !self.author.ends_with('.') {
                out.push('.');
            }
            out.push(' ');
        }

        match self.kind.as_str() {
            "book" => {
                out.push_str(&format!("*{}*. ", self.title));
                if let Some(publisher) = &self.publisher {
                    out.push_str(&format!("{publisher}, "));
                }
            }
            "phdthesis" => {
                out.push_str(&format!("*{}*. PhD thesis, ", self.title));
                if let Some(school) = &self.school {
                    out.push_str(&format!("{school}, "));
                }
            }
            _ => {
                out.push_str(&format!("\"{}\". ", self.title));
                let container = self.journal.as_ref().or(self.booktitle.as_ref());
                if let Some(container) = container {
                    out.push_str(&format!("*{container}*"));
                    out.push_str(", ");
                }
                if let Some(volume) = &self.volume {
                    out.push_str(&format!("vol. {volume}, "));
                }
                if let Some(number) = &self.number {
                    out.push_str(&format!("no. {number}, "));
                }
                if let Some(pages) = &self.pages {
                    out.push_str(&format!("pp. {pages}, "));
                }
            }
        }

        out.push_str(&self.year);
        out.push('.');

        if let Some(doi) = &self.doi {
            out.push_str(&format!(" [doi:{doi}](https://doi.org/{doi})"));
        }
        if let Some(url) = &self.url {
            out.push_str(&format!(" [[link]]({url})"));
        }
        out
    }

    fn optional_fields(&self) -> Vec<(&'static str, String)> {
        [
            ("journal", &self.journal),
            ("volume", &self.volume),
            ("number", &self.number),
            ("pages", &self.pages),
            ("publisher", &self.publisher),
            ("booktitle", &self.booktitle),
            ("school", &self.school),
            ("isbn", &self.isbn),
            ("doi", &self.doi),
            ("url", &self.url),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.clone().map(|v| (k, v)))
        .collect()
    }
}

/// Read back the `[[extra.references]]` a post already carries, in the order it wrote
/// them.
///
/// The order is the point of returning a `Vec`: rewriting the block has to put a
/// published post's reference list back the way it was, and a map keyed by anchor would
/// silently re-sort it on every run.
pub fn existing(toml_str: &str) -> Vec<Reference> {
    let Ok(table) = toml_str.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(refs) = table
        .get("extra")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("references"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };

    let str_of = |t: &toml::Table, k: &str| t.get(k).and_then(|v| v.as_str()).map(String::from);

    refs.iter()
        .filter_map(|v| v.as_table())
        .filter_map(|t| {
            let key = str_of(t, "key")?;
            Some(
                Reference {
                    key,
                    kind: str_of(t, "type").unwrap_or_else(|| "misc".into()),
                    author: str_of(t, "author").unwrap_or_default(),
                    title: str_of(t, "title").unwrap_or_default(),
                    year: str_of(t, "year").unwrap_or_default(),
                    journal: str_of(t, "journal"),
                    volume: str_of(t, "volume"),
                    number: str_of(t, "number"),
                    pages: str_of(t, "pages"),
                    publisher: str_of(t, "publisher"),
                    booktitle: str_of(t, "booktitle"),
                    school: str_of(t, "school"),
                    isbn: str_of(t, "isbn"),
                    doi: str_of(t, "doi"),
                    url: str_of(t, "url"),
                    families: families_from(&str_of(t, "author").unwrap_or_default()),
                },
            )
        })
        .collect()
}

/// Replace an inline citation's `<a href="#ref-...">2017</a>` with just its text.
///
/// The PDF is one document with no page anchors to jump to, and typst renders raw HTML
/// as literal text. `cite` writes the anchor as HTML rather than as a markdown link
/// because zola link-checks the markdown form against a reference list that is no longer
/// in the page content -- see `bib::Reference::link`.
pub fn strip_citation_anchors(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(open) = rest.find("<a href=\"#ref-") {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        rest = after;

        // A tag that never closes is not a citation link. Leave it as written rather
        // than guessing where it ended.
        let (Some(gt), Some(close)) = (after.find('>'), after.find("</a>")) else {
            break;
        };
        if close < gt {
            break;
        }

        out.push_str(&after[gt + 1..close]);
        rest = &after[close + 4..];
    }

    out.push_str(rest);
    out
}

/// The `## References` section for a post, or `None` if it cites nothing.
///
/// For the outputs that render markdown rather than Tera. The heading matches the one
/// `templates/page.html` prints above the rendered component list.
pub fn references_markdown(toml_str: &str) -> Option<String> {
    let refs = existing(toml_str);
    if refs.is_empty() {
        return None;
    }

    let mut out = String::from("## References\n\n");
    for reference in &refs {
        out.push_str(&reference.to_markdown());
        out.push('\n');
    }
    Some(out)
}

/// Recover the surnames from a stored `author` string.
///
/// The stored form is APA's -- `Christiansen, E. A., & Jakobsen, S.` -- so the surname
/// is what precedes each comma-initial pair. Only used to re-render an inline citation
/// for a reference that was resolved on an earlier run.
fn families_from(author: &str) -> Vec<String> {
    author
        .split(", &")
        .flat_map(|chunk| chunk.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // Initials are one or two letters followed by a period, repeated.
        .filter(|s| !s.split_whitespace().all(|w| w.ends_with('.') && w.len() <= 3))
        .map(|s| s.trim_start_matches('&').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A TOML basic string, escaped.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An HTML anchor for a citation that has no key of its own.
///
/// A DOI is `10.1016/j.marpol.2016.10.020`; a fragment identifier holding `/` and `.`
/// is legal but awkward to link to by hand, so it is flattened.
pub fn anchor_for_doi(doi: &str) -> String {
    let mut out = String::with_capacity(doi.len());
    let mut last_dash = false;
    for c in doi.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// True if a citation marker is a DOI rather than a short key.
///
/// Every registered DOI starts `10.` and carries a `/`. That is the whole test, and it
/// cannot collide with a bibtex key, which may not contain either.
pub fn is_doi(key: &str) -> bool {
    key.starts_with("10.") && key[3..].contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Reference {
        Reference {
            key: "Christiansen2017".into(),
            kind: "article".into(),
            author: "Christiansen, E. A., & Jakobsen, S.".into(),
            title: "Diversity in narratives".into(),
            year: "2017".into(),
            journal: Some("Marine Policy".into()),
            doi: Some("10.1016/j.marpol.2016.10.020".into()),
            families: vec!["Christiansen".into(), "Jakobsen".into()],
            ..Default::default()
        }
    }

    #[test]
    fn narrative_names_the_authors_and_links_the_year() {
        assert_eq!(
            sample().narrative(),
            r##"Christiansen & Jakobsen (<a href="#ref-Christiansen2017">2017</a>)"##
        );
    }

    #[test]
    fn parenthetical_leaves_the_brackets_to_the_caller() {
        assert_eq!(
            sample().parenthetical(),
            r##"Christiansen & Jakobsen, <a href="#ref-Christiansen2017">2017</a>"##
        );
    }

    /// APA: one name, two joined by an ampersand, three or more abbreviated.
    #[test]
    fn author_label_follows_apa() {
        let mut r = sample();
        r.families = vec!["Smith".into()];
        assert_eq!(r.author_label(), "Smith");
        r.families = vec!["Smith".into(), "Jones".into()];
        assert_eq!(r.author_label(), "Smith & Jones");
        r.families = vec!["Smith".into(), "Jones".into(), "Brown".into()];
        assert_eq!(r.author_label(), "Smith et al.");
    }

    /// A work crossref gives no parseable authors for still has to cite as something.
    #[test]
    fn author_label_falls_back_to_the_full_string() {
        let mut r = sample();
        r.families.clear();
        r.author = "Norwegian Directorate of Fisheries".into();
        assert_eq!(r.author_label(), "Norwegian Directorate of Fisheries");
    }

    #[test]
    fn toml_block_omits_absent_fields() {
        let block = sample().to_toml_block();
        assert!(block.starts_with("[[extra.references]]\n"));
        assert!(block.contains(r#"journal = "Marine Policy""#));
        assert!(!block.contains("volume"));
        assert!(!block.contains("isbn"));
    }

    /// A title with a quote in it must not break the frontmatter it is written into.
    #[test]
    fn toml_block_escapes_strings() {
        let mut r = sample();
        r.title = r#"A "wicked" problem\case"#.into();
        let block = r.to_toml_block();
        assert!(block.contains(r#"title = "A \"wicked\" problem\\case""#));
        let parsed: toml::Table = block.parse().expect("block must be valid TOML");
        assert!(parsed.contains_key("extra"));
    }

    #[test]
    fn a_written_block_reads_back() {
        let block = sample().to_toml_block();
        let back = existing(&block);
        let got = back.first().expect("the entry survives");
        assert_eq!(got.key, "Christiansen2017");
        assert_eq!(got.title, "Diversity in narratives");
        assert_eq!(got.journal.as_deref(), Some("Marine Policy"));
        assert_eq!(got.families, vec!["Christiansen", "Jakobsen"]);
    }

    /// File order, not alphabetical: re-running the tool must not re-sort a published
    /// post's reference list.
    #[test]
    fn existing_keeps_the_order_it_was_written_in() {
        let mut zed = sample();
        zed.key = "Zaharia2019".into();
        let block = format!("{}
{}", zed.to_toml_block(), sample().to_toml_block());
        let keys: Vec<String> = existing(&block).into_iter().map(|r| r.key).collect();
        assert_eq!(keys, vec!["Zaharia2019", "Christiansen2017"]);
    }

    #[test]
    fn existing_is_empty_without_references() {
        assert!(existing("title = \"T\"").is_empty());
        assert!(existing("not = = toml").is_empty());
    }

    #[test]
    fn families_are_recovered_from_the_apa_author_string() {
        assert_eq!(families_from("Smith, J."), vec!["Smith"]);
        assert_eq!(
            families_from("Christiansen, E. A., & Jakobsen, S."),
            vec!["Christiansen", "Jakobsen"]
        );
        assert_eq!(
            families_from("Osmundsen, T. C., Almklov, P., & Tveterås, R."),
            vec!["Osmundsen", "Almklov", "Tveterås"]
        );
    }

    #[test]
    fn markdown_renders_an_article() {
        let mut r = sample();
        r.volume = Some("75".into());
        r.pages = Some("156-164".into());
        let expected = concat!(
            "- Christiansen, E. A., & Jakobsen, S. ",
            "\"Diversity in narratives\". *Marine Policy*, vol. 75, pp. 156-164, 2017. ",
            "[doi:10.1016/j.marpol.2016.10.020](https://doi.org/10.1016/j.marpol.2016.10.020)"
        );
        assert_eq!(r.to_markdown(), expected);
    }

    /// An author string not ending in an initial still gets its period.
    #[test]
    fn a_corporate_author_gets_a_period() {
        let mut r = sample();
        r.author = "Norwegian Directorate of Fisheries".into();
        r.doi = None;
        assert!(r.to_markdown().starts_with("- Norwegian Directorate of Fisheries. \""));
    }

    #[test]
    fn markdown_renders_a_book_and_a_thesis() {
        let mut r = sample();
        r.kind = "book".into();
        r.journal = None;
        r.doi = None;
        r.publisher = Some("Universitetsforlaget".into());
        assert_eq!(
            r.to_markdown(),
            "- Christiansen, E. A., & Jakobsen, S. *Diversity in narratives*. Universitetsforlaget, 2017."
        );

        r.kind = "phdthesis".into();
        r.publisher = None;
        r.school = Some("NTNU".into());
        assert_eq!(
            r.to_markdown(),
            "- Christiansen, E. A., & Jakobsen, S. *Diversity in narratives*. PhD thesis, NTNU, 2017."
        );
    }

    #[test]
    fn a_post_citing_nothing_gets_no_section() {
        assert!(references_markdown("title = \"T\"").is_none());
        let block = sample().to_toml_block();
        let section = references_markdown(&block).expect("one reference");
        assert!(section.starts_with("## References\n\n- Christiansen"));
    }

    #[test]
    fn citation_anchors_lose_their_link_and_keep_the_year() {
        assert_eq!(
            strip_citation_anchors(r##"Smith (<a href="#ref-Smith2020">2020</a>) found it."##),
            "Smith (2020) found it."
        );
        assert_eq!(
            strip_citation_anchors(
                r##"(A, <a href="#ref-a">2020</a>; B, <a href="#ref-b">2019</a>)"##
            ),
            "(A, 2020; B, 2019)"
        );
    }

    /// Any other anchor in a post is the author's link and stays as written.
    #[test]
    fn other_anchors_are_untouched() {
        let line = r##"see <a href="#section-two">below</a> and [a link](https://x)"##;
        assert_eq!(strip_citation_anchors(line), line);
    }

    /// A truncated tag must not swallow the rest of the line.
    #[test]
    fn an_unterminated_anchor_is_left_alone() {
        let line = r##"Smith (<a href="#ref-Smith2020">2020 and more"##;
        assert_eq!(strip_citation_anchors(line), line);
    }

    #[test]
    fn dois_are_told_apart_from_keys() {
        assert!(is_doi("10.1016/j.marpol.2016.10.020"));
        assert!(is_doi("10.1080/13657305.2017.1262476"));
        assert!(!is_doi("Christiansen2017"));
        assert!(!is_doi("10.1016"));
        assert!(!is_doi("iversen2020"));
        assert!(!is_doi(""));
    }

    #[test]
    fn doi_anchors_are_flat() {
        assert_eq!(
            anchor_for_doi("10.1016/j.marpol.2016.10.020"),
            "10-1016-j-marpol-2016-10-020"
        );
        assert_eq!(anchor_for_doi("10.1/A_b"), "10-1-a-b");
    }
}
