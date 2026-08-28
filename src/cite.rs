//! Resolve the citation markers in a post and store what they resolved to.
//!
//! A post is written with `@key` and `[@key]` markers (see `markers`). This resolves
//! each against crossref or a local Zotero library (see `sources`), rewrites the marker
//! into a linked citation, and appends the reference record to the post's own
//! frontmatter as `[[extra.references]]`, which is what `templates/components.html`
//! renders.
//!
//! Storing the record in the post is what makes the pipeline offline: after the first
//! run there are no markers left to resolve, so a build touches neither the network nor
//! a Zotero database, and the reference cannot drift from the post citing it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::bib::Reference;
use crate::markers::Marker;
use crate::sources::{Resolver, Source};

pub fn run(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_usage();
        return Ok(());
    }

    match args[0].as_str() {
        "process" => {
            if args.len() < 2 {
                return Err("Usage: site-tools cite process <post-path> [--source auto|crossref|zotero] [--output <path>]".to_string());
            }
            let file = PathBuf::from(&args[1]);
            let source = parse_source(&args[2..])?;
            let output = super::parse_flag(&args[2..], "--output").map(PathBuf::from);

            let content = std::fs::read_to_string(&file)
                .map_err(|e| format!("Read {}: {}", file.display(), e))?;

            // The mask hides code and frontmatter from the marker scanner, so a post
            // documenting the `@key` syntax no longer needs to opt out. The flag stays
            // as an escape hatch for a post the mask cannot cover.
            if skip_citations(&content) {
                eprintln!("Skipping {} (extra.skip_citations)", file.display());
                return Ok(());
            }

            let mut resolver = Resolver::new(source);
            let (final_content, n_refs) = render(&content, &mut resolver, "post")?;

            if let Some(out_path) = output {
                std::fs::write(&out_path, &final_content)
                    .map_err(|e| format!("Write {}: {}", out_path.display(), e))?;
                eprintln!("Resolved {n_refs} citations, wrote to {}", out_path.display());
            } else {
                print!("{}", final_content);
            }
            Ok(())
        }
        "all" => process_all(parse_source(&args[1..])?),
        "list" => {
            let library = open_zotero()?;
            let citekeys = library.citekeys()?;
            println!("Available citekeys ({}):\n", citekeys.len());
            for (citekey, title) in citekeys {
                let title: String = title.chars().take(60).collect();
                println!("  @{citekey:<44} {title}");
            }
            Ok(())
        }
        "lookup" => {
            if args.len() < 2 {
                return Err("Usage: site-tools cite lookup <citekey|doi>".to_string());
            }
            let key = args[1].trim_start_matches('@');
            let mut resolver = Resolver::new(parse_source(&args[2..])?);
            let reference = resolver.resolve(key, None)?;

            println!("Key:      {}", reference.key);
            println!("Type:     {}", reference.kind);
            println!("Authors:  {}", reference.author);
            println!("Title:    {}", reference.title);
            println!("Year:     {}", reference.year);
            if let Some(journal) = &reference.journal {
                println!("Journal:  {journal}");
            }
            if let Some(doi) = &reference.doi {
                println!("DOI:      {doi}");
            }
            println!("\nFrontmatter:\n{}", reference.to_toml_block());
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("Unknown cite subcommand: {other}")),
    }
}

fn open_zotero() -> Result<crate::zotero::Library, String> {
    crate::zotero::Library::open(&crate::zotero::default_data_dir())
        .map_err(|e| format!("{e}\nHint: set ZOTERO_DATA_DIR to your Zotero data directory"))
}

fn parse_source(args: &[String]) -> Result<Source, String> {
    super::parse_flag(args, "--source")
        .unwrap_or_else(|| "auto".to_string())
        .parse()
}

