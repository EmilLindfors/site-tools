//! Synthesise the spoken scripts into committed MP3s.
//!
//! Reads `static/speech/<slug>.txt`, sends each block to a TTS API, joins the returned
//! WAVs with silence at the block gaps, and encodes one MP3 per post. Output and a
//! sidecar describing what produced it land in `static/audio/`, committed alongside the
//! PDFs — Cloudflare Pages runs its own `zola build`, so nothing generated here
//! survives unless it is in the repo.
//!
//! A post is only re-synthesised when its script changes, which is what keeps the GPU
//! (or the API) out of the critical path of an ordinary build. With the script
//! unchanged this command makes no network calls at all.
//!
//! HTTP goes through `curl`, as it does in `newsletter::send`: this binary depends on
//! `toml` and `zotero-cite`, and one build-time task is not worth an async runtime.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const CACHE_DIR: &str = ".audio-cache";

/// Silence inserted between blocks, in seconds. Two blank lines in the script mean a
/// section break and get the longer gap.
const PARAGRAPH_GAP: f32 = 0.45;
const SECTION_GAP: f32 = 0.9;

/// Mono at 64 kbps: about 8 MB for a 17-minute post, comfortably inside Cloudflare
/// Pages' 25 MiB per-file limit, and spoken word does not need more.
const BITRATE: &str = "64k";

/// EBU R128, the podcast convention. Without it, volume drifts between posts
/// synthesised weeks apart.
const LOUDNORM: &str = "loudnorm=I=-16:TP=-1.5:LRA=11";

/// Retries per block. The failure this covers is a dropped connection or a busy GPU,
/// not a bad request, so it is deliberately small.
const ATTEMPTS: usize = 3;

/// Which API shape to speak.
///
/// A closed set of two, so static dispatch rather than a trait object. Fish is the
/// native shape; the OpenAI shape covers Kokoro-FastAPI, OpenAI itself, and the
/// OpenAI-compatible Fish wrappers.
#[derive(Clone, Copy, PartialEq)]
enum Backend {
    Fish,
    OpenAi,
}

impl Backend {
    fn parse(name: &str) -> Result<Self, String> {
        match name.to_lowercase().as_str() {
            "fish" | "fishaudio" => Ok(Backend::Fish),
            "openai" | "openai-compatible" => Ok(Backend::OpenAi),
            other => Err(format!("Unknown TTS_BACKEND '{other}' (expected fish or openai)")),
        }
    }

    fn path(self) -> &'static str {
        match self {
            Backend::Fish => "/v1/tts",
            Backend::OpenAi => "/v1/audio/speech",
        }
    }
}

struct Config {
    backend: Backend,
    base_url: String,
    api_key: Option<String>,
    model: String,
    voice: Option<String>,
}

impl Config {
    /// Read from the environment, falling back to `.env`.
    fn load(root: &Path) -> Result<Self, String> {
        let backend = match crate::util::setting(root, "TTS_BACKEND") {
            Some(name) => Backend::parse(&name)?,
            None => Backend::Fish,
        };

        let base_url = crate::util::setting(root, "TTS_BASE_URL")
            .unwrap_or_else(|| "https://api.fish.audio".to_string())
            .trim_end_matches('/')
            .to_string();

        // s2.1-pro-free is free through 2026-08-31; after that, or on a self-hosted
        // box, set TTS_MODEL explicitly.
        let model = crate::util::setting(root, "TTS_MODEL")
            .unwrap_or_else(|| match backend {
                Backend::Fish => "s2.1-pro-free".to_string(),
                Backend::OpenAi => "tts-1".to_string(),
            });

        Ok(Config {
            backend,
            base_url,
            api_key: crate::util::setting(root, "TTS_API_KEY"),
            model,
            voice: crate::util::setting(root, "TTS_VOICE"),
        })
    }

    fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, self.backend.path())
    }

    /// Request body for one block.
    fn body(&self, text: &str) -> String {
        let text = json_escape(text);

        match self.backend {
            Backend::Fish => {
                let voice = match &self.voice {
                    Some(id) => format!(r#""reference_id":"{}","#, json_escape(id)),
                    None => String::new(),
                };
                format!(
                    r#"{{"text":"{text}",{voice}"format":"wav","normalize":true,"latency":"normal"}}"#
                )
            }
            Backend::OpenAi => {
                let voice = self.voice.as_deref().unwrap_or("alloy");
                format!(
                    r#"{{"model":"{}","input":"{text}","voice":"{}","response_format":"wav"}}"#,
                    json_escape(&self.model),
                    json_escape(voice)
                )
            }
        }
    }
}

