//! Threads that work in their own git worktree, branched from the freshly
//! fetched default branch of `origin` (usually `main`), so they never touch
//! the checkout the user has open.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

/// Where a project's thread worktrees live: the app's data folder, never
/// inside the repository.
pub fn root(project: &str) -> PathBuf {
    all_roots().join(project)
}

fn all_roots() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/Sense/worktrees")
}

/// Whether `path` is where this app puts thread worktrees.
pub fn is_thread_worktree(path: &Path) -> bool {
    path.starts_with(all_roots()) && path != all_roots()
}

/// Creates a worktree of `repo` at `path` on a new branch `branch`, starting
/// from origin's freshly fetched default branch. Blocks on git, including
/// the network fetch: call it off the main thread.
pub fn create(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    let main = default_branch(repo)?;
    git(repo, &["fetch", "--quiet", "origin", &main])
        .with_context(|| format!("fetch origin/{main}"))?;
    let start = format!("origin/{main}");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = path.to_str().context("worktree path isn't UTF-8")?;
    git(
        repo,
        &[
            // The repository's hooks are for a person switching branches: a
            // post-checkout hook can take seconds, or run tools that open
            // windows or ask for logins. An agent's checkout needs none.
            "-c",
            "core.hooksPath=/dev/null",
            // Writes files on every core: a large checkout is mostly that.
            "-c",
            "checkout.workers=0",
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            path,
            &start,
        ],
    )
    .context("add worktree")?;
    Ok(())
}

/// origin's default branch: as recorded by the clone, or else asked of the
/// remote.
fn default_branch(repo: &Path) -> Result<String> {
    if let Ok(head) = git(
        repo,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) && let Some(branch) = head.trim().strip_prefix("origin/")
    {
        return Ok(branch.to_string());
    }
    let listed = git(repo, &["ls-remote", "--symref", "origin", "HEAD"])
        .context("ask origin for its default branch")?;
    listed
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .map(str::to_string)
        .context("origin has no default branch")
}

/// What removing a thread's worktree did with its branch.
#[derive(Debug, PartialEq, Eq)]
pub enum Removed {
    /// The branch had nothing that isn't on a remote, so it went too.
    BranchDeleted,
    /// The branch has commits found nowhere else; it stays in the repository.
    BranchKept(String),
}

