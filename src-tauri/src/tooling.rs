use std::path::PathBuf;

/// Resolve a user-installed developer tool even when a macOS Finder launch has a minimal PATH.
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
        PathBuf::from("/usr/bin").join(name),
    ];
    if let Some(home) = dirs::home_dir() {
        for directory in [
            ".local/bin",
            ".npm-global/bin",
            ".cargo/bin",
            ".volta/bin",
            ".local/share/pnpm",
        ] {
            candidates.push(home.join(directory).join(name));
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_system_git() {
        assert!(resolve_binary("git").is_some())
    }
    #[test]
    fn rejects_unknown_tools() {
        assert!(resolve_binary("duet-tool-that-does-not-exist").is_none())
    }
}