/// One block of the script, with the silence that precedes it.
struct Chunk {
    text: String,
    hash: String,
    section: bool,
}

/// FNV-1a, 64-bit. A cache key, not a signature: the question it answers is "is this
/// block byte-identical to the one already synthesised".
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Read one string field out of a sidecar this program wrote.
///
/// Deliberately narrow: it parses our own output, not arbitrary JSON, which is why it
/// is here instead of a serde dependency.
fn json_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\": \"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Split a script into blocks. One blank line separates blocks; two or more mark a
/// section break, which earns a longer silence.
fn parse_script(script: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    // The gap that precedes the block being accumulated, not the one after it.
    let mut section = false;

    for line in script.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            continue;
        }

        if !current.is_empty() && blank_run > 0 {
            push_chunk(&mut out, &current.join(" "), section);
            current.clear();
            section = blank_run >= 2;
        }
        blank_run = 0;
        current.push(line.trim());
    }

    if !current.is_empty() {
        push_chunk(&mut out, &current.join(" "), section);
    }

    out
}

fn push_chunk(out: &mut Vec<Chunk>, text: &str, section: bool) {
    if text.is_empty() {
        return;
    }
    out.push(Chunk {
        text: text.to_string(),
        hash: hash(text),
        section,
    });
}

/// Synthesise one block to `out_path`. Retries a failed call before giving up.
fn synthesize(cfg: &Config, chunk: &Chunk, out_path: &Path, cache: &Path) -> Result<(), String> {
    // The body goes through a file: block text runs to hundreds of characters and
    // quoting it into an argv on Windows is a losing game.
    let body_path = cache.join(format!("{}.json", chunk.hash));
    fs::write(&body_path, cfg.body(&chunk.text))
        .map_err(|e| format!("Failed to write request body: {e}"))?;

    let mut last = String::new();

    for attempt in 1..=ATTEMPTS {
        let mut args: Vec<String> = vec![
            "-sS".into(),
            "-X".into(),
            "POST".into(),
            cfg.endpoint(),
            "-H".into(),
            "Content-Type: application/json".into(),
            "--data-binary".into(),
            format!("@{}", body_path.display()),
            "-o".into(),
            out_path.display().to_string(),
            "-w".into(),
            "%{http_code}".into(),
        ];

        if let Some(key) = &cfg.api_key {
            args.push("-H".into());
            args.push(format!("Authorization: Bearer {key}"));
        }

        // Fish takes the model in a header; the OpenAI shape has it in the body.
        if cfg.backend == Backend::Fish {
            args.push("-H".into());
            args.push(format!("model: {}", cfg.model));
        }

        let output = Command::new("curl")
            .args(&args)
            .output()
            .map_err(|e| format!("Failed to run curl: {e}"))?;

        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if status == "200" {
            // A 200 is not proof of audio: an API can answer with a JSON error body.
            // Checking the RIFF header keeps that from being cached as a valid block
            // and concatenated into the MP3 as silence or noise.
            if is_wav(out_path) {
                let _ = fs::remove_file(&body_path);
                return Ok(());
            }
            last = "response was not a WAV".to_string();
        } else {
            // On an error the response body is the API's error JSON, not audio.
            let detail = fs::read_to_string(out_path).unwrap_or_default();
            let detail = detail.trim();
            last = format!(
                "HTTP {status}{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", detail.chars().take(200).collect::<String>())
                }
            );
        }

        if attempt < ATTEMPTS {
            eprintln!("    attempt {attempt} failed ({last}), retrying");
            std::thread::sleep(std::time::Duration::from_secs(attempt as u64 * 2));
        }
    }

    // Whatever curl left behind is an error body, not audio. Leaving it would look
    // like a cached block on the next run and be concatenated into the MP3.
    let _ = fs::remove_file(out_path);
    let _ = fs::remove_file(&body_path);
    Err(format!("Synthesis failed after {ATTEMPTS} attempts: {last}"))
}

