//! Hero images and social cards, generated from a prompt through OpenRouter.
//!
//! Each post that wants one carries `hero.prompt.txt`: a few sentences describing the
//! subject, and nothing about style. The house style lives once, at the project root:
//! `hero-style.txt` says it in words and `hero-style.webp`, if present, shows it. The
//! picture is sent along with every prompt as a reference, which holds the line weight,
//! paper and palette steady across posts far better than the words alone did. Change
//! either file and every post drawn afterwards follows.
//!
//! Two outputs from the one prompt:
//!
//! - `hero gen` asks for a text-free illustration and runs it through img-optim, so the
//!   post ends up with exactly the `hero.webp` and `hero-thumb.webp` a hand-made image
//!   would have. The page header and the front-page cards use those.
//! - `hero card` asks for the same subject with the post's title set into the picture
//!   and keeps it as `card.webp` next to the post, converted by img-optim at a higher
//!   quality and without a thumbnail. `og all` later re-renders it through Typst into
//!   the 1200x630 PNG the share metadata points at. WebP because the build's preflight
//!   refuses any JPEG or PNG under content/, and one rule with no exceptions is worth
//!   the second encode.
//!
//! Nothing here runs from build.sh. A generation costs money and no two are alike, so
//! it happens by hand, and the outputs are committed like every other image. Re-running
//! without `--force` never overwrites an existing hero or card.
//!
//! The API is OpenRouter's chat completions with `modalities: ["image", "text"]`; the
//! image comes back as a base64 data URL inside the message. The model is
//! `OPENROUTER_IMAGE_MODEL`, defaulting to Gemini 3.1 Flash Lite Image, which returns a
//! JPEG at 16:9 for about three and a half cents. Qwen-Image is not on OpenRouter as of
//! September 2026; a second backend is the place to add it if that changes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;

use crate::frontmatter;
use crate::util;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_MODEL: &str = "google/gemini-3.1-flash-lite-image";
const ATTEMPTS: u32 = 2;

pub const PROMPT_FILE: &str = "hero.prompt.txt";
pub const STYLE_FILE: &str = "hero-style.txt";
pub const REFERENCE_FILE: &str = "hero-style.webp";
pub const CARD_STEM: &str = "card";
const HERO_STEM: &str = "hero";
/// Text has to survive this, so the card is encoded harder than a photograph.
const CARD_QUALITY: &str = "90";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Hero,
    Card,
}

struct Config {
    api_key: String,
    model: String,
    style: String,
    /// The reference picture, already a data URL, or None when there is no file.
    reference: Option<String>,
    /// The bare host, "lindfors.no", printed on the card under the title.
    site: String,
    root: PathBuf,
}

impl Config {
    fn load(root: &Path) -> Result<Self, String> {
        let api_key = util::setting(root, "OPENROUTER_API_KEY")
            .ok_or("OPENROUTER_API_KEY is not set (environment or .env)")?;
        let model = util::setting(root, "OPENROUTER_IMAGE_MODEL")
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let style_path = root.join(STYLE_FILE);
        let style = fs::read_to_string(&style_path)
            .map_err(|e| format!("Failed to read {}: {e}", style_path.display()))?;

        let reference_path = root.join(REFERENCE_FILE);
        let reference = if reference_path.is_file() {
            let bytes = fs::read(&reference_path)
                .map_err(|e| format!("Failed to read {}: {e}", reference_path.display()))?;
            Some(data_url("image/webp", &bytes))
        } else {
            None
        };

        let zola = fs::read_to_string(root.join("zola.toml"))
            .map_err(|e| format!("Failed to read zola.toml: {e}"))?;
        let site = site_host(&zola).unwrap_or_else(|| "lindfors.no".to_string());

        Ok(Self {
            api_key,
            model,
            style: style.trim().to_string(),
            reference,
            site,
            root: root.to_path_buf(),
        })
    }
}

/// `base_url = "https://lindfors.no"` -> `lindfors.no`.
fn site_host(zola_toml: &str) -> Option<String> {
    let table: toml::Table = zola_toml.parse().ok()?;
    let url = table.get("base_url")?.as_str()?;
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    (!host.is_empty()).then(|| host.to_string())
}

pub fn gen(target: &str, force: bool) -> Result<(), String> {
    one(target, Kind::Hero, force)
}

pub fn card(target: &str, force: bool) -> Result<(), String> {
    one(target, Kind::Card, force)
}

