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
            let style_str = super::parse_flag(&args[2..], "--style")
                .unwrap_or_else(|| "apa".to_string());
            let output = super::parse_flag(&args[2..], "--output").map(PathBuf::from);

            let cite_style: CitationStyle = style_str
                .parse()
                .map_err(|e: String| e)?;

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

            let (processed, refs) =
                process_markdown(&content, &db, cite_style).map_err(|e| e.to_string())?;

            let final_content = if !refs.is_empty() && !processed.contains("## References") {
                format!(
                    "{}{}",
                    processed,
                    format_references_section(&refs, cite_style)
                )
            } else {
                processed
            };

            if let Some(out_path) = output {
                std::fs::write(&out_path, &final_content)
                    .map_err(|e| format!("Write {}: {}", out_path.display(), e))?;
                eprintln!(
                    "Processed {} citations, wrote to {}",
                    refs.len(),
                    out_path.display()
                );
            } else {
                print!("{}", final_content);
            }
            Ok(())
        }
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

fn print_usage() {
    eprintln!("site-tools cite — Process Zotero citations in blog posts");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  process <post-path> [--style apa|numeric|numeric-link] [--output <path>]");
    eprintln!("                              Replace @citekeys with formatted citations");
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
}
