mod cite;
mod cv;
mod frontmatter;
mod newsletter;
mod pdf;
mod util;

use std::{env, process};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let result = match args[1].as_str() {
        "cite" => cite::run(&args[2..]),
        "cv" => run_cv(&args[2..]),
        "newsletter" => run_newsletter(&args[2..]),
        "pdf" => run_pdf(&args[2..]),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("Unknown command: {other}")),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn run_newsletter(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        eprintln!("Usage: site-tools newsletter <gen|send> ...");
        process::exit(1);
    }

    match args[0].as_str() {
        "gen" => {
            if args.len() < 2 {
                return Err("Usage: site-tools newsletter gen <post-path>".to_string());
            }
            newsletter::gen(&args[1])
        }
        "send" => {
            if args.len() < 2 {
                return Err("Usage: site-tools newsletter send <slug> [--subject <text>]".to_string());
            }
            let slug = &args[1];
            let subject = parse_flag(&args[2..], "--subject");
            newsletter::send(slug, subject.as_deref())
        }
        "-h" | "--help" | "help" => {
            print_newsletter_usage();
            Ok(())
        }
        other => Err(format!("Unknown newsletter subcommand: {other}")),
    }
}

fn run_cv(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_cv_usage();
        std::process::exit(1);
    }

    match args[0].as_str() {
        "build" => cv::build(),
        "-h" | "--help" | "help" => {
            print_cv_usage();
            Ok(())
        }
        other => Err(format!("Unknown cv subcommand: {other}")),
    }
}

fn run_pdf(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        eprintln!("Usage: site-tools pdf <gen> ...");
        process::exit(1);
    }

    match args[0].as_str() {
        "gen" => {
            if args.len() < 2 {
                return Err("Usage: site-tools pdf gen <post-path>".to_string());
            }
            pdf::gen(&args[1])
        }
        "all" => pdf::gen_all(),
        "-h" | "--help" | "help" => {
            print_pdf_usage();
            Ok(())
        }
        other => Err(format!("Unknown pdf subcommand: {other}")),
    }
}

/// Parse a --flag value from args.
fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
}

fn print_usage() {
    eprintln!("site-tools — CLI for lindfors.no blog tasks");
    eprintln!();
    eprintln!("Usage: site-tools <command> <subcommand> [options]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  cite process <post-path> [--style ...]  Replace @citekeys with formatted citations");
    eprintln!("  cite all [--style ...]                  Same, in place, for every post");
    eprintln!("  cite list                               List available Zotero citekeys");
    eprintln!("  cite lookup <citekey>                   Show reference details");
    eprintln!("  cv build                                Compile cv.typ to static/cv.pdf");
    eprintln!("  newsletter gen <post-path>              Generate newsletter .md from blog post");
    eprintln!("  newsletter send <slug> [--subject ...]  Send newsletter to subscribers");
    eprintln!("  pdf gen <post-path>                     Generate PDF from blog post");
    eprintln!("  pdf all                                 Generate PDFs for all posts (skips drafts)");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  site-tools cite all");
    eprintln!("  site-tools cite process content/blog/my-post/index.md");
    eprintln!("  site-tools cite list");
    eprintln!("  site-tools cite lookup @Smith2020");
    eprintln!("  site-tools newsletter gen content/blog/my-post/index.md");
    eprintln!("  site-tools newsletter send my-post");
    eprintln!("  site-tools pdf gen content/blog/my-post/index.md");
}

fn print_newsletter_usage() {
    eprintln!("site-tools newsletter — Generate and send newsletters");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  gen <post-path>              Parse blog post, clean for email, write to static/newsletter/<slug>.md");
    eprintln!("  send <slug> [--subject ...]  Send newsletter via API (reads ADMIN_KEY from .env)");
}

fn print_cv_usage() {
    eprintln!("site-tools cv — Build CV PDF");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  build  Compile cv.typ to static/cv.pdf");
}

fn print_pdf_usage() {
    eprintln!("site-tools pdf — Generate PDFs from blog posts");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  gen <post-path>  Preprocess markdown and compile with Typst");
    eprintln!("  all              Same, for every post under content/blog/");
    eprintln!();
    eprintln!("Drafts are skipped. Set INCLUDE_DRAFTS=1 to generate one anyway.");
}