/// Every post with a prompt file: a hero where none exists, a card where none exists.
///
/// Costs two generations per post that has neither, so it prints what it is about to
/// do and does nothing for posts without `hero.prompt.txt`.
pub fn gen_all(force: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;
    let blog = root.join("content/blog");

    let mut dirs: Vec<PathBuf> = fs::read_dir(&blog)
        .map_err(|e| format!("Failed to read {}: {e}", blog.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("index.md").is_file() && p.join(PROMPT_FILE).is_file())
        .collect();
    dirs.sort();

    if dirs.is_empty() {
        println!("No post has a {PROMPT_FILE}; nothing to generate.");
        return Ok(());
    }

    let cfg = Config::load(&root)?;
    let mut failures = 0;
    for dir in &dirs {
        for kind in [Kind::Hero, Kind::Card] {
            if let Err(e) = generate(&cfg, dir, kind, force) {
                eprintln!("  ERROR {}: {e}", dir.display());
                failures += 1;
            }
        }
    }

    if failures > 0 {
        return Err(format!("{failures} generation(s) failed"));
    }
    Ok(())
}

fn one(target: &str, kind: Kind, force: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;
    let dir = resolve_post_dir(&root, target)?;
    let cfg = Config::load(&root)?;
    generate(&cfg, &dir, kind, force)
}

/// A slug, a post directory, or a path to its index.md.
fn resolve_post_dir(root: &Path, target: &str) -> Result<PathBuf, String> {
    let path = Path::new(target);
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else if path.is_file() {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        root.join("content/blog").join(target)
    };

    if !dir.join("index.md").is_file() {
        return Err(format!("No post at {}", dir.display()));
    }
    Ok(dir)
}

fn generate(cfg: &Config, dir: &Path, kind: Kind, force: bool) -> Result<(), String> {
    let slug = frontmatter::slug_from_path(&dir.join("index.md"));

    let existing = match kind {
        Kind::Hero => dir.join("hero.webp").exists().then(|| "hero.webp".to_string()),
        Kind::Card => existing_card(dir).map(|_| format!("{CARD_STEM}.webp")),
    };
    if let Some(name) = existing {
        if !force {
            println!("{slug}: {name} exists, skipping (--force to regenerate)");
            return Ok(());
        }
    }

    let prompt_path = dir.join(PROMPT_FILE);
    let subject = fs::read_to_string(&prompt_path).map_err(|_| {
        format!(
            "{slug}: no {PROMPT_FILE}. Write a few sentences describing the subject \
             of the picture (style comes from {STYLE_FILE}) and re-run."
        )
    })?;
    let subject = subject.trim();
    if subject.is_empty() {
        return Err(format!("{slug}: {PROMPT_FILE} is empty"));
    }

    let content = fs::read_to_string(dir.join("index.md"))
        .map_err(|e| format!("Failed to read {}: {e}", dir.display()))?;
    let fm = frontmatter::parse(&content)?;

    let mut prompt = match kind {
        Kind::Hero => hero_prompt(&cfg.style, subject),
        Kind::Card => card_prompt(&cfg.style, subject, &fm.title, &cfg.site),
    };
    if cfg.reference.is_some() {
        prompt.push_str(REFERENCE_NOTE);
    }

    let what = match kind {
        Kind::Hero => "hero",
        Kind::Card => "card",
    };
    println!("{slug}: generating {what} with {}", cfg.model);

    let (bytes, ext, cost) = request_image(cfg, &prompt, &slug, what)?;

    match kind {
        Kind::Hero => {
            let source = dir.join(format!("{HERO_STEM}.{ext}"));
            fs::write(&source, &bytes)
                .map_err(|e| format!("Failed to write {}: {e}", source.display()))?;
            optimise(&cfg.root, &source, true, "80")?;
        }
        Kind::Card => {
            let source = dir.join(format!("{CARD_STEM}.{ext}"));
            fs::write(&source, &bytes)
                .map_err(|e| format!("Failed to write {}: {e}", source.display()))?;
            optimise(&cfg.root, &source, false, CARD_QUALITY)?;
        }
    }

    if let Some(cost) = cost {
        println!("  cost ${cost:.4}");
    }
    Ok(())
}

/// The committed card next to a post.
pub fn existing_card(dir: &Path) -> Option<PathBuf> {
    let path = dir.join(format!("{CARD_STEM}.webp"));
    path.is_file().then_some(path)
}

/// Appended when a reference picture rides along with the prompt.
const REFERENCE_NOTE: &str = "

The attached image is a style reference from the same series: match its line weight, paper, palette and amount of empty space exactly. Do not copy its subject.";

fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// The user message: the prompt, plus the reference picture when there is one.
fn message_content(prompt: &str, reference: Option<&str>) -> serde_json::Value {
    match reference {
        None => serde_json::Value::String(prompt.to_string()),
        Some(url) => serde_json::json!([
            { "type": "text", "text": prompt },
            { "type": "image_url", "image_url": { "url": url } },
        ]),
    }
}

fn hero_prompt(style: &str, subject: &str) -> String {
    format!(
        "{style}\n\nSubject: {subject}\n\n\
         Landscape, 16:9. No text, no letters, no words, no numbers and no logos \
         anywhere in the image."
    )
}

fn card_prompt(style: &str, subject: &str, title: &str, site: &str) -> String {
    format!(
        "{style}\n\nSubject: {subject}\n\n\
         This is a social media card, landscape 16:9. The illustration occupies the \
         right two thirds of the image. The left third carries two pieces of text and \
         nothing else. First, set large in a classic serif typeface in deep navy, \
         left-aligned and broken over two to four lines, this title, spelled exactly \
         as written:\n\n{title}\n\n\
         Second, below it, small and in coral, this site name and nothing more:\n\n\
         {site}\n\n\
         No other words, labels or captions anywhere in the image."
    )
}

/// One image from the model: the bytes, a file extension for them, and what it cost.
fn request_image(
    cfg: &Config,
    prompt: &str,
    slug: &str,
    what: &str,
) -> Result<(Vec<u8>, String, Option<f64>), String> {
    let work = std::env::temp_dir().join(format!("site-tools-hero-{slug}-{what}"));
    fs::create_dir_all(&work).map_err(|e| format!("Failed to create {}: {e}", work.display()))?;
    let body_path = work.join("request.json");
    let resp_path = work.join("response.json");

    let body = serde_json::json!({
        "model": cfg.model,
        "modalities": ["image", "text"],
        "image_config": { "aspect_ratio": "16:9" },
        "messages": [{ "role": "user", "content": message_content(prompt, cfg.reference.as_deref()) }],
    });
    fs::write(&body_path, body.to_string())
        .map_err(|e| format!("Failed to write request body: {e}"))?;

    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        let output = Command::new("curl")
            .args([
                "-sS",
                "-X",
                "POST",
                ENDPOINT,
                "-H",
                "Content-Type: application/json",
                "-H",
                &format!("Authorization: Bearer {}", cfg.api_key),
                "--data-binary",
                &format!("@{}", body_path.display()),
                "-o",
                &resp_path.display().to_string(),
                "-w",
                "%{http_code}",
            ])
            .output()
            .map_err(|e| format!("Failed to run curl: {e}"))?;

        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let response = fs::read_to_string(&resp_path).unwrap_or_default();

        match extract_image(&status, &response) {
            Ok(found) => {
                let _ = fs::remove_dir_all(&work);
                return Ok(found);
            }
            Err(e) => last = e,
        }

        if attempt < ATTEMPTS {
            eprintln!("    attempt {attempt} failed ({last}), retrying");
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }

    let _ = fs::remove_dir_all(&work);
    Err(last)
}

/// Pull the first image out of a chat-completions response.
///
/// A 200 is not proof of an image: the model can answer with text only, or OpenRouter
/// with an error object, and both are JSON. The data URL is the only thing accepted.
fn extract_image(status: &str, response: &str) -> Result<(Vec<u8>, String, Option<f64>), String> {
    let snippet = |s: &str| s.chars().take(200).collect::<String>();

    if status != "200" {
        return Err(format!("HTTP {status}: {}", snippet(response.trim())));
    }

    let value: serde_json::Value =
        serde_json::from_str(response).map_err(|e| format!("response is not JSON: {e}"))?;

    if let Some(err) = value.get("error") {
        return Err(format!("API error: {}", snippet(&err.to_string())));
    }

    let message = &value["choices"][0]["message"];
    let url = message["images"][0]["image_url"]["url"]
        .as_str()
        .ok_or_else(|| {
            let text = message["content"].as_str().unwrap_or("");
            format!("no image in response; text was: {}", snippet(text))
        })?;

    let (mime, b64) = split_data_url(url).ok_or("image URL is not a base64 data URL")?;
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        other => return Err(format!("unexpected image type {other}")),
    };

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("image is not valid base64: {e}"))?;
    if bytes.is_empty() {
        return Err("image is empty".to_string());
    }

    let cost = value["usage"]["cost"].as_f64();
    Ok((bytes, ext.to_string(), cost))
}

