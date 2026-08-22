use crate::models::{ChangedFile, RepoInspection};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(repo).args(args).output().await.context("launch git")?;
    if !output.status.success() { return Err(anyhow!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim())); }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

pub async fn inspect_repository(path: &Path) -> Result<RepoInspection> {
    let canonical = path.canonicalize().context("repository path does not exist")?;
    let inside = git(&canonical, &["rev-parse", "--is-inside-work-tree"]).await?;
    if inside != "true" { return Err(anyhow!("selected folder is not a Git repository")); }
    let root = PathBuf::from(git(&canonical, &["rev-parse", "--show-toplevel"]).await?);
    let branch = git(&root, &["branch", "--show-current"]).await.unwrap_or_else(|_| "detached".into());
    let head_sha = git(&root, &["rev-parse", "HEAD"]).await?;
    let dirty = !git(&root, &["status", "--porcelain"]).await?.is_empty();
    let (language, build_system, test) = detect_project(&root);
    Ok(RepoInspection { path: root.to_string_lossy().into(), branch, head_sha, dirty, language, build_system, suggested_test_command: test })
}

fn detect_project(root: &Path) -> (String, String, String) {
    let candidates = [
        ("Cargo.toml", "Rust", "Cargo", "cargo test"),
        ("package.json", "TypeScript / JavaScript", "Node", "npm test"),
        ("pyproject.toml", "Python", "Python", "pytest -q"),
        ("requirements.txt", "Python", "Python", "pytest -q"),
        ("go.mod", "Go", "Go modules", "go test ./..."),
        ("CMakeLists.txt", "C / C++", "CMake", "cmake --build build && ctest --test-dir build"),
        ("pom.xml", "Java", "Maven", "mvn test"),
        ("build.gradle", "Java / Kotlin", "Gradle", "./gradlew test"),
    ];
    for (file, language, system, test) in candidates { if root.join(file).exists() { return (language.into(), system.into(), test.into()); } }
    ("Unknown".into(), "Custom".into(), "".into())
}

pub async fn create_worktree(repo: &Path, root: &Path, run_id: &str, base_sha: &str) -> Result<(PathBuf, String)> {
    let short = &run_id[..run_id.len().min(8)];
    let branch = format!("duet/run-{short}");
    let worktree = root.join(run_id).join("implementation");
    if let Some(parent) = worktree.parent() { tokio::fs::create_dir_all(parent).await?; }
    let path = worktree.to_string_lossy().to_string();
    git(repo, &["worktree", "add", "-b", &branch, &path, base_sha]).await?;
    Ok((worktree, branch))
}

pub async fn changed_files(worktree: &Path) -> Result<Vec<ChangedFile>> {
    let _ = git(worktree, &["add", "-N", "."]).await;
    let status = git(worktree, &["status", "--porcelain"]).await?;
    let numstat = git(worktree, &["diff", "--numstat", "HEAD"]).await.unwrap_or_default();
    let mut stats = std::collections::HashMap::new();
    for line in numstat.lines() {
        let parts: Vec<_> = line.splitn(3, '\t').collect();
        if parts.len() == 3 { stats.insert(parts[2].to_string(), (parts[0].parse().unwrap_or(0), parts[1].parse().unwrap_or(0))); }
    }
    Ok(status.lines().filter_map(|line| {
        if line.len() < 4 { return None; }
        let code = line[..2].trim(); let path = line[3..].split(" -> ").last().unwrap_or("").to_string();
        let (additions, deletions) = stats.get(&path).copied().unwrap_or((0,0));
        Some(ChangedFile { path, status: match code.chars().next().unwrap_or('M') {'A'|'?' => "added", 'D' => "deleted", _ => "modified"}.into(), additions, deletions })
    }).collect())
}

pub async fn diff(worktree: &Path) -> Result<String> {
    let _ = git(worktree, &["add", "-N", "."]).await;
    let mut patch=git(worktree, &["diff", "--no-ext-diff", "--binary", "HEAD"]).await?;
    if !patch.is_empty() && !patch.ends_with('\n'){patch.push('\n');}
    Ok(patch)
}

pub async fn apply_worktree_changes(repo: &Path, worktree: &Path, expected_head: &str) -> Result<()> {
    let current = git(repo, &["rev-parse", "HEAD"]).await?;
    if current != expected_head { return Err(anyhow!("target repository changed since this run started; apply aborted")); }
    if !git(repo, &["status", "--porcelain"]).await?.is_empty() { return Err(anyhow!("target working tree is dirty; commit or stash its changes before applying")); }
    let patch = diff(worktree).await?;
    if patch.is_empty() { return Err(anyhow!("run contains no changes")); }
    let mut child = Command::new("git").arg("-C").arg(repo).args(["apply", "--3way", "-"]).stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn()?;
    use tokio::io::AsyncWriteExt;
    child.stdin.take().unwrap().write_all(patch.as_bytes()).await?;
    let output = child.wait_with_output().await?;
    if !output.status.success() { return Err(anyhow!("could not apply changes: {}", String::from_utf8_lossy(&output.stderr))); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_rust_projects() {
        let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project(dir.path()).0, "Rust");
    }

    #[tokio::test]
    async fn isolates_and_explicitly_applies_a_patch() {
        let source=tempfile::tempdir().unwrap();
        git(source.path(),&["init"]).await.unwrap();git(source.path(),&["config","user.email","duet@example.test"]).await.unwrap();git(source.path(),&["config","user.name","Duet Test"]).await.unwrap();
        std::fs::write(source.path().join("README.md"),"base\n").unwrap();git(source.path(),&["add","README.md"]).await.unwrap();git(source.path(),&["commit","-m","base"]).await.unwrap();
        let base=git(source.path(),&["rev-parse","HEAD"]).await.unwrap();let managed=tempfile::tempdir().unwrap();let (worktree,_)=create_worktree(source.path(),managed.path(),"12345678-test",&base).await.unwrap();
        std::fs::write(worktree.join("feature.txt"),"isolated\n").unwrap();
        assert!(!source.path().join("feature.txt").exists());assert!(changed_files(&worktree).await.unwrap().iter().any(|f|f.path=="feature.txt"));
        apply_worktree_changes(source.path(),&worktree,&base).await.unwrap();assert_eq!(std::fs::read_to_string(source.path().join("feature.txt")).unwrap(),"isolated\n");
    }
}
