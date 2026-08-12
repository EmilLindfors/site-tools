use std::path::PathBuf;
use zotero_cite::{
    CitationStyle, ZoteroDb, default_bbt_db, default_zotero_db, format_references_section,
    process_markdown,
};

pub fn run(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_usage();
        return Ok(());
    }

    match args[0].as_str() {
        "process" => {
            if args.len() < 2 {
                return Err("Usage: site-tools cite process <post-path> [--style apa|numeric|numeric-link] [--output <path>]".to_string());
            }
            let file = PathBuf::from(&args[1]);
            let cite_style = parse_style(&args[2..])?;
            let output = super::parse_flag(&args[2..], "--output").map(PathBuf::from);

            let content =
                std::fs::read_to_string(&file).map_err(|e| format!("Read {}: {}", file.display(), e))?;

            // A post that documents the citation syntax contains @citekeys inside code
            // blocks and code spans. The processor is not code-block aware, so running
            // it over such a post rewrites the examples it is trying to teach -- and
            // deploy.sh commits and pushes the result. Let a post opt out.
            if skip_citations(&content) {
                eprintln!("Skipping {} (extra.skip_citations)", file.display());
                return Ok(());
            }

            let db = open_db()?;
            let (final_content, n_refs) = render(&content, &db, cite_style)?;

            if let Some(out_path) = output {
                std::fs::write(&out_path, &final_content)
                    .map_err(|e| format!("Write {}: {}", out_path.display(), e))?;
                eprintln!(
                    "Processed {n_refs} citations, wrote to {}",
                    out_path.display()
                );
            } else {
                print!("{}", final_content);
            }
            Ok(())
        }
        "all" => process_all(parse_style(&args[1..])?),
        "list" => {
            let db = open_db()?;
            let citekeys = db.list_citekeys().map_err(|e| e.to_string())?;
            println!("Available citekeys ({}):\n", citekeys.len());
            for (citekey, item_key) in citekeys {
                println!("  @{:<40} [{}]", citekey, item_key);
            }
            Ok(())
        }
        "lookup" => {
            if args.len() < 2 {
                return Err("Usage: site-tools cite lookup <citekey>".to_string());
            }
            let key = args[1].trim_start_matches('@');
            let db = open_db()?;
            let ref_data = db.lookup(key).map_err(|e| e.to_string())?;

            println!("Citekey: @{}", ref_data.citekey);
            println!("Type: {}", ref_data.item_type);
            println!("Title: {}", ref_data.title);
            println!(
                "Authors: {}",
                ref_data
                    .authors
                    .iter()
                    .map(|a| format!("{} {}", a.first_name, a.last_name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("Year: {}", ref_data.year);
            if !ref_data.journal.is_empty() {
                println!("Journal: {}", ref_data.journal);
            }
            if !ref_data.doi.is_empty() {
                println!("DOI: {}", ref_data.doi);
            }
            println!("\nFull reference:\n{}", ref_data.full_reference());
            println!("\nTOML:\n{}", ref_data.to_toml());
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("Unknown cite subcommand: {other}")),
    }
}

fn open_db() -> Result<ZoteroDb, String> {
    ZoteroDb::open(&default_zotero_db(), &default_bbt_db()).map_err(|e| {
        format!(
            "Failed to open Zotero databases: {e}\n\
             Hint: set ZOTERO_DATA_DIR to your Zotero data directory"
        )
    })
}

fn parse_style(args: &[String]) -> Result<CitationStyle, String> {
    super::parse_flag(args, "--style")
        .unwrap_or_else(|| "apa".to_string())
        .parse()
        .map_err(|e: String| e)
}

/// Replace citekeys and append a References section. Returns (content, citation count).
fn render(
    content: &str,
    db: &ZoteroDb,
    style: CitationStyle,
) -> Result<(String, usize), String> {
    let (processed, refs) = process_markdown(content, db, style).map_err(|e| e.to_string())?;

    let out = if !refs.is_empty() && !processed.contains("## References") {
        format!("{processed}{}", format_references_section(&refs, style))
    } else {
        processed
    };

    Ok((out, refs.len()))
}

/// Process citations in every post under `content/blog/`, in place.
///
/// This is the loop `build.sh` and `deploy.sh` each used to carry a copy of. Doing it
/// here means one Zotero connection for the whole run instead of one per post, and the
/// `extra.skip_citations` opt-out is applied in the same place it is defined.
pub fn process_all(style: CitationStyle) -> Result<(), String> {
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

    // Read and filter before touching Zotero: a run over posts that cite nothing should
    // not need a Zotero library to be installed at all.
    let mut pending: Vec<(PathBuf, String)> = Vec::new();
    for post in posts {
        let content = std::fs::read_to_string(&post)
            .map_err(|e| format!("Read {}: {e}", post.display()))?;

        if skip_citations(&content) {
            let slug = crate::frontmatter::slug_from_path(&post);
            println!("Skipping {slug} (extra.skip_citations)");
            continue;
        }
        if !has_citekeys(&content) {
            continue;
        }
        pending.push((post, content));
    }

    if pending.is_empty() {
        println!("No posts with citations to process.");
        return Ok(());
    }

    let db = open_db()?;

    for (post, content) in pending {
        let slug = crate::frontmatter::slug_from_path(&post);
        let (rendered, n_refs) = render(&content, &db, style)
            .map_err(|e| format!("{slug}: {e}"))?;

        // Only write on a real change. deploy.sh runs `git add -A`, so rewriting an
        // identical file would still be a no-op there, but leaving mtimes alone keeps
        // incremental tooling honest.
        if rendered == content {
            continue;
        }

        std::fs::write(&post, &rendered)
            .map_err(|e| format!("Write {}: {e}", post.display()))?;
        println!("  {slug}: {n_refs} citations");
    }

    Ok(())
}

/// True if the text plausibly contains a `@citekey`.
///
/// A cheap pre-filter so posts that cite nothing never open the Zotero database. The
/// `@` must start a word, which keeps email addresses (`emil@lindfors.no`) out; a false
/// positive only costs a no-op pass through the processor.
fn has_citekeys(content: &str) -> bool {
    let bytes = content.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        b == b'@'
            && bytes.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic())
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
    })
}

