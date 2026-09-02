//! Where a citation's metadata comes from: crossref, or a local Zotero library.
//!
//! Nothing here is configured. The marker decides: a `@key` that is a DOI, or that the
//! post's `[extra.bib]` maps to one, goes to crossref; anything left goes to Zotero.
//! A site that only ever writes DOIs never opens a Zotero database, and someone who
//! prefers their Zotero collection carries on writing citekeys and never reaches the
//! network. `--source` forces one when the default routing is not what is wanted.

use std::collections::BTreeMap;

use crossref_client::{CnFormat, Contributor, Crossref, Work};

use crate::bib::Reference;

/// Which source a run is allowed to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Route on the marker: a DOI to crossref, a bare key to Zotero.
    Auto,
    /// Crossref only. A key with no `[extra.bib]` entry is an error rather than a
    /// silent Zotero lookup.
    Crossref,
    /// Zotero only, which is what this tool did before crossref was an option.
    Zotero,
}

impl std::str::FromStr for Source {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(Source::Auto),
            "crossref" => Ok(Source::Crossref),
            "zotero" => Ok(Source::Zotero),
            other => Err(format!(
                "Unknown citation source: {other} (want auto, crossref or zotero)"
            )),
        }
    }
}

/// Resolves citation markers, opening each backing source only when one is needed.
pub struct Resolver {
    source: Source,
    crossref: Option<Crossref>,
    zotero: Option<crate::zotero::Library>,
    cache: BTreeMap<String, Reference>,
}

impl Resolver {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            crossref: None,
            zotero: None,
            cache: BTreeMap::new(),
        }
    }

    /// Resolve one marker against whichever source its shape selects.
    ///
    /// `key` is what the post wrote; `doi` is what `[extra.bib]` mapped it to, if
    /// anything. The anchor is the key as written, so a post's own citekeys survive
    /// into the HTML ids even when the metadata came from crossref.
    pub fn resolve(&mut self, key: &str, doi: Option<&str>) -> Result<Reference, String> {
        let doi = doi.map(str::to_string).or_else(|| {
            crate::bib::is_doi(key).then(|| key.to_string())
        });

        let anchor = if crate::bib::is_doi(key) {
            crate::bib::anchor_for_doi(key)
        } else {
            key.to_string()
        };

        if let Some(hit) = self.cache.get(&anchor) {
            return Ok(hit.clone());
        }

        let reference = match (self.source, doi) {
            (Source::Zotero, _) => self.from_zotero(key, &anchor)?,
            (_, Some(doi)) => self.from_crossref(&doi, &anchor)?,
            (Source::Crossref, None) => {
                return Err(format!(
                    "@{key} is not a DOI and the post's [extra.bib] does not map it to one"
                ));
            }
            (Source::Auto, None) => self.from_zotero(key, &anchor)?,
        };

        self.cache.insert(anchor, reference.clone());
        Ok(reference)
    }

    fn from_crossref(&mut self, doi: &str, anchor: &str) -> Result<Reference, String> {
        if self.crossref.is_none() {
            self.crossref = Some(build_client()?);
        }
        let client = self.crossref.as_ref().unwrap();

        // One current-thread runtime per lookup would be wasteful, but a client cannot
        // be moved between runtimes safely, so the runtime is built alongside it and
        // the block is the only place anything is awaited.
        let work = runtime()?
            .block_on(client.work(doi))
            .map_err(|e| format!("crossref lookup of {doi} failed: {e}"))?;

        Ok(from_work(&work, anchor))
    }

    fn from_zotero(&mut self, key: &str, anchor: &str) -> Result<Reference, String> {
        if self.zotero.is_none() {
            let dir = crate::zotero::default_data_dir();
            self.zotero = Some(crate::zotero::Library::open(&dir).map_err(|e| {
                format!(
                    "{e}
                     Hint: set ZOTERO_DATA_DIR, or give @{key} a DOI in the post's                      [extra.bib] so it resolves against crossref instead"
                )
            })?);
        }

        Ok(self.zotero.as_ref().unwrap().lookup(key)?.to_reference(anchor))
    }
}

/// A crossref client, polite when an address has been provided for the purpose.
///
/// Crossref limits anonymous callers to roughly one request a second and answers a
/// burst with a `429`; naming an address moves the caller into the polite pool. The
/// address is read from `CROSSREF_POLITE` rather than assumed from the site config,
/// because handing an email to a third party is the author's decision to make.
/// A reference rendered by crossref in a named CSL style (`apa`, `ieee`,
/// `vancouver`, ...). Crossref formats it server-side from the deposited
/// metadata, so this needs a DOI and the network, and answers an unknown style
/// with a 406. Content negotiation cannot cover a Zotero-only reference.
pub fn format_bibliography(doi: &str, style: &str) -> Result<String, String> {
    let client = build_client()?;
    let rendered = runtime()?
        .block_on(client.transform(doi, &CnFormat::bibliography(style)))
        .map_err(|e| format!("crossref could not render {doi} as {style}: {e}"))?;
    Ok(rendered.trim().to_string())
}

