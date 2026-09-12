use image::GenericImageView;
use std::path::{Path, PathBuf};
use std::{env, fs, process};

const THUMB_WIDTH: u32 = 600;
const THUMB_QUALITY: f32 = 75.0;
const THUMB_SUFFIX: &str = "-thumb";

/// What the command line asked for.
#[derive(Debug, PartialEq)]
struct Args {
    paths: Vec<PathBuf>,
    max_width: u32,
    quality: f32,
    thumbnails: bool,
    help: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            max_width: 1200,
            quality: 80.0,
            thumbnails: false,
            help: false,
        }
    }
}

/// Parse argv, everything after the program name.
///
/// A flag whose value is missing or unparseable is an error rather than a panic: `-q`
/// at the end of the line used to index past the end of argv.
fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = argv.iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-w" | "--max-width" => {
                args.max_width = value_of(&mut it, arg)?
                    .parse()
                    .map_err(|_| format!("{arg} wants a width in pixels"))?;
            }
            "-q" | "--quality" => {
                let quality: f32 = value_of(&mut it, arg)?
                    .parse()
                    .map_err(|_| format!("{arg} wants a number from 0 to 100"))?;
                if !(0.0..=100.0).contains(&quality) {
                    return Err(format!("{arg} wants a number from 0 to 100, not {quality}"));
                }
                args.quality = quality;
            }
            "-t" | "--thumbnails" => args.thumbnails = true,
            "-h" | "--help" => args.help = true,
            other if other.starts_with('-') => return Err(format!("Unknown flag: {other}")),
            path => args.paths.push(PathBuf::from(path)),
        }
    }

    Ok(args)
}

fn value_of<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();

    let args = match parse_args(&argv) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            print_usage();
            process::exit(1);
        }
    };

    if args.help {
        print_usage();
        return;
    }

    let Args {
        paths,
        max_width,
        quality,
        thumbnails,
        ..
    } = args;

    if paths.is_empty() {
        print_usage();
        process::exit(1);
    }

    let files = collect_files(&paths);
    if files.is_empty() {
        eprintln!("No convertible images found (jpg, jpeg, png, gif, bmp, tiff)");
        process::exit(1);
    }

    let mut total_before: u64 = 0;
    let mut total_after: u64 = 0;

    for file in &files {
        let result = if is_animated_gif(file) {
            optimize_animated_gif(file, max_width, quality)
        } else {
            optimize(file, max_width, quality)
        };

        match result {
            Ok((before, after, out)) => {
                let saved = 100.0 - (after as f64 / before as f64 * 100.0);
                println!(
                    "  {} -> {} ({} -> {}, -{:.0}%)",
                    file.display(),
                    out.file_name().unwrap().to_string_lossy(),
                    fmt_size(before),
                    fmt_size(after),
                    saved,
                );
                total_before += before;
                total_after += after;

                if thumbnails && !is_animated_gif(file) {
                    match thumbnail(file, THUMB_WIDTH, THUMB_QUALITY) {
                        Ok((sz, thumb_path)) => {
                            println!(
                                "  {} -> {} ({})",
                                file.display(),
                                thumb_path.file_name().unwrap().to_string_lossy(),
                                fmt_size(sz),
                            );
                            total_after += sz;
                        }
                        Err(e) => eprintln!("  THUMB ERROR {}: {e}", file.display()),
                    }
                }
            }
            Err(e) => eprintln!("  ERROR {}: {e}", file.display()),
        }
    }

    if files.len() > 1 {
        let saved = 100.0 - (total_after as f64 / total_before as f64 * 100.0);
        println!(
            "\n  Total: {} -> {} (-{:.0}%)",
            fmt_size(total_before),
            fmt_size(total_after),
            saved,
        );
    }
}

// ---------------------------------------------------------------------------
// Static image optimization
// ---------------------------------------------------------------------------

fn optimize(
    path: &Path,
    max_width: u32,
    quality: f32,
) -> Result<(u64, u64, PathBuf), Box<dyn std::error::Error>> {
    let before = fs::metadata(path)?.len();
    let img = image::open(path)?;
    let img = resize_to_width(img, max_width);

    let out_path = path.with_extension("webp");
    encode_webp(&img, &out_path, quality)?;

    let after = fs::metadata(&out_path)?.len();
    Ok((before, after, out_path))
}

fn thumbnail(
    path: &Path,
    width: u32,
    quality: f32,
) -> Result<(u64, PathBuf), Box<dyn std::error::Error>> {
    let img = image::open(path)?;
    let img = resize_to_width(img, width);

    let stem = path.file_stem().unwrap().to_string_lossy();
    let out_path = path.with_file_name(format!("{stem}{THUMB_SUFFIX}.webp"));
    encode_webp(&img, &out_path, quality)?;

    let size = fs::metadata(&out_path)?.len();
    Ok((size, out_path))
}