/// `data:image/png;base64,AAAA` -> ("image/png", "AAAA").
fn split_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, payload) = rest.split_once(";base64,")?;
    (!mime.is_empty() && !payload.is_empty()).then_some((mime, payload))
}

/// Convert the model's file into a WebP beside it and delete the source.
///
/// Delegated to img-optim, the same binary used for hand-made images, so a generated
/// hero is indistinguishable from a photographed one in the repo. The source is deleted
/// only once the WebP exists; a leftover would otherwise trip the build's image
/// preflight, which is the right outcome for a failed conversion.
fn optimise(root: &Path, source: &Path, thumbnail: bool, quality: &str) -> Result<(), String> {
    let bin = img_optim_bin(root)?;
    let mut cmd = Command::new(&bin);
    cmd.arg("-q").arg(quality);
    if thumbnail {
        cmd.arg("-t");
    }
    let status = cmd
        .arg(source)
        .status()
        .map_err(|e| format!("Failed to run {}: {e}", bin.display()))?;
    if !status.success() {
        return Err(format!("img-optim failed with status {status}"));
    }

    let webp = source.with_extension("webp");
    if !webp.is_file() {
        return Err(format!("img-optim did not produce {}", webp.display()));
    }
    fs::remove_file(source).map_err(|e| format!("Failed to remove {}: {e}", source.display()))?;
    println!("  wrote {}", webp.display());
    Ok(())
}

