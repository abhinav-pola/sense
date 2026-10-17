//! What a chat changed on disk, measured with git against a snapshot taken
//! before the agent ran.
//!
//! Each chat gets a private git directory and index under the app's data
//! folder, pointed at the chat's working folder. Because the diff compares the
//! disk to the snapshot, it covers every tool, shell commands included, and a
//! file created then deleted leaves no trace.
//!
//! Inside a git repository the snapshot starts from a copy of the repository's
//! index and reads its objects through git's alternates, so it only hashes
//! files that differ from what git already knows: about the cost of `git
//! status`. The repository itself is only ever read. Outside git, the first
//! snapshot hashes every file once.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

/// Long enough for a large repository; a folder that takes longer to scan
/// is too big to track on every tool call.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Heavy directories skipped even when the folder has no `.gitignore`.
const DEFAULT_EXCLUDES: &str = "node_modules/\ntarget/\n.venv/\n__pycache__/\n.DS_Store\n";

/// Where a chat's snapshot lives: the app's data folder, never the project.
pub fn store_dir(chat: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/Sense/snapshots")
        .join(chat)
}

/// The git repository a folder belongs to, as far as reading it goes.
struct Repo {
    top: PathBuf,
    objects: PathBuf,
    index: PathBuf,
}

impl Repo {
    fn containing(folder: &Path) -> Option<Self> {
        let output = Command::new("git")
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-common-dir",
                "--git-path",
                "index",
            ])
            .current_dir(folder)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let mut lines = text.lines().map(PathBuf::from);
        let (top, common, index) = (lines.next()?, lines.next()?, lines.next()?);
        Some(Self {
            top,
            objects: common.join("objects"),
            index,
        })
    }
}

pub struct Snapshot {
    git_dir: PathBuf,
    work_tree: PathBuf,
    base: String,
}