fn resize_to_width(img: image::DynamicImage, max_width: u32) -> image::DynamicImage {
    let (w, h) = img.dimensions();
    if w > max_width {
        let new_h = (max_width as f64 / w as f64 * h as f64) as u32;
        img.resize_exact(max_width, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    }
}

fn encode_webp(
    img: &image::DynamicImage,
    path: &Path,
    quality: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    let encoder = webp::Encoder::from_image(img).map_err(|e| format!("webp encode: {e}"))?;
    let data = encoder.encode(quality);
    fs::write(path, &*data)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Animated GIF -> animated WebP
// ---------------------------------------------------------------------------

fn is_animated_gif(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if ext.as_deref() != Some("gif") {
        return false;
    }
    // Check if GIF has more than one frame
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut decoder = gif::DecodeOptions::new();
    decoder.set_color_output(gif::ColorOutput::RGBA);
    let Ok(mut reader) = decoder.read_info(file) else {
        return false;
    };
    // Read first frame
    if reader.read_next_frame().ok().flatten().is_none() {
        return false;
    }
    // If there's a second frame, it's animated
    reader.read_next_frame().ok().flatten().is_some()
}

fn optimize_animated_gif(
    path: &Path,
    max_width: u32,
    quality: f32,
) -> Result<(u64, u64, PathBuf), Box<dyn std::error::Error>> {
    let before = fs::metadata(path)?.len();

    let file = fs::File::open(path)?;
    let mut decoder = gif::DecodeOptions::new();
    decoder.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = decoder.read_info(file)?;

    let src_width = reader.width() as u32;
    let src_height = reader.height() as u32;

    // Determine output dimensions
    let (out_w, out_h) = if src_width > max_width {
        let scale = max_width as f64 / src_width as f64;
        (max_width, (src_height as f64 * scale) as u32)
    } else {
        (src_width, src_height)
    };

    let config = webp_animation::EncodingConfig::new_lossy(quality);
    let mut options = webp_animation::EncoderOptions::default();
    options.encoding_config = Some(config);
    options.minimize_size = true;

    let mut encoder = webp_animation::Encoder::new_with_options((out_w, out_h), options)?;

    let mut timestamp_ms: i32 = 0;
    let needs_resize = src_width > max_width;

    while let Some(frame) = reader.read_next_frame()? {
        let delay_ms = frame.delay as i32 * 10; // GIF delay is in centiseconds

        let frame_rgba = if needs_resize {
            let img = image::RgbaImage::from_raw(src_width, src_height, frame.buffer.to_vec())
                .ok_or("invalid frame dimensions")?;
            let resized = image::imageops::resize(
                &img,
                out_w,
                out_h,
                image::imageops::FilterType::Lanczos3,
            );
            resized.into_raw()
        } else {
            frame.buffer.to_vec()
        };

        encoder.add_frame(&frame_rgba, timestamp_ms)?;
        timestamp_ms += delay_ms.max(20); // Floor at 20ms (50fps) to avoid 0-delay GIFs
    }

    let webp_data = encoder.finalize(timestamp_ms)?;

    let out_path = path.with_extension("webp");
    fs::write(&out_path, &webp_data)?;

    let after = fs::metadata(&out_path)?.len();
    Ok((before, after, out_path))
}

// ---------------------------------------------------------------------------
// File collection
// ---------------------------------------------------------------------------

fn collect_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if is_convertible(&p) {
                        files.push(p);
                    }
                }
            }
        } else if is_convertible(path) {
            files.push(path.clone());
        }
    }
    files.sort();
    files
}