/// True when the file carries a RIFF/WAVE header.
fn is_wav(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Drop cached blocks that the current script no longer references.
///
/// A block is about a megabyte of WAV. Without this, every edit to a post leaves its
/// old blocks behind and the cache grows without bound.
fn prune_cache(cache: &Path, chunks: &[Chunk]) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if !chunks.iter().any(|c| c.hash == stem) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Build a silence file from a synthesised block, so its sample rate and channel count
/// match by construction rather than by guessing what the model returns.
fn make_silence(reference: &Path, seconds: f32, out_path: &Path) -> Result<(), String> {
    run_ffmpeg(&[
        "-y",
        "-i",
        &reference.display().to_string(),
        "-af",
        "volume=0",
        "-t",
        &seconds.to_string(),
        &out_path.display().to_string(),
    ])
}

fn run_ffmpeg(args: &[&str]) -> Result<(), String> {
    let output = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {e} (is it on PATH?)"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(())
}

/// Duration in seconds, via ffprobe. Falls back to the CBR size calculation.
fn duration_seconds(mp3: &Path) -> usize {
    let probed = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=nw=1:nk=1",
            &mp3.display().to_string(),
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok());

    if let Some(seconds) = probed {
        return seconds.round() as usize;
    }

    let bytes = fs::metadata(mp3).map(|m| m.len()).unwrap_or(0);
    (bytes * 8 / 64_000) as usize
}

/// The concat demuxer wants forward slashes and single-quoted paths.
fn concat_entry(path: &Path) -> String {
    format!("file '{}'\n", path.display().to_string().replace('\\', "/"))
}

fn write_sidecar(
    path: &Path,
    slug: &str,
    script_hash: &str,
    cfg: &Config,
    chunks: &[Chunk],
    mp3: &Path,
) -> Result<usize, String> {
    let seconds = duration_seconds(mp3);
    let bytes = fs::metadata(mp3).map(|m| m.len()).unwrap_or(0);

    let mut json = String::new();
    json.push_str("{\n");
    json.push_str(&format!("  \"slug\": \"{}\",\n", json_escape(slug)));
    json.push_str(&format!("  \"script_hash\": \"{script_hash}\",\n"));
    json.push_str(&format!("  \"model\": \"{}\",\n", json_escape(&cfg.model)));
    json.push_str(&format!(
        "  \"voice\": \"{}\",\n",
        json_escape(cfg.voice.as_deref().unwrap_or("default"))
    ));
    json.push_str(&format!("  \"duration_seconds\": {seconds},\n"));
    json.push_str(&format!(
        "  \"duration\": \"{}:{:02}\",\n",
        seconds / 60,
        seconds % 60
    ));
    json.push_str(&format!("  \"bytes\": {bytes},\n"));
    json.push_str(&format!("  \"blocks\": {}\n", chunks.len()));
    json.push_str("}\n");

    fs::write(path, &json).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok(seconds)
}

struct Options {
    force: bool,
    dry_run: bool,
}