impl Snapshot {
    /// Records `folder` as it is now, storing git data in `git_dir`.
    pub fn take(folder: &Path, git_dir: &Path) -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::take_with_home(folder, git_dir, home.as_deref())
    }

    fn take_with_home(folder: &Path, git_dir: &Path, home: Option<&Path>) -> Result<Self> {
        // Compared resolved: git reports /private/var where the caller may
        // say /var, and a symlinked home must still match.
        let resolve =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let home = home.map(resolve);
        let too_broad = |path: &Path| {
            let path = resolve(path);
            path == Path::new("/") || Some(&path) == home.as_ref()
        };
        if too_broad(folder) {
            bail!(
                "tracking changes needs a project folder, not {}",
                folder.display()
            );
        }
        // A home folder kept in git (dotfiles) would make every project
        // under it look like part of one huge repository.
        let repo = Repo::containing(folder).filter(|repo| !too_broad(&repo.top));
        std::fs::create_dir_all(git_dir)?;
        let mut snapshot = Self {
            git_dir: git_dir.to_path_buf(),
            // Inside a repository, the whole repository: the agent may edit
            // outside the chat's subfolder.
            work_tree: repo
                .as_ref()
                .map_or_else(|| folder.to_path_buf(), |repo| repo.top.clone()),
            base: String::new(),
        };
        snapshot.git(&["init", "--quiet"])?;
        snapshot.git(&["config", "core.autocrlf", "false"])?;
        snapshot.git(&["config", "gc.auto", "0"])?;
        std::fs::create_dir_all(git_dir.join("info"))?;
        let mut excludes = DEFAULT_EXCLUDES.to_string();
        // The snapshot's own files, if the store happens to be inside.
        if let Ok(inside) = git_dir.strip_prefix(&snapshot.work_tree) {
            excludes.push_str(&format!("/{}/\n", inside.display()));
        }
        std::fs::write(git_dir.join("info/exclude"), excludes)?;
        if let Some(repo) = &repo {
            std::fs::write(
                git_dir.join("objects/info/alternates"),
                format!("{}\n", repo.objects.display()),
            )?;
            if std::fs::copy(&repo.index, git_dir.join("index")).is_err() {
                // No index yet (a fresh repository); a full scan is the same work.
                let _ = std::fs::remove_file(git_dir.join("index"));
            }
        }
        if snapshot.git(&["add", "--all"]).is_err() && repo.is_some() {
            // An index format this git can't reuse (e.g. a split index whose
            // shared part lives in the repository): start from empty.
            std::fs::remove_file(git_dir.join("index"))?;
            snapshot.git(&["add", "--all"])?;
        }
        snapshot.base = snapshot.git(&["write-tree"])?.trim().to_string();
        Ok(snapshot)
    }

    /// Unified git diff of the folder now against the snapshot.
    pub fn diff(&self) -> Result<String> {
        self.git(&["add", "--all"])?;
        self.git(&[
            // Paths as they are, not octal-escaped.
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            &self.base,
        ])
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let mut child = Command::new("git")
            .args(args)
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.work_tree)
            .env("GIT_INDEX_FILE", self.git_dir.join("index"))
            // The user's git config must not change what the diff contains.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&self.work_tree)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run git")?;
        let deadline = Instant::now() + SCAN_TIMEOUT;
        // Drain stdout while waiting so a large diff can't block the pipe.
        let mut stdout = child.stdout.take().context("git stdout")?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut stdout, &mut out).map(|_| out)
        });
        loop {
            if let Some(status) = child.try_wait()? {
                let out = reader
                    .join()
                    .map_err(|_| anyhow::anyhow!("git reader panicked"))??;
                if !status.success() {
                    let mut err = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = std::io::Read::read_to_string(&mut stderr, &mut err);
                    }
                    bail!("git {}: {}", args.join(" "), err.trim());
                }
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                bail!(
                    "{} is too large to track changes in",
                    self.work_tree.display()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sense-snapshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn diff_covers_any_writer_and_forgets_deleted_files() {
        let root = scratch("any-writer");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        let snapshot = Snapshot::take(&work, &root.join("git")).unwrap();
        assert_eq!(snapshot.diff().unwrap(), "");

        std::fs::write(work.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        std::fs::write(work.join("scratch.txt"), "temporary\n").unwrap();
        let patch = snapshot.diff().unwrap();
        assert!(patch.contains("-    return a - b") && patch.contains("+    return a + b"));
        assert!(patch.contains("scratch.txt"));

        std::fs::remove_file(work.join("scratch.txt")).unwrap();
        let patch = snapshot.diff().unwrap();
        assert!(
            !patch.contains("scratch.txt"),
            "deleted file still listed:\n{patch}"
        );
        assert!(patch.contains("calc.py"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn leaves_the_folders_own_repository_alone() {
        let root = scratch("own-repo");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&work)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        let before = std::fs::read_dir(work.join(".git")).unwrap().count();

        let snapshot = Snapshot::take(&work, &root.join("git")).unwrap();
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        assert!(snapshot.diff().unwrap().contains("+two"));

        assert!(
            !work.join(".git/index").exists(),
            "the folder's own index was written"
        );
        assert_eq!(
            std::fs::read_dir(work.join(".git")).unwrap().count(),
            before
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Loose objects and packs, ignoring the temporary and lock files git's
    /// background maintenance may create at any moment.
    fn stored_objects(objects: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for dir in std::fs::read_dir(objects).unwrap().flatten() {
            let name = dir.file_name().to_string_lossy().into_owned();
            let is_fanout = name.len() == 2 && name.chars().all(|ch| ch.is_ascii_hexdigit());
            if !(is_fanout || name == "pack") {
                continue;
            }
            for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
                let file = file.file_name().to_string_lossy().into_owned();
                if !file.starts_with("tmp") && !file.ends_with(".lock") {
                    found.push(format!("{name}/{file}"));
                }
            }
        }
        found.sort();
        found
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn reuses_a_repository_without_writing_to_it() {
        let root = scratch("committed-repo");
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        git_in(&work, &["init", "--quiet"]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        std::fs::write(work.join("sub/b.txt"), "keep\n").unwrap();
        git_in(&work, &["add", "--all"]);
        git_in(&work, &["commit", "--quiet", "-m", "init"]);
        std::fs::write(work.join("dirty.txt"), "uncommitted before the chat\n").unwrap();
        let objects_before = stored_objects(&work.join(".git/objects"));
        let index_before = std::fs::read(work.join(".git/index")).unwrap();

        // Started from a subfolder: the snapshot still covers the repository.
        let snapshot = Snapshot::take(&work.join("sub"), &root.join("git")).unwrap();
        assert_eq!(
            snapshot.diff().unwrap(),
            "",
            "pre-existing changes are part of the baseline"
        );
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        let patch = snapshot.diff().unwrap();
        assert!(patch.contains("-one") && patch.contains("+two"), "{patch}");
        assert!(!patch.contains("dirty.txt"));

        assert_eq!(stored_objects(&work.join(".git/objects")), objects_before);
        assert_eq!(
            std::fs::read(work.join(".git/index")).unwrap(),
            index_before
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `SENSE_SNAPSHOT_BENCH=<folder> cargo test -p sense --release snapshot_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn snapshot_bench() {
        let folder =
            PathBuf::from(std::env::var("SENSE_SNAPSHOT_BENCH").expect("set SENSE_SNAPSHOT_BENCH"));
        let root = scratch("bench");
        let started = Instant::now();
        let snapshot = Snapshot::take(&folder, &root.join("git")).unwrap();
        let taken = started.elapsed();
        let started = Instant::now();
        let patch = snapshot.diff().unwrap();
        println!(
            "take {taken:?}, diff {:?} ({} bytes)",
            started.elapsed(),
            patch.len()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Every file under `.git` except git's own temporary and lock files,
    /// with its contents: refs, HEAD, config, logs, hooks, index, objects.
    fn git_dir_contents(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("tmp") || name.ends_with(".lock") {
                    continue;
                }
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    let relative = path.strip_prefix(root).unwrap().to_path_buf();
                    out.insert(relative, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn committed_repo(work: &Path, files: usize) {
        std::fs::create_dir_all(work).unwrap();
        git_in(work, &["init", "--quiet"]);
        for n in 0..files {
            std::fs::write(work.join(format!("f{n}.txt")), format!("file {n}\n")).unwrap();
        }
        git_in(work, &["add", "--all"]);
        git_in(work, &["commit", "--quiet", "-m", "init"]);
    }

    #[test]
    fn never_writes_to_the_repository() {
        let root = scratch("untouched");
        let work = root.join("work");
        committed_repo(&work, 3);
        std::fs::write(work.join("dirty.txt"), "uncommitted\n").unwrap();
        let before = git_dir_contents(&work.join(".git"));

        let snapshot = Snapshot::take(&work, &root.join("git")).unwrap();
        std::fs::write(work.join("f0.txt"), "changed\n").unwrap();
        std::fs::write(work.join("new.txt"), "new\n").unwrap();
        snapshot.diff().unwrap();

        assert_eq!(
            git_dir_contents(&work.join(".git")),
            before,
            "the repository's .git changed"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reuses_the_repository_index_instead_of_rehashing() {
        let root = scratch("index-reuse");
        let work = root.join("work");
        committed_repo(&work, 20);
        // Entries written in the same second as the files are "racily clean"
        // and get re-hashed regardless; wait so the copied index can vouch.
        std::thread::sleep(Duration::from_millis(1100));
        let git_dir = root.join("git");
        Snapshot::take(&work, &git_dir).unwrap();
        // Unchanged files are found through the repository's objects, so the
        // snapshot stores no copies of them.
        let stored = stored_objects(&git_dir.join("objects"));
        assert!(stored.is_empty(), "rehashed unchanged files: {stored:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_home_folder_in_git_doesnt_swallow_projects_under_it() {
        let root = scratch("home-repo");
        let home = root.join("home");
        committed_repo(&home, 1);
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("main.py"), "print(1)\n").unwrap();

        let snapshot = Snapshot::take_with_home(&project, &root.join("git"), Some(&home)).unwrap();
        std::fs::write(project.join("main.py"), "print(2)\n").unwrap();
        std::fs::write(home.join("f0.txt"), "outside the project\n").unwrap();
        let patch = snapshot.diff().unwrap();
        assert!(patch.contains("+print(2)"), "{patch}");
        assert!(
            !patch.contains("f0.txt"),
            "tracked the whole home folder:\n{patch}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_home_and_root_without_scanning() {
        let root = scratch("refuse");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        for folder in [home.as_path(), Path::new("/")] {
            let started = Instant::now();
            let refused = Snapshot::take_with_home(folder, &root.join("git"), Some(&home));
            let error = refused.err().expect("must refuse").to_string();
            assert!(error.contains("needs a project folder"), "{error}");
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "scanned before refusing"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn names_with_spaces_and_unicode_come_through_unquoted() {
        let root = scratch("names");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("café menu.txt"), "old\n").unwrap();
        let snapshot = Snapshot::take(&work, &root.join("git")).unwrap();
        std::fs::write(work.join("café menu.txt"), "new\n").unwrap();
        let patch = snapshot.diff().unwrap();
        let files = gpui_kit::component::diff::DiffFile::parse(&patch).unwrap();
        assert_eq!(files.len(), 1, "{patch}");
        assert_eq!(files[0].path().as_ref(), "café menu.txt");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_store_inside_the_folder_doesnt_show_up() {
        let root = scratch("store-inside");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        let snapshot = Snapshot::take(&work, &work.join(".sense/git")).unwrap();
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        let patch = snapshot.diff().unwrap();
        assert!(
            patch.contains("+two") && !patch.contains(".sense"),
            "{patch}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