fn is_convertible(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "tif")
    )
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn print_usage() {
    eprintln!("img-optim — Convert and resize images to WebP for blog posts");
    eprintln!();
    eprintln!("Usage: img-optim [OPTIONS] <PATH>...");
    eprintln!();
    eprintln!("Arguments:");
    eprintln!("  <PATH>  Image file or directory containing images");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -w, --max-width <PX>   Max width in pixels (default: 1200)");
    eprintln!("  -q, --quality <0-100>   WebP quality (default: 80)");
    eprintln!("  -t, --thumbnails        Also generate *-thumb.webp (600px, q75)");
    eprintln!("  -h, --help              Show this help");
    eprintln!();
    eprintln!("Supported formats:");
    eprintln!("  Static:   jpg, jpeg, png, bmp, tiff -> WebP");
    eprintln!("  Animated: gif (multi-frame) -> animated WebP");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  img-optim content/blog/my-post/");
    eprintln!("  img-optim -t content/blog/my-post/hero.jpg");
    eprintln!("  img-optim content/blog/my-post/demo.gif");
    eprintln!("  img-optim -q 90 -w 1600 photo.png");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_match_what_the_help_promises() {
        let args = parse_args(&argv(&["hero.jpg"])).unwrap();
        assert_eq!(args.max_width, 1200);
        assert_eq!(args.quality, 80.0);
        assert!(!args.thumbnails);
        assert_eq!(args.paths, vec![PathBuf::from("hero.jpg")]);
    }

    #[test]
    fn flags_and_paths_can_be_interleaved() {
        let args = parse_args(&argv(&["-t", "a.jpg", "-q", "90", "b.png", "--max-width", "1600"]))
            .unwrap();
        assert!(args.thumbnails);
        assert_eq!(args.quality, 90.0);
        assert_eq!(args.max_width, 1600);
        assert_eq!(args.paths, vec![PathBuf::from("a.jpg"), PathBuf::from("b.png")]);
    }

    /// `img-optim -q` used to index past the end of argv and panic.
    #[test]
    fn a_flag_without_its_value_is_an_error() {
        assert!(parse_args(&argv(&["-q"])).is_err());
        assert!(parse_args(&argv(&["hero.jpg", "--max-width"])).is_err());
    }

    #[test]
    fn a_bad_value_is_an_error() {
        assert!(parse_args(&argv(&["-q", "high"])).is_err());
        assert!(parse_args(&argv(&["-w", "wide"])).is_err());
    }

    /// libwebp reads quality as 0-100 and clamps silently, so a typo like `-q 900`
    /// would encode at 100 and look like it worked.
    #[test]
    fn quality_is_bounded() {
        assert!(parse_args(&argv(&["-q", "900"])).is_err());
        assert!(parse_args(&argv(&["-q", "-1"])).is_err());
        assert_eq!(parse_args(&argv(&["-q", "0"])).unwrap().quality, 0.0);
        assert_eq!(parse_args(&argv(&["-q", "100"])).unwrap().quality, 100.0);
    }

    #[test]
    fn unknown_flags_are_refused() {
        assert!(parse_args(&argv(&["--lossless", "a.jpg"])).is_err());
    }

    #[test]
    fn help_wins_before_any_path_is_needed() {
        assert!(parse_args(&argv(&["--help"])).unwrap().help);
        assert!(parse_args(&argv(&["-h"])).unwrap().help);
    }

    #[test]
    fn only_raster_sources_convert() {
        for name in ["a.jpg", "a.JPEG", "a.png", "a.gif", "a.bmp", "a.tiff", "a.tif"] {
            assert!(is_convertible(Path::new(name)), "{name} should convert");
        }
        // Already converted, or not a raster image at all.
        for name in ["a.webp", "a.svg", "index.md", "a", "a.WEBP"] {
            assert!(!is_convertible(Path::new(name)), "{name} should not convert");
        }
    }

    /// The template derives the thumbnail path from `featured_image` with a string
    /// replace, so `hero.jpg` has to produce exactly `hero.webp` and `hero-thumb.webp`.
    #[test]
    fn output_names_follow_the_template_convention() {
        let src = Path::new("content/blog/my-post/hero.jpg");
        assert_eq!(
            src.with_extension("webp"),
            Path::new("content/blog/my-post/hero.webp")
        );
        let stem = src.file_stem().unwrap().to_string_lossy();
        assert_eq!(
            src.with_file_name(format!("{stem}{THUMB_SUFFIX}.webp")),
            Path::new("content/blog/my-post/hero-thumb.webp")
        );
    }

    /// An image already inside the cap is left alone rather than resampled, which would
    /// cost quality for no bytes saved.
    #[test]
    fn resize_only_shrinks() {
        let wide = image::DynamicImage::new_rgba8(2400, 1200);
        let out = resize_to_width(wide, 1200);
        assert_eq!(out.dimensions(), (1200, 600));

        let narrow = image::DynamicImage::new_rgba8(800, 400);
        let out = resize_to_width(narrow, 1200);
        assert_eq!(out.dimensions(), (800, 400));

        let exact = image::DynamicImage::new_rgba8(1200, 300);
        let out = resize_to_width(exact, 1200);
        assert_eq!(out.dimensions(), (1200, 300));
    }

    #[test]
    fn sizes_read_in_the_unit_that_fits() {
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(1024), "1.0 KB");
        assert_eq!(fmt_size(48_845), "47.7 KB");
        assert_eq!(fmt_size(4 * 1024 * 1024), "4.0 MB");
    }

    /// A directory argument picks up its convertible files and nothing else, sorted so
    /// the run reads the same twice.
    #[test]
    fn a_directory_yields_its_convertible_files() {
        let dir = std::env::temp_dir().join("img-optim-collect-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for name in ["b.png", "a.jpg", "hero.webp", "index.md", "chart.svg"] {
            fs::write(dir.join(name), b"x").unwrap();
        }

        let files = collect_files(&[dir.clone()]);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.jpg", "b.png"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    /// A named file that is not a raster source is skipped rather than erroring, so
    /// `img-optim post-dir/*` does the right thing with a mixed glob.
    #[test]
    fn unconvertible_paths_are_skipped() {
        assert!(collect_files(&[PathBuf::from("index.md")]).is_empty());
        assert!(collect_files(&[PathBuf::from("hero.webp")]).is_empty());
    }

    /// Collection is by extension alone. A missing file is still collected, and reported
    /// per-file when it fails to open -- which is the right place to say so, since the
    /// path came from the command line.
    #[test]
    fn a_missing_source_is_collected_and_fails_later() {
        let missing = PathBuf::from("does-not-exist.jpg");
        assert_eq!(collect_files(&[missing.clone()]), vec![missing.clone()]);
        assert!(optimize(&missing, 1200, 80.0).is_err());
    }
}
