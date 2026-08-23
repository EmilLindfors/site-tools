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

/// Find the .env file by walking up from the given directory.
pub fn find_env_file(from: &Path) -> Result<PathBuf, String> {
    let mut dir = from.to_path_buf();
    loop {
        let candidate = dir.join(".env");
        if candidate.exists() {
            return Ok(candidate);
        }
        if !dir.pop() {
            return Err("Could not find .env file".to_string());
        }
    }
}

/// Read a KEY=value from a .env file.
pub fn read_env_var(env_path: &Path, key: &str) -> Result<String, String> {
    let content = fs::read_to_string(env_path)
        .map_err(|e| format!("Failed to read {}: {e}", env_path.display()))?;

    let prefix = format!("{key}=");
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&prefix) {
            return Ok(trimmed[prefix.len()..].to_string());
        }
    }

    Err(format!("{key} not found in {}", env_path.display()))
}

/// A setting from the real environment, falling back to the project `.env`.
///
/// The environment wins so a one-off run can override without editing the file.
pub fn setting(root: &Path, key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key) {
        if !value.is_empty() {
            return Some(value);
        }
    }

    let value = find_env_file(root)
        .and_then(|path| read_env_var(&path, key))
        .ok()?;

    (!value.is_empty()).then_some(value)
}
