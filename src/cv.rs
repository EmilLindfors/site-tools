use std::fs;
use std::path::Path;
use std::process::Command;

use crate::util;

/// A stable timestamp for a source file: its last commit, falling back to its mtime.
///
/// Typst stamps a CreationDate into the PDF, so without pinning this every build
/// rewrites cv.pdf even when cv.typ has not changed.
fn stable_epoch(root: &Path, file: &Path) -> Option<i64> {
    let out = Command::new("git")
        .current_dir(root)
        .args(["log", "-1", "--format=%ct", "--"])
        .arg(file)
        .output()
        .ok()?;

    if out.status.success() {
        if let Some(t) = String::from_utf8_lossy(&out.stdout).trim().parse::<i64>().ok() {
            return Some(t);
        }
    }

    // Not committed yet (or no git): mtime still only changes when the file does.
    fs::metadata(file)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

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

    let mut cmd = Command::new("typst");
    cmd.arg("compile")
        .arg("--font-path")
        .arg(&font_path)
        .arg(&cv_src)
        .arg(&output_path);

    if let Some(epoch) = stable_epoch(&root, &cv_src) {
        cmd.env("SOURCE_DATE_EPOCH", epoch.to_string());
    }

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to run typst: {e}"))?;

    if !status.success() {
        return Err(format!("typst compile failed with status {status}"));
    }

    println!("Generated: {}", output_path.display());
    Ok(())
}
