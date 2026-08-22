use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

const SYSTEM_PATHS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Build a deterministic developer-tool PATH for GUI launches.
///
/// Finder/LaunchServices apps commonly inherit only the system directories.
/// Prepending the selected program's directory is important for npm-installed
/// wrappers such as `#!/usr/bin/env node`.
pub fn path_for_program(program: impl AsRef<Path>) -> OsString {
    join_tool_paths(developer_tool_paths(
        Some(program.as_ref()),
        dirs::home_dir().as_deref(),
        std::env::var_os("PATH").as_deref(),
    ))
}

fn developer_tool_paths(
    program: Option<&Path>,
    home: Option<&Path>,
    inherited: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |path: PathBuf| {
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    };
    if let Some(parent) = program
        .and_then(Path::parent)
        .filter(|path| !path.as_os_str().is_empty())
    {
        add(parent.to_path_buf());
    }
    if let Some(current) = inherited {
        for path in std::env::split_paths(current).filter(|path| !is_system_path(path)) {
            add(path);
        }
    }
    if let Some(home) = home {
        let mut node_bins = nvm_node_bins(home);
        for path in node_bins.drain(..) {
            add(path);
        }
        for directory in [
            ".volta/bin",
            ".asdf/shims",
            ".local/share/mise/shims",
            ".local/bin",
            ".npm-global/bin",
            ".cargo/bin",
            ".local/share/pnpm",
        ] {
            add(home.join(directory));
        }
    }
    for path in ["/opt/homebrew/bin", "/usr/local/bin"] {
        add(PathBuf::from(path));
    }
    for path in SYSTEM_PATHS {
        add(PathBuf::from(path));
    }
    paths
}

fn join_tool_paths(paths: Vec<PathBuf>) -> OsString {
    std::env::join_paths(paths).unwrap_or_else(|_| {
        OsString::from("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
    })
}

fn is_system_path(path: &Path) -> bool {
    SYSTEM_PATHS.iter().any(|system| path == Path::new(system))
}

fn nvm_node_bins(home: &Path) -> Vec<PathBuf> {
    let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) else {
        return Vec::new();
    };
    let mut bins: Vec<_> = versions
        .flatten()
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.is_dir())
        .collect();
    bins.sort_by_key(|path| {
        path.parent()
            .and_then(Path::file_name)
            .map(node_version_key)
            .unwrap_or_default()
    });
    bins.reverse();
    bins
}

fn node_version_key(value: &OsStr) -> Vec<u64> {
    value
        .to_string_lossy()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// Resolve a user-installed developer tool even when a macOS Finder launch has a minimal PATH.
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    resolve_binary_in(
        name,
        dirs::home_dir().as_deref(),
        std::env::var_os("PATH").as_deref(),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    )
}

fn resolve_binary_in(
    name: &str,
    home: Option<&Path>,
    inherited: Option<&OsStr>,
    cwd: &Path,
) -> Option<PathBuf> {
    let search_path = join_tool_paths(developer_tool_paths(None, home, inherited));
    which::which_in(name, Some(search_path), cwd).ok()
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
    #[test]
    fn prepends_the_selected_program_directory_to_gui_paths() {
        let path = path_for_program("/custom/tooling/codex");
        assert_eq!(
            std::env::split_paths(&path).next(),
            Some(PathBuf::from("/custom/tooling"))
        );
        assert!(std::env::split_paths(&path).any(|entry| entry == Path::new("/usr/bin")));
    }

    #[cfg(unix)]
    #[test]
    fn discovers_nvm_tools_with_a_minimal_finder_path() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let home = tempfile::tempdir().unwrap();
        let newest = home.path().join(".nvm/versions/node/v20.12.0/bin");
        let older = home.path().join(".nvm/versions/node/v9.9.0/bin");
        fs::create_dir_all(&newest).unwrap();
        fs::create_dir_all(&older).unwrap();
        let codex = newest.join("codex");
        fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&codex, permissions).unwrap();
        let finder_path = OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");

        assert_eq!(
            resolve_binary_in("codex", Some(home.path()), Some(finder_path), home.path()),
            Some(codex)
        );
        let paths = developer_tool_paths(None, Some(home.path()), Some(finder_path));
        let newest_position = paths.iter().position(|path| path == &newest).unwrap();
        let older_position = paths.iter().position(|path| path == &older).unwrap();
        let system_position = paths
            .iter()
            .position(|path| path == Path::new("/usr/bin"))
            .unwrap();
        assert!(newest_position < older_position);
        assert!(older_position < system_position);
    }
}