fn build_client() -> Result<Crossref, String> {
    let polite = std::env::var("CROSSREF_POLITE").ok();
    let mut builder = Crossref::builder().user_agent("site-tools (+https://lindfors.no)");
    if let Some(email) = polite.as_deref() {
        builder = builder.polite(email);
    }
    builder
        .build()
        .map_err(|e| format!("Failed to build the crossref client: {e}"))
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start a tokio runtime: {e}"))
}

/// Map a crossref `Work` onto the reference record the templates render.
pub fn from_work(work: &Work, anchor: &str) -> Reference {
    let authors = work.author.as_deref().unwrap_or_default();
    let kind = kind_of(work.type_.as_deref());
    let doi = work.doi.clone();

    Reference {
        key: anchor.to_string(),
        author: apa_authors(authors),
        title: plain(work.title.first().map(String::as_str).unwrap_or_default()),
        year: year_of(work),
        journal: first_nonempty(work.container_title.as_deref()).map(|s| plain(&s)),
        volume: work.volume.clone(),
        number: work.issue.clone(),
        pages: work.page.clone(),
        // Crossref names the publisher of a journal article too, and `bib.reference`
        // prints it for a book alone. Storing it everywhere would put a line in every
        // post's frontmatter that nothing ever renders.
        publisher: (kind == "book").then(|| work.publisher.clone()).flatten(),
        booktitle: None,
        school: None,
        isbn: work.isbn.as_deref().and_then(|v| v.first().cloned()),
        // `url` becomes a `[link]` next to the `doi:` link. Crossref sets it to the
        // resolver for everything it registers, so keeping it would print the same
        // destination twice.
        url: work.url.clone().filter(|u| !is_doi_resolver(u, &doi)),
        doi: Some(doi),
        kind,
        families: authors.iter().filter_map(|c| c.family.clone()).collect(),
    }
}

/// Crossref returns titles as fragments of XML: entity-encoded, and marked up with
/// `<i>`, `<sub>` and friends where a publisher used them.
///
/// Both have to come out. The templates escape what they render, so an `&amp;` left in
/// place is displayed as the five characters `&amp;`, and a `<i>` as `<i>`.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;

    // Tags first, so a `&lt;` decoded below is not then read as one.
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        match rest[open..].find('>') {
            Some(close) => rest = &rest[open + close + 1..],
            None => {
                rest = &rest[open..];
                break;
            }
        }
    }
    out.push_str(rest);

    for (entity, ch) in [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        // Last: decoding it first would turn `&amp;lt;` into a `<`.
        ("&amp;", "&"),
    ] {
        if out.contains(entity) {
            out = out.replace(entity, ch);
        }
    }

    out.trim().to_string()
}

/// Whether a URL is just the DOI resolver for this work.
pub fn is_doi_resolver(url: &str, doi: &str) -> bool {
    url.trim_end_matches('/')
        .ends_with(&format!("doi.org/{doi}"))
}

/// Crossref's work types, mapped onto the ones `bib.reference` branches on.
fn kind_of(type_: Option<&str>) -> String {
    match type_.unwrap_or("") {
        "journal-article" | "posted-content" | "report" => "article",
        "book" | "monograph" | "edited-book" | "reference-book" => "book",
        "proceedings-article" => "inproceedings",
        "dissertation" => "phdthesis",
        _ => "misc",
    }
    .to_string()
}

/// `Christiansen, E. A., & Jakobsen, S.` -- the reference-list form.
pub fn apa_authors(authors: &[Contributor]) -> String {
    let parts: Vec<(String, String)> = authors
        .iter()
        .map(|c| match (&c.family, &c.given, &c.name) {
            (Some(family), given, _) => (given.clone().unwrap_or_default(), family.clone()),
            (None, _, Some(name)) => (String::new(), name.clone()),
            _ => (String::new(), String::new()),
        })
        .collect();
    apa_authors_from_parts(&parts)
}

/// The same, from (given, family) pairs -- which is the shape Zotero stores.
///
/// Both sources go through here so a reference reads the same however it was resolved.
pub fn apa_authors_from_parts(authors: &[(String, String)]) -> String {
    let names: Vec<String> = authors
        .iter()
        .map(|(given, family)| {
            let initials = initials(given);
            match (family.is_empty(), initials.is_empty()) {
                (true, _) => String::new(),
                (false, true) => family.clone(),
                (false, false) => format!("{family}, {initials}"),
            }
        })
        .filter(|s| !s.is_empty())
        .collect();

    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{}, & {last}", rest.join(", ")),
    }
}