/// Resolve every marker in one post. Returns (content, count of references stored).
///
/// `label` names the post in warnings, which are the only output on the happy path.
fn render(content: &str, resolver: &mut Resolver, label: &str) -> Result<(String, usize), String> {
    let (masked, spans) = crate::codemask::mask(content);
    let markers = crate::markers::scan(&masked);

    for doi in crate::markers::unbracketed_dois(&masked, &markers) {
        eprintln!(
            "  {label}: @{doi} looks like a DOI. Write it as [@{}] -- a bare one cannot \
             tell where it ends.",
            doi.trim_end_matches(['.', ',', ';', ':'])
        );
    }

    let (toml_str, _) = crate::frontmatter::split(content)?;
    let bib = crate::frontmatter::bib_map(toml_str);
    // File order, kept: an existing reference list is put back the way the post has it.
    let previous = crate::bib::existing(toml_str);
    let order: Vec<String> = previous.iter().map(|r| r.key.clone()).collect();
    let mut stored: BTreeMap<String, Reference> = previous
        .into_iter()
        .map(|r| (r.key.clone(), r))
        .collect();

    let mut resolved: BTreeMap<String, Reference> = BTreeMap::new();
    let mut fresh: Vec<String> = Vec::new();

    for key in markers.iter().flat_map(|m| m.keys.iter()) {
        if resolved.contains_key(key) {
            continue;
        }
        let anchor = anchor_of(key);

        // Already in the post's frontmatter from an earlier run: no lookup, no network.
        if let Some(hit) = stored.get(&anchor) {
            resolved.insert(key.clone(), hit.clone());
            continue;
        }

        match resolver.resolve(key, bib.get(key).map(String::as_str)) {
            Ok(reference) => {
                fresh.push(anchor.clone());
                stored.insert(anchor, reference.clone());
                resolved.insert(key.clone(), reference);
            }
            // An unresolved key is left in the text as written, so it shows up in the
            // rendered post rather than vanishing into a silently missing citation.
            Err(e) => eprintln!("  {label}: {e}"),
        }
    }

    let rewritten = crate::markers::apply(&masked, &markers, |m| inline(m, &resolved));
    let content = crate::codemask::unmask(&rewritten, &spans);

    // Existing references keep their order; newly resolved ones follow in the order they
    // were first cited. Anything else would reshuffle a published post's reference list
    // every time one more citation is added to it.
    let ordered: Vec<Reference> = order
        .iter()
        .chain(fresh.iter())
        .filter_map(|k| stored.get(k).cloned())
        .collect();

    let n = ordered.len();
    Ok((set_references(&content, &ordered)?, n))
}

/// The rendered citation for one marker, or `None` if a key in it did not resolve.
fn inline(marker: &Marker, resolved: &BTreeMap<String, Reference>) -> Option<String> {
    let refs: Vec<&Reference> = marker
        .keys
        .iter()
        .map(|k| resolved.get(k))
        .collect::<Option<Vec<_>>>()?;

    if marker.narrative {
        // A narrative group would read "Smith (2020)Jones (2019)". One key only.
        return refs.first().map(|r| r.narrative());
    }

    Some(format!(
        "({})",
        refs.iter()
            .map(|r| r.parenthetical())
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

fn anchor_of(key: &str) -> String {
    if crate::bib::is_doi(key) {
        crate::bib::anchor_for_doi(key)
    } else {
        key.to_string()
    }
}

/// Rewrite the `[[extra.references]]` tail of the frontmatter.
///
/// The tool owns the frontmatter from the first `[[extra.references]]` line onward and
/// nothing else, so everything the author wrote comes back byte for byte. That is also
/// why the blocks go last: an array of tables captures every bare key after it, so a key
/// written below one would silently become part of a reference.
fn set_references(content: &str, refs: &[Reference]) -> Result<String, String> {
    let (start, end) =
        crate::frontmatter::bounds(content).ok_or("Missing +++ frontmatter delimiters")?;

    let head = &content[start..end];
    let previous = head.find("[[extra.references]]");

    // A post that cites nothing and has no block to replace is not this tool's to
    // rewrite. Without this it would still be written back, because trimming the
    // frontmatter's tail is a change even when nothing else is.
    if refs.is_empty() && previous.is_none() {
        return Ok(content.to_string());
    }

    let kept = match previous {
        Some(i) => &head[..i],
        None => head,
    };

    let mut block = String::new();
    for reference in refs {
        block.push('\n');
        block.push_str(&reference.to_toml_block());
    }

    // Blocks are written with `\n`; a CRLF post keeps CRLF, or `git status` reports
    // every post the tool has touched as modified when nothing in it changed.
    let eol = if content.contains("\r\n") {
        block = block.replace('\n', "\r\n");
        "\r\n"
    } else {
        "\n"
    };

    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..start]);
    out.push_str(kept.trim_end());
    out.push_str(eol);
    out.push_str(&block);
    out.push_str(&content[end..]);
    Ok(out)
}

/// Resolve citations in every post under `content/blog/`, in place.
pub fn process_all(source: Source) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = crate::util::find_project_root(&cwd)?;
    let blog = root.join("content/blog");

    let mut posts: Vec<PathBuf> = std::fs::read_dir(&blog)
        .map_err(|e| format!("Failed to read {}: {e}", blog.display()))?
        .flatten()
        .map(|e| e.path().join("index.md"))
        .filter(|p| p.is_file())
        .collect();
    posts.sort();

    // Read, mask and scan before opening anything: a run over posts whose citations are
    // already resolved needs no network and no Zotero library at all, which after the
    // first run is every run.
    let mut pending: Vec<(PathBuf, String)> = Vec::new();
    for post in posts {
        let content = std::fs::read_to_string(&post)
            .map_err(|e| format!("Read {}: {e}", post.display()))?;

        if skip_citations(&content) {
            let slug = crate::frontmatter::slug_from_path(&post);
            println!("Skipping {slug} (extra.skip_citations)");
            continue;
        }
        let (masked, _) = crate::codemask::mask(&content);
        let markers = crate::markers::scan(&masked);
        if markers.is_empty() && crate::markers::unbracketed_dois(&masked, &markers).is_empty() {
            continue;
        }
        pending.push((post, content));
    }

    if pending.is_empty() {
        println!("No unresolved citations.");
        return Ok(());
    }

    let mut resolver = Resolver::new(source);

    for (post, content) in pending {
        let slug = crate::frontmatter::slug_from_path(&post);
        let (rendered, n_refs) = render(&content, &mut resolver, &slug)?;

        // Only write on a real change. deploy.sh runs `git add -A`, so rewriting an
        // identical file would still be a no-op there, but leaving mtimes alone keeps
        // incremental tooling honest.
        if rendered == content {
            continue;
        }

        std::fs::write(&post, &rendered)
            .map_err(|e| format!("Write {}: {e}", post.display()))?;
        println!("  {slug}: {n_refs} references");
    }

    Ok(())
}