/// Synthesise one post. Returns the duration in seconds, or None when nothing was done.
fn gen_one(root: &Path, slug: &str, cfg: &Config, opts: &Options) -> Result<Option<usize>, String> {
    let script_path = root.join("static/speech").join(format!("{slug}.txt"));
    let script = fs::read_to_string(&script_path)
        .map_err(|e| format!("Failed to read {}: {e}", script_path.display()))?;

    let script_hash = hash(&script);
    let out_dir = root.join("static/audio");
    let mp3_path = out_dir.join(format!("{slug}.mp3"));
    let sidecar_path = out_dir.join(format!("{slug}.json"));

    let current = fs::read_to_string(&sidecar_path)
        .ok()
        .and_then(|json| json_field(&json, "script_hash").map(str::to_string));

    if !opts.force && mp3_path.exists() && current.as_deref() == Some(script_hash.as_str()) {
        return Ok(None);
    }

    let chunks = parse_script(&script);
    if chunks.is_empty() {
        return Err(format!("{} has no blocks", script_path.display()));
    }

    let chars: usize = chunks.iter().map(|c| c.text.chars().count()).sum();

    if opts.dry_run {
        println!("{slug}: would synthesise {} blocks, {chars} characters", chunks.len());
        return Ok(None);
    }

    println!("{slug}: {} blocks, {chars} characters", chunks.len());

    let cache = root.join(CACHE_DIR).join(slug);
    fs::create_dir_all(&cache).map_err(|e| format!("Failed to create {}: {e}", cache.display()))?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    let mut synthesised = 0usize;
    for (i, chunk) in chunks.iter().enumerate() {
        let wav = cache.join(format!("{}.wav", chunk.hash));
        if wav.exists() {
            continue;
        }
        print!("  block {}/{}\r", i + 1, chunks.len());
        let _ = std::io::stdout().flush();
        synthesize(cfg, chunk, &wav, &cache)?;
        synthesised += 1;
    }
    println!("  synthesised {synthesised}, reused {}", chunks.len() - synthesised);

    // Silence is cut from a real block so its format matches the chunks exactly; the
    // concat demuxer rejects a stream whose parameters differ.
    let first = cache.join(format!("{}.wav", chunks[0].hash));
    let para_gap = cache.join("gap-paragraph.wav");
    let section_gap = cache.join("gap-section.wav");
    make_silence(&first, PARAGRAPH_GAP, &para_gap)?;
    make_silence(&first, SECTION_GAP, &section_gap)?;

    let mut list = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if i > 0 {
            list.push_str(&concat_entry(if chunk.section { &section_gap } else { &para_gap }));
        }
        list.push_str(&concat_entry(&cache.join(format!("{}.wav", chunk.hash))));
    }

    let list_path = cache.join("concat.txt");
    fs::write(&list_path, &list).map_err(|e| format!("Failed to write concat list: {e}"))?;

    // One encode over the joined PCM: stitching MP3s instead would add a frame-boundary
    // gap at every block and a second generation of encoding loss.
    run_ffmpeg(&[
        "-y",
        "-f", "concat",
        "-safe", "0",
        "-i", &list_path.display().to_string(),
        "-af", LOUDNORM,
        "-ac", "1",
        "-codec:a", "libmp3lame",
        "-b:a", BITRATE,
        &mp3_path.display().to_string(),
    ])?;

    let seconds = write_sidecar(&sidecar_path, slug, &script_hash, cfg, &chunks, &mp3_path)?;
    let bytes = fs::metadata(&mp3_path).map(|m| m.len()).unwrap_or(0);
    prune_cache(&cache, &chunks);

    println!(
        "Audio: {} ({}:{:02}, {:.1} MB)",
        mp3_path.display(),
        seconds / 60,
        seconds % 60,
        bytes as f64 / 1_048_576.0
    );

    Ok(Some(seconds))
}

/// Synthesise one post by slug or post path.
pub fn gen(target: &str, force: bool, dry_run: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = crate::util::find_project_root(&cwd)?;
    let cfg = Config::load(&root)?;

    let slug = Path::new(target)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| *n != "index.md")
        .map(|n| n.trim_end_matches(".md").to_string())
        .unwrap_or_else(|| crate::frontmatter::slug_from_path(Path::new(target)));

    let opts = Options { force, dry_run };
    if gen_one(&root, &slug, &cfg, &opts)?.is_none() && !dry_run {
        println!("{slug}: up to date");
    }

    Ok(())
}