/// `Emil Andre` -> `E. A.`
fn initials(given: &str) -> String {
    given
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.chars().next())
        .map(|c| format!("{c}."))
        .collect::<Vec<_>>()
        .join(" ")
}

fn year_of(work: &Work) -> String {
    work.issued
        .as_ref()
        .and_then(|d| d.date_parts.0.first())
        .and_then(|parts| parts.first().copied().flatten())
        .map(|y| y.to_string())
        .unwrap_or_default()
}

fn first_nonempty(values: Option<&[String]>) -> Option<String> {
    values?.iter().find(|s| !s.is_empty()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contributor(family: &str, given: &str) -> Contributor {
        Contributor {
            prefix: None,
            suffix: None,
            family: Some(family.into()),
            given: (!given.is_empty()).then(|| given.into()),
            name: None,
            orcid: None,
            authenticated_orcid: None,
            affiliation: Vec::new(),
            sequence: "first".into(),
        }
    }

    #[test]
    fn source_parses_the_three_names() {
        assert_eq!("auto".parse::<Source>().unwrap(), Source::Auto);
        assert_eq!("crossref".parse::<Source>().unwrap(), Source::Crossref);
        assert_eq!("zotero".parse::<Source>().unwrap(), Source::Zotero);
        assert!("bibtex".parse::<Source>().is_err());
    }

    #[test]
    fn apa_authors_join_the_way_apa_does() {
        assert_eq!(apa_authors(&[]), "");
        assert_eq!(apa_authors(&[contributor("Smith", "John")]), "Smith, J.");
        assert_eq!(
            apa_authors(&[contributor("Christiansen", "Elin Agnete"), contributor("Jakobsen", "Stig")]),
            "Christiansen, E. A., & Jakobsen, S."
        );
        assert_eq!(
            apa_authors(&[
                contributor("Osmundsen", "Tonje Cecilie"),
                contributor("Almklov", "Petter"),
                contributor("Tveterås", "Ragnar"),
            ]),
            "Osmundsen, T. C., Almklov, P., & Tveterås, R."
        );
    }

    /// An author with no given name keeps their surname rather than growing a stray
    /// comma, and a corporate author comes through under `name`.
    #[test]
    fn apa_authors_survive_missing_parts() {
        assert_eq!(apa_authors(&[contributor("Havforskningsinstituttet", "")]), "Havforskningsinstituttet");

        let mut corporate = contributor("", "");
        corporate.family = None;
        corporate.name = Some("Norwegian Directorate of Fisheries".into());
        assert_eq!(
            apa_authors(&[corporate]),
            "Norwegian Directorate of Fisheries"
        );
    }

    #[test]
    fn initials_are_one_per_name_part() {
        assert_eq!(initials("John"), "J.");
        assert_eq!(initials("Elin Agnete"), "E. A.");
        assert_eq!(initials("Jean-Pierre"), "J. P.");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn markup_and_entities_come_out_of_a_title() {
        assert_eq!(plain("Aquaculture Economics &amp; Management"), "Aquaculture Economics & Management");
        assert_eq!(plain("Growth of <i>Salmo salar</i> in cages"), "Growth of Salmo salar in cages");
        assert_eq!(plain("CO<sub>2</sub> uptake"), "CO2 uptake");
        assert_eq!(plain("plain title"), "plain title");
    }

    /// `&amp;lt;` is an escaped `&lt;`, and must decode to the text `&lt;` rather than
    /// to a `<` that then reads as a tag.
    #[test]
    fn entity_decoding_does_not_cascade() {
        assert_eq!(plain("a &amp;lt; b"), "a &lt; b");
    }

    /// An unclosed `<` is content, not the start of a tag, and must not eat the rest.
    #[test]
    fn an_unclosed_tag_keeps_its_text() {
        assert_eq!(plain("a < b"), "a < b");
    }

    #[test]
    fn the_doi_resolver_is_not_a_separate_link() {
        let doi = "10.1016/j.marpol.2016.10.020";
        assert!(is_doi_resolver("https://doi.org/10.1016/j.marpol.2016.10.020", doi));
        assert!(is_doi_resolver("http://dx.doi.org/10.1016/j.marpol.2016.10.020", doi));
        assert!(!is_doi_resolver("https://example.com/paper.pdf", doi));
    }

    #[test]
    fn crossref_types_map_onto_the_component_branches() {
        assert_eq!(kind_of(Some("journal-article")), "article");
        assert_eq!(kind_of(Some("proceedings-article")), "inproceedings");
        assert_eq!(kind_of(Some("dissertation")), "phdthesis");
        assert_eq!(kind_of(Some("book")), "book");
        assert_eq!(kind_of(Some("dataset")), "misc");
        assert_eq!(kind_of(None), "misc");
    }
}