fn print_usage() {
    eprintln!("site-tools cite — Resolve citation markers in blog posts");
    eprintln!();
    eprintln!("Markers:");
    eprintln!("  @Christiansen2017            narrative: the citation carries the sentence");
    eprintln!("  [@Christiansen2017]          parenthetical; join several with `;`");
    eprintln!("  [@10.1016/j.marpol...]       a DOI, which must be bracketed");
    eprintln!();
    eprintln!("A key that is not a DOI is looked up in the post's [extra.bib] map, and");
    eprintln!("failing that in a local Zotero library. Resolved references are written to");
    eprintln!("the post's frontmatter as [[extra.references]], so later builds need neither.");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  process <post-path> [--source ...] [--output <path>]");
    eprintln!("                              Resolve one post");
    eprintln!("  all [--source ...]          Same, in place, for every post under content/blog/");
    eprintln!("  list                        List all available citekeys from Zotero");
    eprintln!("  lookup <citekey|doi>        Show what a marker resolves to");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --source auto|crossref|zotero   auto routes on the marker's shape (default)");
    eprintln!();
    eprintln!("Environment:");
    eprintln!("  CROSSREF_POLITE  an email address, which moves crossref requests into its");
    eprintln!("                   polite pool (3 req/s rather than 1)");
    eprintln!("  ZOTERO_DATA_DIR  the Zotero data directory, for citekeys that are not DOIs");
}