fn print_usage() {
    eprintln!("site-tools cite — Process Zotero citations in blog posts");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  process <post-path> [--style apa|numeric|numeric-link] [--output <path>]");
    eprintln!("                              Replace @citekeys with formatted citations");
    eprintln!("  all [--style ...]           Same, in place, for every post under content/blog/");
    eprintln!("  list                        List all available citekeys from Zotero");
    eprintln!("  lookup <citekey>            Show reference details for a citekey");
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
    fn detects_a_citekey() {
        assert!(has_citekeys("as shown by @Smith2020, this holds"));
        assert!(has_citekeys("[@Smith2020]"));
        assert!(has_citekeys("@Smith2020 at the very start"));
    }

    /// The pre-filter exists to avoid opening Zotero for posts that cite nothing.
    /// An email address is the case that made the old `grep '@[a-zA-Z]'` fire uselessly.
    #[test]
    fn ignores_email_addresses() {
        assert!(!has_citekeys("write to emil@lindfors.no for details"));
    }

    #[test]
    fn ignores_text_without_citekeys() {
        assert!(!has_citekeys(""));
        assert!(!has_citekeys("no citations here at all"));
        assert!(!has_citekeys("a bare @ sign, and @1234 digits"));
    }

    /// A trailing `@` must not index past the end of the buffer.
    #[test]
    fn trailing_at_sign_does_not_panic() {
        assert!(!has_citekeys("ends with @"));
    }

    /// The filter runs on raw bytes; multi-byte characters must not shift the result.
    #[test]
    fn handles_non_ascii_content() {
        assert!(has_citekeys("på norsk, se @Hansen2019"));
        assert!(!has_citekeys("på norsk, ingen kilder"));
    }
}
