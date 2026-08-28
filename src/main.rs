mod audio;
mod bib;
mod cite;
mod codemask;
mod cv;
mod frontmatter;
mod markdown;
mod markers;
mod newsletter;
mod pdf;
mod sources;
mod speech;
mod util;
mod zotero;

use std::{env, process};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let result = match args[1].as_str() {
        "audio" => run_audio(&args[2..]),
        "cite" => cite::run(&args[2..]),
        "cv" => run_cv(&args[2..]),
        "markdown" => run_markdown(&args[2..]),
        "newsletter" => run_newsletter(&args[2..]),
        "pdf" => run_pdf(&args[2..]),
        "speech" => run_speech(&args[2..]),
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
        eprintln!("Usage: site-tools newsletter <gen|send|check-sendlog> ...");
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
        "check-sendlog" => {
            // Verifies the idempotency guard against the live server without sending
            // anything. Reads SEND_LOG_URL / JMAP_LIST_USER / JMAP_LIST_PASSWORD.
            newsletter::check_sendlog()
        }
        "-h" | "--help" | "help" => {
            print_newsletter_usage();
            Ok(())
        }
        other => Err(format!("Unknown newsletter subcommand: {other}")),
    }
}

fn run_markdown(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_markdown_usage();
        process::exit(1);
    }

    match args[0].as_str() {
        "gen" => {
            if args.len() < 2 {
                return Err("Usage: site-tools markdown gen <post-path>".to_string());
            }
            markdown::gen(&args[1])
        }
        "all" => markdown::gen_all(),
        "-h" | "--help" | "help" => {
            print_markdown_usage();
            Ok(())
        }
        other => Err(format!("Unknown markdown subcommand: {other}")),
    }
}

fn run_audio(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_audio_usage();
        process::exit(1);
    }

    let force = args.iter().any(|a| a == "--force");
    let dry_run = args.iter().any(|a| a == "--dry-run");

    match args[0].as_str() {
        "gen" => {
            if args.len() < 2 {
                return Err("Usage: site-tools audio gen <slug|post-path>".to_string());
            }
            audio::gen(&args[1], force, dry_run)
        }
        "all" => audio::gen_all(force, dry_run),
        "-h" | "--help" | "help" => {
            print_audio_usage();
            Ok(())
        }
        other => Err(format!("Unknown audio subcommand: {other}")),
    }
}

fn run_speech(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_speech_usage();
        process::exit(1);
    }

    match args[0].as_str() {
        "gen" => {
            if args.len() < 2 {
                return Err("Usage: site-tools speech gen <post-path>".to_string());
            }
            speech::gen(&args[1])
        }
        "all" => speech::gen_all(),
        "-h" | "--help" | "help" => {
            print_speech_usage();
            Ok(())
        }
        other => Err(format!("Unknown speech subcommand: {other}")),
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
    eprintln!("  audio gen <slug> [--force]              Synthesise the MP3 for one post");
    eprintln!("  audio all [--force] [--dry-run]         Same, for every post with a script");
    eprintln!("  cite process <post-path> [--style ...]  Replace @citekeys with formatted citations");
    eprintln!("  cite all [--style ...]                  Same, in place, for every post");
    eprintln!("  cite list                               List available Zotero citekeys");
    eprintln!("  cite lookup <citekey>                   Show reference details");
    eprintln!("  cv build                                Compile cv.typ to static/cv.pdf");
    eprintln!("  markdown gen <post-path>                Generate plain markdown for one post");
    eprintln!("  markdown all                            Same, for every post (skips drafts)");
    eprintln!("  newsletter gen <post-path>              Generate newsletter .md from blog post");
    eprintln!("  newsletter send <slug> [--subject ...]  Send newsletter to subscribers");
    eprintln!("  pdf gen <post-path>                     Generate PDF from blog post");
    eprintln!("  pdf all                                 Generate PDFs for all posts (skips drafts)");
    eprintln!("  speech gen <post-path>                  Write the spoken script for one post");
    eprintln!("  speech all                              Same, for every post (skips drafts)");
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

fn print_markdown_usage() {
    eprintln!("site-tools markdown — Emit plain markdown for content negotiation");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  gen <post-path>  Write static/blog/<slug>.md for one post");
    eprintln!("  all              Same, for every post, and prune stale files");
    eprintln!();
    eprintln!("Drafts are skipped. Set INCLUDE_DRAFTS=1 to generate one anyway.");
}

fn print_audio_usage() {
    eprintln!("site-tools audio — Synthesise spoken scripts into committed MP3s");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  gen <slug|post-path>  Synthesise static/audio/<slug>.mp3");
    eprintln!("  all                   Same, for every script under static/speech/");
    eprintln!();
    eprintln!("Flags:");
    eprintln!("  --force    Regenerate even when the script is unchanged");
    eprintln!("  --dry-run  Report what would be synthesised, and how many characters");
    eprintln!();
    eprintln!("Reads TTS_BACKEND, TTS_BASE_URL, TTS_API_KEY, TTS_MODEL and TTS_VOICE from");
    eprintln!("the environment, falling back to .env. Needs curl and ffmpeg on PATH.");
    eprintln!("A post is only re-synthesised when its script changes.");
}

fn print_speech_usage() {
    eprintln!("site-tools speech — Derive spoken scripts for text-to-speech");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  gen <post-path>  Write static/speech/<slug>.txt for one post");
    eprintln!("  all              Same, for every post, and prune stale scripts");
    eprintln!();
    eprintln!("Code, tables and the reference list become spoken markers. Pronunciation");
    eprintln!("overrides go in speech-lexicon.toml at the project root.");
    eprintln!();
    eprintln!("Drafts are skipped. Set INCLUDE_DRAFTS=1 to generate one anyway.");
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
