use image::GenericImageView;
use std::path::{Path, PathBuf};
use std::{env, fs, process};

const THUMB_WIDTH: u32 = 600;
const THUMB_QUALITY: f32 = 75.0;
const THUMB_SUFFIX: &str = "-thumb";

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut paths = Vec::new();
    let mut max_width: u32 = 1200;
    let mut quality: f32 = 80.0;
    let mut thumbnails = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--max-width" => {
                i += 1;
                max_width = args[i].parse().expect("invalid max-width");
            }
            "-q" | "--quality" => {
                i += 1;
                quality = args[i].parse().expect("invalid quality (0-100)");
            }
            "-t" | "--thumbnails" => {
                thumbnails = true;
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            arg if arg.starts_with('-') => {
                eprintln!("Unknown flag: {arg}");
                process::exit(1);
            }
            _ => paths.push(PathBuf::from(&args[i])),
        }
        i += 1;
    }

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