/// Removes a thread's worktree at `path`, and its branch when nothing would
/// be lost. Uncommitted changes in the worktree go with it: the thread was
/// deleted. Refuses anything that isn't a linked worktree on a `sense/`
/// branch, so the user's own checkouts and worktrees are never touched.
pub fn remove(path: &Path) -> Result<Removed> {
    let common = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let own = git(path, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    if common.trim() == own.trim() {
        bail!(
            "{} is a repository's main checkout, not a worktree",
            path.display()
        );
    }
    let repo = Path::new(common.trim())
        .parent()
        .context("worktree without a repository")?
        .to_path_buf();
    let branch = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let branch = branch.trim().to_string();
    if !branch.starts_with("sense/") {
        bail!("{} is on {branch}, not a thread's branch", path.display());
    }
    let target = path.to_str().context("worktree path isn't UTF-8")?;
    git(&repo, &["worktree", "remove", "--force", target]).context("remove worktree")?;
    let unique = git(
        &repo,
        &["rev-list", "--count", &branch, "--not", "--remotes"],
    )?;
    if unique.trim() != "0" {
        return Ok(Removed::BranchKept(branch));
    }
    git(&repo, &["branch", "--quiet", "-D", &branch]).context("delete branch")?;
    Ok(Removed::BranchDeleted)
}

/// A branch name for a thread: `sense/` and a slug of `title`, made unique
/// with `suffix`.
pub fn branch_name(title: &str, suffix: u64) -> String {
    let slug: String = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "thread".to_string()
    } else {
        slug
    };
    format!("sense/{slug}-{suffix}")
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{Removed, branch_name, create, git, remove};
    use std::path::{Path, PathBuf};

    fn run(dir: &Path, args: &[&str]) -> String {
        let mut all = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ];
        all.extend_from_slice(args);
        git(dir, &all).unwrap()
    }

    /// A clone of an `origin` whose main has moved past the clone's main,
    /// with the clone on a feature branch with uncommitted work.
    fn repo_with_origin(name: &str) -> (PathBuf, PathBuf) {
        repo_with_origin_on(name, "main")
    }

    fn repo_with_origin_on(name: &str, main: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("sense-worktree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let origin = root.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        run(&origin, &["init", "--quiet", "-b", main]);
        std::fs::write(origin.join("a.txt"), "one\n").unwrap();
        run(&origin, &["add", "--all"]);
        run(&origin, &["commit", "--quiet", "-m", "one"]);
        let clone = root.join("clone");
        run(
            &root,
            &["clone", "--quiet", origin.to_str().unwrap(), "clone"],
        );
        std::fs::write(origin.join("a.txt"), "two\n").unwrap();
        run(&origin, &["commit", "--quiet", "-am", "two"]);
        run(&clone, &["checkout", "--quiet", "-b", "feature"]);
        std::fs::write(clone.join("wip.txt"), "uncommitted\n").unwrap();
        (root, clone)
    }

    #[test]
    fn a_worktree_starts_from_the_latest_origin_main_and_leaves_the_checkout_alone() {
        let (root, clone) = repo_with_origin("fresh");
        let path = root.join("worktrees/thread");
        create(&clone, &path, "sense/fix-1").unwrap();

        // The fetched origin/main, not the clone's stale local main.
        assert_eq!(
            std::fs::read_to_string(path.join("a.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            run(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "sense/fix-1"
        );
        assert_eq!(
            run(&path, &["rev-parse", "HEAD"]),
            run(&clone, &["rev-parse", "origin/main"])
        );
        // The user's checkout: same branch, same files, work still there.
        assert_eq!(
            run(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "feature"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(clone.join("wip.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_repositorys_checkout_hooks_do_not_run() {
        let (root, clone) = repo_with_origin("hooks");
        let marker = root.join("hook-ran");
        let hook = clone.join(".git/hooks/post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&hook)
            .status()
            .unwrap();
        // The hook is live: an ordinary checkout runs it.
        run(&clone, &["checkout", "--quiet", "-b", "probe"]);
        assert!(
            marker.exists(),
            "the test's hook should run on a plain checkout"
        );
        std::fs::remove_file(&marker).unwrap();

        create(&clone, &root.join("worktrees/thread"), "sense/fix-1").unwrap();
        assert!(
            !marker.exists(),
            "making a thread's worktree ran the repository's hook"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_taken_branch_name_fails_instead_of_reusing_it() {
        let (root, clone) = repo_with_origin("taken");
        create(&clone, &root.join("worktrees/a"), "sense/same-1").unwrap();
        assert!(create(&clone, &root.join("worktrees/b"), "sense/same-1").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_remote_whose_default_branch_is_not_main_works_too() {
        let (root, clone) = repo_with_origin_on("master", "master");
        let path = root.join("worktrees/thread");
        create(&clone, &path, "sense/fix-1").unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("a.txt")).unwrap(),
            "two\n"
        );
        // Without the clone's record of origin's HEAD, the remote is asked.
        run(&clone, &["remote", "set-head", "origin", "--delete"]);
        create(&clone, &root.join("worktrees/again"), "sense/fix-2").unwrap();
        assert_eq!(
            run(&root.join("worktrees/again"), &["rev-parse", "HEAD"]),
            run(&clone, &["rev-parse", "origin/master"])
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_worktree_without_a_reachable_origin() {
        let (root, clone) = repo_with_origin("unreachable");
        std::fs::remove_dir_all(root.join("origin")).unwrap();
        assert!(create(&clone, &root.join("worktrees/a"), "sense/a-1").is_err());
        assert!(!root.join("worktrees/a").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_a_thread_worktree_takes_its_folder_and_its_branch() {
        let (root, clone) = repo_with_origin("remove");
        let path = root.join("worktrees/thread");
        create(&clone, &path, "sense/fix-1").unwrap();
        std::fs::write(path.join("a.txt"), "edited by the agent\n").unwrap();
        assert_eq!(remove(&path).unwrap(), Removed::BranchDeleted);
        assert!(!path.exists());
        assert!(!run(&clone, &["worktree", "list"]).contains("worktrees/thread"));
        assert_eq!(run(&clone, &["branch", "--list", "sense/fix-1"]), "");
        // The checkout is as it was.
        assert_eq!(
            run(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "feature"
        );
        assert!(clone.join("wip.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_branch_with_commits_found_nowhere_else_is_kept() {
        let (root, clone) = repo_with_origin("keep");
        let path = root.join("worktrees/thread");
        create(&clone, &path, "sense/fix-1").unwrap();
        std::fs::write(path.join("a.txt"), "committed\n").unwrap();
        run(&path, &["commit", "--quiet", "-am", "work"]);
        let work = run(&path, &["rev-parse", "HEAD"]);
        assert_eq!(
            remove(&path).unwrap(),
            Removed::BranchKept("sense/fix-1".into())
        );
        assert!(!path.exists());
        assert_eq!(run(&clone, &["rev-parse", "sense/fix-1"]), work);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_users_own_checkouts_are_never_removed() {
        let (root, clone) = repo_with_origin("refuse");
        // The main checkout, even on a branch named like a thread's.
        run(&clone, &["checkout", "--quiet", "-b", "sense/mine"]);
        assert!(remove(&clone).is_err());
        assert!(clone.join("wip.txt").exists());
        // A worktree the user made on their own branch.
        let theirs = root.join("theirs");
        run(
            &clone,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "experiment",
                theirs.to_str().unwrap(),
            ],
        );
        assert!(remove(&theirs).is_err());
        assert!(theirs.join("a.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn branch_names_are_safe_slugs() {
        assert_eq!(
            branch_name("Fix the merge-queue bug!", 7),
            "sense/fix-the-merge-queue-bug-7"
        );
        assert_eq!(branch_name("  ☃ ", 2), "sense/thread-2");
        assert_eq!(branch_name("a b c d e f g h", 1), "sense/a-b-c-d-e-f-1");
    }
}
