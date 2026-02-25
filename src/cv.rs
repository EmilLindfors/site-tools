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

    let output_dir = root.join("static/pdf");
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create {}: {e}", output_dir.display()))?;

    let output_path = output_dir.join("cv.pdf");

    println!("Compiling cv.typ -> {}", output_path.display());

    let status = Command::new("typst")
        .arg("compile")
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
