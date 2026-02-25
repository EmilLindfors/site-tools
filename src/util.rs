use std::fs;
use std::path::{Path, PathBuf};

/// Walk up from a path to find the project root (contains zola.toml).
pub fn find_project_root(from: &Path) -> Result<PathBuf, String> {
    let abs = fs::canonicalize(from)
        .map_err(|e| format!("Failed to resolve {}: {e}", from.display()))?;

    let mut dir = if abs.is_file() {
        abs.parent().unwrap().to_path_buf()
    } else {
        abs
    };

    loop {
        if dir.join("zola.toml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("Could not find project root (no zola.toml found)".to_string());
        }
    }
}