/// The img-optim binary, built if it is not there yet.
fn img_optim_bin(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join("tools/img-optim");
    let candidates = [
        dir.join("target/release/img-optim.exe"),
        dir.join("target/release/img-optim"),
    ];
    if let Some(bin) = candidates.iter().find(|p| p.is_file()) {
        return Ok(bin.clone());
    }

    println!("  building img-optim");
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&dir)
        .status()
        .map_err(|e| format!("Failed to run cargo: {e}"))?;
    if !status.success() {
        return Err("cargo build of img-optim failed".to_string());
    }

    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| "img-optim built but no binary found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_splits_into_mime_and_payload() {
        assert_eq!(
            split_data_url("data:image/png;base64,AAAA"),
            Some(("image/png", "AAAA"))
        );
        assert_eq!(split_data_url("https://example.com/a.png"), None);
        assert_eq!(split_data_url("data:image/png;base64,"), None);
    }

    #[test]
    fn a_text_only_answer_is_an_error_not_an_image() {
        let body = r#"{"choices":[{"message":{"content":"I cannot draw that.","images":[]}}]}"#;
        let err = extract_image("200", body).unwrap_err();
        assert!(err.contains("no image"), "{err}");
        assert!(err.contains("cannot draw"), "{err}");
    }

    #[test]
    fn an_error_object_with_a_200_is_still_an_error() {
        let body = r#"{"error":{"message":"rate limited","code":429}}"#;
        let err = extract_image("200", body).unwrap_err();
        assert!(err.contains("rate limited"), "{err}");
    }

    #[test]
    fn a_data_url_decodes_to_bytes_with_the_right_extension() {
        // "hello" in base64, labelled as a JPEG.
        let body = r#"{"choices":[{"message":{"images":[{"image_url":{"url":"data:image/jpeg;base64,aGVsbG8="}}]}}],"usage":{"cost":0.0336}}"#;
        let (bytes, ext, cost) = extract_image("200", body).unwrap();
        assert_eq!(bytes, b"hello");
        assert_eq!(ext, "jpg");
        assert_eq!(cost, Some(0.0336));
    }

    #[test]
    fn non_200_reports_the_status_and_body() {
        let err = extract_image("402", r#"{"error":"insufficient credits"}"#).unwrap_err();
        assert!(err.starts_with("HTTP 402"), "{err}");
        assert!(err.contains("insufficient"), "{err}");
    }

    #[test]
    fn card_prompt_carries_the_exact_title_and_the_site() {
        let p = card_prompt("Ink and wash.", "A drawer.", "Zola has no plugins", "lindfors.no");
        assert!(p.starts_with("Ink and wash."));
        assert!(p.contains("Subject: A drawer."));
        assert!(p.contains("\n\nZola has no plugins\n\n"));
        assert!(p.contains("\n\nlindfors.no\n\n"));
    }

    #[test]
    fn hero_prompt_forbids_text() {
        let p = hero_prompt("Ink and wash.", "A drawer.");
        assert!(p.contains("No text"));
        assert!(!p.contains("title"));
    }

    #[test]
    fn a_reference_makes_the_message_multimodal() {
        let plain = message_content("draw", None);
        assert_eq!(plain, serde_json::Value::String("draw".into()));

        let with = message_content("draw", Some("data:image/webp;base64,AAAA"));
        assert_eq!(with[0]["type"], "text");
        assert_eq!(with[0]["text"], "draw");
        assert_eq!(with[1]["type"], "image_url");
        assert_eq!(with[1]["image_url"]["url"], "data:image/webp;base64,AAAA");
    }

    #[test]
    fn data_urls_carry_the_mime_and_base64() {
        assert_eq!(data_url("image/webp", b"hello"), "data:image/webp;base64,aGVsbG8=");
    }

    #[test]
    fn site_host_strips_the_scheme() {
        assert_eq!(
            site_host("base_url = \"https://lindfors.no\"\ntitle = \"x\"\n"),
            Some("lindfors.no".to_string())
        );
        assert_eq!(site_host("title = \"x\"\n"), None);
    }
}
