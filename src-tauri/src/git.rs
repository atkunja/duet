use crate::{
    models::{ChangedFile, RepoInspection},
    process::{run_process, OutputCallback, ProcessRequest},
    tooling::resolve_binary,
};
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let binary = resolve_binary("git").context("Git executable not found")?;
    let mut process_args = vec!["-C".into(), repo.to_string_lossy().into()];
    process_args.extend(args.iter().map(|value| (*value).into()));
    let output = run_process(
        ProcessRequest {
            program: binary.to_string_lossy().into(),
            args: process_args,
            cwd: repo.into(),
            timeout: Duration::from_secs(120),
            env: vec![],
            stdin: None,
            capture_limit: 64 * 1024 * 1024,
            fail_on_output_limit: true,
        },
        CancellationToken::new(),
        Arc::new(|_: &str, _: &str| {}) as OutputCallback,
    )
    .await
    .context("launch git")?;
    if !output.success {
        return Err(anyhow!("git {}: {}", args.join(" "), output.stderr.trim()));
    }
    Ok(output.stdout.trim_end().to_string())
}

#[cfg(test)]
pub mod tests_support {
    use super::*;
    pub async fn init_repo(path: &Path) {
        git(path, &["init"]).await.unwrap();
        git(path, &["config", "user.email", "duet@example.test"])
            .await
            .unwrap();
        git(path, &["config", "user.name", "Duet Test"])
            .await
            .unwrap();
        std::fs::write(path.join("README.md"), "base\n").unwrap();
        git(path, &["add", "README.md"]).await.unwrap();
        git(path, &["commit", "-m", "base"]).await.unwrap();
    }
}

pub async fn inspect_repository(path: &Path) -> Result<RepoInspection> {
    let canonical = path
        .canonicalize()
        .context("repository path does not exist")?;
    let inside = git(&canonical, &["rev-parse", "--is-inside-work-tree"]).await?;
    if inside != "true" {
        return Err(anyhow!("selected folder is not a Git repository"));
    }
    let root = PathBuf::from(git(&canonical, &["rev-parse", "--show-toplevel"]).await?);
    let branch = git(&root, &["branch", "--show-current"])
        .await
        .unwrap_or_else(|_| "detached".into());
    let head_sha = git(&root, &["rev-parse", "HEAD"]).await?;
    let dirty = !git(&root, &["status", "--porcelain"]).await?.is_empty();
    let (language, build_system, test) = detect_project(&root);
    Ok(RepoInspection {
        path: root.to_string_lossy().into(),
        branch,
        head_sha,
        dirty,
        language,
        build_system,
        suggested_test_command: test,
    })
}

fn detect_project(root: &Path) -> (String, String, String) {
    let candidates = [
        ("Cargo.toml", "Rust", "Cargo", "cargo test"),
        (
            "package.json",
            "TypeScript / JavaScript",
            "Node",
            "npm test",
        ),
        ("pyproject.toml", "Python", "Python", "pytest -q"),
        ("requirements.txt", "Python", "Python", "pytest -q"),
        ("go.mod", "Go", "Go modules", "go test ./..."),
        (
            "CMakeLists.txt",
            "C / C++",
            "CMake",
            "cmake --build build && ctest --test-dir build",
        ),
        ("pom.xml", "Java", "Maven", "mvn test"),
        ("build.gradle", "Java / Kotlin", "Gradle", "./gradlew test"),
    ];
    for (file, language, system, test) in candidates {
        if root.join(file).exists() {
            return (language.into(), system.into(), test.into());
        }
    }
    ("Unknown".into(), "Custom".into(), "".into())
}

pub async fn create_worktree(
    repo: &Path,
    root: &Path,
    run_id: &str,
    base_sha: &str,
) -> Result<(PathBuf, String)> {
    let short = &run_id[..run_id.len().min(8)];
    let branch = format!("duet/run-{short}");
    let worktree = root.join(run_id).join("implementation");
    if let Some(parent) = worktree.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let path = worktree.to_string_lossy().to_string();
    git(repo, &["worktree", "add", "-b", &branch, &path, base_sha]).await?;
    Ok((worktree, branch))
}