/// Synthesise every post that has a script, and prune audio with no script behind it.
pub fn gen_all(force: bool, dry_run: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to read cwd: {e}"))?;
    let root = crate::util::find_project_root(&cwd)?;
    let cfg = Config::load(&root)?;

    let speech_dir = root.join("static/speech");
    if !speech_dir.is_dir() {
        return Err(format!(
            "No scripts at {} — run `site-tools speech all` first",
            speech_dir.display()
        ));
    }

    let mut slugs: Vec<String> = fs::read_dir(&speech_dir)
        .map_err(|e| format!("Failed to read {}: {e}", speech_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("txt"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    slugs.sort();

    if slugs.is_empty() {
        return Err(format!("No scripts found under {}", speech_dir.display()));
    }

    println!("Backend: {} at {}", cfg.model, cfg.base_url);

    let opts = Options { force, dry_run };
    let mut total = 0usize;
    let mut generated = 0usize;

    for slug in &slugs {
        match gen_one(&root, slug, &cfg, &opts)? {
            Some(seconds) => {
                total += seconds;
                generated += 1;
            }
            None => total += existing_duration(&root, slug),
        }
    }

    if dry_run {
        return Ok(());
    }

    println!(
        "{generated} regenerated, {} unchanged, {}:{:02} total",
        slugs.len() - generated,
        total / 60,
        total % 60
    );

    prune(&root, &slugs)
}

/// Duration recorded in an existing sidecar, for the run summary.
fn existing_duration(root: &Path, slug: &str) -> usize {
    fs::read_to_string(root.join("static/audio").join(format!("{slug}.json")))
        .ok()
        .and_then(|json| {
            let needle = "\"duration_seconds\": ";
            let start = json.find(needle)? + needle.len();
            let rest = &json[start..];
            let end = rest.find(',')?;
            rest[..end].trim().parse::<usize>().ok()
        })
        .unwrap_or(0)
}

/// Delete audio whose script is gone, so an unpublished post stops serving its audio.
fn prune(root: &Path, live: &[String]) -> Result<(), String> {
    let out_dir = root.join("static/audio");
    if !out_dir.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(&out_dir)
        .map_err(|e| format!("Failed to read {}: {e}", out_dir.display()))?
        .flatten()
    {
        let path: PathBuf = entry.path();
        let is_output = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("mp3") | Some("json")
        );
        if !is_output {
            continue;
        }

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        if !live.contains(&stem) {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
            println!("Removed stale audio: {}", path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_blank_line_separates_blocks() {
        let chunks = parse_script("First block.\n\nSecond block.\n");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1].text, "Second block.");
        assert!(!chunks[1].section);
    }

    /// The extra blank line is how `speech` marks a section break, and it is the only
    /// thing that tells this step to insert the longer silence.
    #[test]
    fn two_blank_lines_mark_a_section() {
        let chunks = parse_script("First.\n\n\nA heading.\n\nAfter.\n");
        assert_eq!(chunks.len(), 3);
        assert!(chunks[1].section);
        assert!(!chunks[2].section);
    }

    #[test]
    fn wrapped_lines_inside_a_block_join() {
        let chunks = parse_script("A block that\nwraps.\n\nNext.\n");
        assert_eq!(chunks[0].text, "A block that wraps.");
    }

    #[test]
    fn identical_text_hashes_identically() {
        assert_eq!(hash("some block"), hash("some block"));
        assert_ne!(hash("some block"), hash("some block."));
    }

    #[test]
    fn quotes_and_newlines_are_escaped_for_json() {
        assert_eq!(json_escape(r#"a "b"\c"#), r#"a \"b\"\\c"#);
        assert_eq!(json_escape("a\nb"), "a\\nb");
    }

    #[test]
    fn sidecar_field_reads_back() {
        let json = "{\n  \"slug\": \"a-post\",\n  \"script_hash\": \"deadbeef\",\n}";
        assert_eq!(json_field(json, "script_hash"), Some("deadbeef"));
        assert_eq!(json_field(json, "missing"), None);
    }

    #[test]
    fn fish_body_carries_the_voice_and_asks_for_wav() {
        let cfg = Config {
            backend: Backend::Fish,
            base_url: "https://api.fish.audio".into(),
            api_key: None,
            model: "s2.1-pro-free".into(),
            voice: Some("abc123".into()),
        };
        let body = cfg.body("Hello.");
        assert!(body.contains(r#""text":"Hello.""#));
        assert!(body.contains(r#""reference_id":"abc123""#));
        assert!(body.contains(r#""format":"wav""#));
        assert_eq!(cfg.endpoint(), "https://api.fish.audio/v1/tts");
    }

    /// The model is a header for Fish and a body field for the OpenAI shape; getting
    /// that backwards produces a default-voice MP3 rather than an error.
    #[test]
    fn openai_body_carries_the_model() {
        let cfg = Config {
            backend: Backend::OpenAi,
            base_url: "http://gpu-box:8080".into(),
            api_key: None,
            model: "kokoro".into(),
            voice: None,
        };
        let body = cfg.body("Hello.");
        assert!(body.contains(r#""model":"kokoro""#));
        assert!(body.contains(r#""input":"Hello.""#));
        assert_eq!(cfg.endpoint(), "http://gpu-box:8080/v1/audio/speech");
    }

    #[test]
    fn unknown_backend_is_rejected() {
        assert!(Backend::parse("elevenlabs").is_err());
        assert!(Backend::parse("Fish").is_ok());
    }

    #[test]
    fn concat_entries_use_forward_slashes() {
        let entry = concat_entry(Path::new(r"C:\site\.audio-cache\a\b.wav"));
        assert_eq!(entry, "file 'C:/site/.audio-cache/a/b.wav'\n");
    }
}
