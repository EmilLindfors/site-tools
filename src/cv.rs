use std::fs;
use std::process::Command;

use crate::util;

/// Build the CV PDF from cv.typ at the project root.
pub fn build() -> Result<(), String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("Failed to get cwd: {e}"))?;
    let root = util::find_project_root(&cwd)?;

    let cv_src = root.join("cv.typ");
    if !cv_src.exists() {
        return Err(format!("cv.typ not found at {}", root.display()));
    }

    // static/cv.pdf, not static/pdf/cv.pdf: the sidebar links /cv.pdf, and nothing
    // serves /pdf/cv.pdf. Writing there just left an unreferenced stale copy behind.
    let output_path = root.join("static/cv.pdf");
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    println!("Compiling cv.typ -> {}", output_path.display());

    // cv.typ asks for Libertinus Serif; without this it silently renders in fallback
    // fonts. --font-path is recursive, so one path covers everything fetch-fonts.sh
    // installs.
    let font_path = root.join("fonts");

    let status = Command::new("typst")
        .arg("compile")
        .arg("--font-path")
        .arg(&font_path)
        .arg(&cv_src)
        .arg(&output_path)
        .status()
        .map_err(|e| format!("Failed to run typst: {e}"))?;

    if !status.success() {
        return Err(format!("typst compile failed with status {status}"));
    }

    println!("Generated: {}", output_path.display());
    Ok(())
}