pub async fn changed_files(worktree: &Path, base_sha: &str) -> Result<Vec<ChangedFile>> {
    let _ = git(worktree, &["add", "-N", "."]).await;
    let status = git(
        worktree,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-status",
            "--no-renames",
            base_sha,
        ],
    )
    .await?;
    let numstat = git(
        worktree,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--numstat",
            "--no-renames",
            base_sha,
        ],
    )
    .await
    .unwrap_or_default();
    let mut stats = std::collections::HashMap::new();
    for line in numstat.lines() {
        let parts: Vec<_> = line.splitn(3, '\t').collect();
        if parts.len() == 3 {
            stats.insert(
                parts[2].to_string(),
                (parts[0].parse().unwrap_or(0), parts[1].parse().unwrap_or(0)),
            );
        }
    }
    Ok(status
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let code = fields.next()?;
            let path = fields.next_back()?.to_string();
            let (additions, deletions) = stats.get(&path).copied().unwrap_or((0, 0));
            Some(ChangedFile {
                path,
                status: match code.chars().next().unwrap_or('M') {
                    'A' | '?' => "added",
                    'D' => "deleted",
                    _ => "modified",
                }
                .into(),
                additions,
                deletions,
            })
        })
        .collect())
}

pub async fn diff(worktree: &Path, base_sha: &str) -> Result<String> {
    let _ = git(worktree, &["add", "-N", "."]).await;
    let mut patch = git(
        worktree,
        &[
            "diff",
            "--no-ext-diff",
            "--binary",
            "--no-renames",
            base_sha,
        ],
    )
    .await?;
    if !patch.is_empty() && !patch.ends_with('\n') {
        patch.push('\n');
    }
    Ok(patch)
}

pub async fn patch_sha256(worktree: &Path, base_sha: &str) -> Result<String> {
    let patch = diff(worktree, base_sha).await?;
    Ok(patch_content_sha256(&patch))
}

pub fn patch_content_sha256(patch: &str) -> String {
    format!("{:x}", Sha256::digest(patch.as_bytes()))
}

pub async fn apply_worktree_changes(
    repo: &Path,
    worktree: &Path,
    expected_head: &str,
    verified_patch_sha256: &str,
) -> Result<()> {
    let current = git(repo, &["rev-parse", "HEAD"]).await?;
    if current != expected_head {
        return Err(anyhow!(
            "target repository changed since this run started; apply aborted"
        ));
    }
    if !git(repo, &["status", "--porcelain"]).await?.is_empty() {
        return Err(anyhow!(
            "target working tree is dirty; commit or stash its changes before applying"
        ));
    }
    let patch = diff(worktree, expected_head).await?;
    if patch.is_empty() {
        return Err(anyhow!("run contains no changes"));
    }
    let current_patch_sha256 = format!("{:x}", Sha256::digest(patch.as_bytes()));
    if current_patch_sha256 != verified_patch_sha256 {
        return Err(anyhow!(
            "the isolated worktree changed after verification; apply aborted"
        ));
    }
    apply_patch(repo, &patch, true).await?;
    apply_patch(repo, &patch, false).await?;
    Ok(())
}