/// True if the post opts out of citation processing via `extra.skip_citations`.
///
/// Frontmatter is optional here: a file without it is simply processed as before.
fn skip_citations(content: &str) -> bool {
    let Ok((toml_str, _)) = crate::frontmatter::split(content) else {
        return false;
    };
    let Ok(table) = toml_str.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("extra")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("skip_citations"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(key: &str, family: &str, year: &str) -> Reference {
        Reference {
            key: key.into(),
            kind: "article".into(),
            author: format!("{family}, J."),
            title: "A title".into(),
            year: year.into(),
            journal: Some("A journal".into()),
            families: vec![family.into()],
            ..Default::default()
        }
    }

    #[test]
    fn opts_out_when_flag_set() {
        let src = "+++\ntitle = \"T\"\n\n[extra]\nskip_citations = true\n+++\n\nbody @key\n";
        assert!(skip_citations(src));
    }

    #[test]
    fn processes_by_default() {
        let src = "+++\ntitle = \"T\"\n+++\n\nbody @key\n";
        assert!(!skip_citations(src));
    }

    #[test]
    fn explicit_false_processes() {
        let src = "+++\ntitle = \"T\"\n\n[extra]\nskip_citations = false\n+++\n\nbody\n";
        assert!(!skip_citations(src));
    }

    /// Never skip because a file is malformed -- that would silently drop real work.
    #[test]
    fn malformed_frontmatter_does_not_opt_out() {
        assert!(!skip_citations("no frontmatter at all"));
        assert!(!skip_citations("+++\nthis is not = valid = toml\n+++\nbody"));
    }

    #[test]
    fn narrative_renders_one_citation() {
        let mut resolved = BTreeMap::new();
        resolved.insert(
            "Smith2020".to_string(),
            reference("Smith2020", "Smith", "2020"),
        );
        let marker = Marker {
            start: 0,
            end: 0,
            keys: vec!["Smith2020".into()],
            narrative: true,
        };
        assert_eq!(
            inline(&marker, &resolved).unwrap(),
            r##"Smith (<a href="#ref-Smith2020">2020</a>)"##
        );
    }

    #[test]
    fn a_bracketed_group_joins_with_semicolons() {
        let mut resolved = BTreeMap::new();
        resolved.insert("a".to_string(), reference("a", "Smith", "2020"));
        resolved.insert("b".to_string(), reference("b", "Jones", "2019"));
        let marker = Marker {
            start: 0,
            end: 0,
            keys: vec!["a".into(), "b".into()],
            narrative: false,
        };
        assert_eq!(
            inline(&marker, &resolved).unwrap(),
            r##"(Smith, <a href="#ref-a">2020</a>; Jones, <a href="#ref-b">2019</a>)"##
        );
    }

    /// One unresolved key in a group leaves the whole marker as written, rather than
    /// rendering half a citation.
    #[test]
    fn a_group_with_an_unknown_key_renders_nothing() {
        let mut resolved = BTreeMap::new();
        resolved.insert("a".to_string(), reference("a", "Smith", "2020"));
        let marker = Marker {
            start: 0,
            end: 0,
            keys: vec!["a".into(), "missing".into()],
            narrative: false,
        };
        assert!(inline(&marker, &resolved).is_none());
    }

    #[test]
    fn references_are_appended_to_the_frontmatter() {
        let src = "+++\ntitle = \"T\"\n\n[extra]\ntoc = true\n+++\n\nbody\n";
        let out = set_references(src, &[reference("Smith2020", "Smith", "2020")]).unwrap();

        assert!(out.contains("toc = true"));
        assert!(out.contains("[[extra.references]]"));
        assert!(out.contains(r#"key = "Smith2020""#));
        assert!(out.ends_with("+++\n\nbody\n"));

        let (toml_str, body) = crate::frontmatter::split(&out).unwrap();
        toml_str
            .parse::<toml::Table>()
            .expect("frontmatter stays valid TOML");
        assert_eq!(body.trim(), "body");
    }

    /// A second run replaces the block rather than stacking another copy on it.
    #[test]
    fn rewriting_replaces_the_previous_block() {
        let src = "+++\ntitle = \"T\"\n+++\n\nbody\n";
        let once = set_references(src, &[reference("Smith2020", "Smith", "2020")]).unwrap();
        let twice = set_references(&once, &[reference("Smith2020", "Smith", "2020")]).unwrap();
        assert_eq!(once, twice);
        assert_eq!(twice.matches("[[extra.references]]").count(), 1);
    }

    /// A post that cites nothing comes back untouched, trailing blank line and all.
    /// Rewriting it would show up as a modified file on every build.
    #[test]
    fn writing_no_references_leaves_the_frontmatter_alone() {
        for src in [
            "+++\ntitle = \"T\"\ndate = 2026-01-01\n+++\n\nbody\n",
            "+++\ntitle = \"T\"\n\n[extra]\ntoc = true\n\n+++\n\nbody\n",
            "+++\r\ntitle = \"T\"\r\n+++\r\n\r\nbody\r\n",
        ] {
            assert_eq!(set_references(src, &[]).unwrap(), src, "changed: {src:?}");
        }
    }

    /// A CRLF post stays CRLF. Git normalises on commit, so the only visible effect of
    /// getting this wrong is every touched post reading as modified.
    #[test]
    fn crlf_posts_keep_their_line_endings() {
        let src = "+++\r\ntitle = \"T\"\r\n+++\r\n\r\nbody\r\n";
        let out = set_references(src, &[reference("Smith2020", "Smith", "2020")]).unwrap();
        assert!(!out.replace("\r\n", "").contains('\n'), "mixed endings: {out:?}");
        assert!(out.contains("[[extra.references]]"));

        let (toml_str, _) = crate::frontmatter::split(&out).unwrap();
        toml_str
            .parse::<toml::Table>()
            .expect("frontmatter stays valid TOML");
    }

    /// The body is not the tool's to touch, `+++` in it included.
    #[test]
    fn the_body_comes_back_byte_for_byte() {
        let src = "+++\ntitle = \"T\"\n+++\n\na += b\n\n+++ not frontmatter\n";
        let out = set_references(src, &[reference("k", "Smith", "2020")]).unwrap();
        assert!(out.ends_with("+++\n\na += b\n\n+++ not frontmatter\n"));
    }
}