async fn apply_patch(repo: &Path, patch: &str, check_only: bool) -> Result<()> {
    let binary = resolve_binary("git").context("Git executable not found")?;
    let mut args = vec![
        "-C".into(),
        repo.to_string_lossy().into(),
        "apply".into(),
        "--binary".into(),
    ];
    if check_only {
        args.push("--check".into())
    }
    args.push("-".into());
    let output = run_process(
        ProcessRequest {
            program: binary.to_string_lossy().into(),
            args,
            cwd: repo.into(),
            timeout: Duration::from_secs(30),
            env: vec![],
            stdin: Some(patch.into()),
            capture_limit: 1_000_000,
            fail_on_output_limit: false,
        },
        CancellationToken::new(),
        Arc::new(|_: &str, _: &str| {}) as OutputCallback,
    )
    .await?;
    if !output.success {
        return Err(anyhow!(
            "could not {} changes: {}",
            if check_only { "validate" } else { "apply" },
            output.stderr
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_rust_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project(dir.path()).0, "Rust");
    }

    #[tokio::test]
    async fn isolates_and_explicitly_applies_a_patch() {
        let source = tempfile::tempdir().unwrap();
        tests_support::init_repo(source.path()).await;
        let base = git(source.path(), &["rev-parse", "HEAD"]).await.unwrap();
        let managed = tempfile::tempdir().unwrap();
        let (worktree, _) = create_worktree(source.path(), managed.path(), "12345678-test", &base)
            .await
            .unwrap();
        std::fs::write(worktree.join("feature.txt"), "isolated\n").unwrap();
        assert!(!source.path().join("feature.txt").exists());
        assert!(changed_files(&worktree, &base)
            .await
            .unwrap()
            .iter()
            .any(|f| f.path == "feature.txt"));
        let digest = patch_sha256(&worktree, &base).await.unwrap();
        apply_worktree_changes(source.path(), &worktree, &base, &digest)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(source.path().join("feature.txt")).unwrap(),
            "isolated\n"
        );
    }
    #[tokio::test]
    async fn includes_agent_commits_in_review_and_apply() {
        let source = tempfile::tempdir().unwrap();
        tests_support::init_repo(source.path()).await;
        let base = git(source.path(), &["rev-parse", "HEAD"]).await.unwrap();
        let managed = tempfile::tempdir().unwrap();
        let (worktree, _) = create_worktree(source.path(), managed.path(), "commit-test", &base)
            .await
            .unwrap();
        std::fs::write(worktree.join("committed.txt"), "agent commit\n").unwrap();
        git(&worktree, &["add", "committed.txt"]).await.unwrap();
        git(&worktree, &["commit", "-m", "agent change"])
            .await
            .unwrap();
        assert!(diff(&worktree, &base)
            .await
            .unwrap()
            .contains("committed.txt"));
        let digest = patch_sha256(&worktree, &base).await.unwrap();
        apply_worktree_changes(source.path(), &worktree, &base, &digest)
            .await
            .unwrap();
        assert!(source.path().join("committed.txt").exists());
    }

    #[tokio::test]
    async fn refuses_worktree_changes_made_after_verification() {
        let source = tempfile::tempdir().unwrap();
        tests_support::init_repo(source.path()).await;
        let base = git(source.path(), &["rev-parse", "HEAD"]).await.unwrap();
        let managed = tempfile::tempdir().unwrap();
        let (worktree, _) = create_worktree(source.path(), managed.path(), "late-change", &base)
            .await
            .unwrap();
        std::fs::write(worktree.join("verified.txt"), "reviewed\n").unwrap();
        let digest = patch_sha256(&worktree, &base).await.unwrap();
        std::fs::write(worktree.join("verified.txt"), "changed later\n").unwrap();

        let error = apply_worktree_changes(source.path(), &worktree, &base, &digest)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed after verification"));
        assert!(!source.path().join("verified.txt").exists());
    }

    #[tokio::test]
    async fn preserves_and_applies_patches_larger_than_the_log_limit() {
        let source = tempfile::tempdir().unwrap();
        tests_support::init_repo(source.path()).await;
        let base = git(source.path(), &["rev-parse", "HEAD"]).await.unwrap();
        let managed = tempfile::tempdir().unwrap();
        let (worktree, _) = create_worktree(source.path(), managed.path(), "large-patch", &base)
            .await
            .unwrap();
        let content = "large exact patch line\n".repeat(60_000);
        std::fs::write(worktree.join("large.txt"), &content).unwrap();

        let patch = diff(&worktree, &base).await.unwrap();
        assert!(patch.len() > 1_000_000);
        assert!(!patch.contains("Duet truncated"));
        let digest = patch_sha256(&worktree, &base).await.unwrap();
        apply_worktree_changes(source.path(), &worktree, &base, &digest)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(source.path().join("large.txt")).unwrap(),
            content
        );
    }
}
