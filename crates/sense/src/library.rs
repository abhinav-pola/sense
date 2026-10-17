//! How threads are organized: projects (a repository a thread can start in)
//! and folders inside them. fx keeps the conversations; this keeps only
//! where each one sits, by session id, saved as one small JSON file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub type ProjectId = u64;
pub type FolderId = u64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub projects: Vec<Project>,
    pub folders: Vec<Folder>,
    /// Where each thread sits, by fx session id.
    pub threads: BTreeMap<String, Place>,
    /// Where the last new thread was started; new windows start there.
    #[serde(default)]
    pub last_project: Option<ProjectId>,
    /// Light, dark, or as the system is.
    #[serde(default)]
    pub appearance: Appearance,
    /// The model picked last; new threads start on it.
    #[serde(default)]
    pub model: Option<String>,
    /// The model each thread last ran on, by fx session id, so a thread
    /// reopened later stays on it rather than taking the default.
    #[serde(default)]
    pub thread_models: BTreeMap<String, String>,
    /// The title each thread showed, by fx session id. fx keeps a title
    /// only when it generates one, which it doesn't for every provider.
    #[serde(default)]
    pub thread_titles: BTreeMap<String, String>,
    /// OpenRouter's model list, as last refreshed.
    #[serde(default)]
    pub models: Vec<crate::models::Model>,
    /// The text size picked with cmd +/−, if not the default.
    #[serde(default)]
    pub text_size: Option<u16>,
    next_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub project: ProjectId,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub project: Option<ProjectId>,
    pub folder: Option<FolderId>,
}

impl Library {
    /// Reads the saved library. A file that can't be read as one is moved
    /// aside to `<name>.damaged` rather than left to be overwritten by the
    /// next save, so what it held can still be recovered by hand.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            _ = std::fs::rename(path, path.with_extension("json.damaged"));
            Self::default()
        })
    }

    /// Writes through a temporary file, so a crash mid-write leaves the old
    /// library rather than half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temporary, path).context("replace library")
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Adds a project for a repository, or returns the one already there.
    pub fn add_project(&mut self, path: &Path) -> ProjectId {
        if let Some(project) = self.projects.iter().find(|p| p.path == path) {
            return project.id;
        }
        let id = self.fresh_id();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.projects.push(Project {
            id,
            name,
            path: path.to_path_buf(),
        });
        id
    }

    /// Removes a project and its folders. Its threads stay, unfiled.
    pub fn remove_project(&mut self, project: ProjectId) {
        self.projects.retain(|p| p.id != project);
        self.folders.retain(|f| f.project != project);
        for place in self.threads.values_mut() {
            if place.project == Some(project) {
                *place = Place::default();
            }
        }
    }

    pub fn add_folder(&mut self, project: ProjectId, name: &str) -> Option<FolderId> {
        self.project(project)?;
        let id = self.fresh_id();
        self.folders.push(Folder {
            id,
            project,
            name: name.to_string(),
        });
        Some(id)
    }

    pub fn rename_folder(&mut self, folder: FolderId, name: &str) {
        let name = name.trim();
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == folder)
            && !name.is_empty()
        {
            folder.name = name.to_string();
        }
    }

    /// Deletes a folder; its threads move to the project's top level.
    pub fn delete_folder(&mut self, folder: FolderId) {
        self.folders.retain(|f| f.id != folder);
        for place in self.threads.values_mut() {
            if place.folder == Some(folder) {
                place.folder = None;
            }
        }
    }

    pub fn project(&self, project: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == project)
    }

    pub fn folder(&self, folder: FolderId) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == folder)
    }

    /// Records where a thread sits. A folder must belong to the project.
    pub fn place(&mut self, session: &str, place: Place) {
        let valid = match (place.project, place.folder) {
            (_, None) => place.project.is_none_or(|p| self.project(p).is_some()),
            (Some(project), Some(folder)) => {
                self.folder(folder).is_some_and(|f| f.project == project)
            }
            (None, Some(_)) => false,
        };
        if valid {
            self.threads.insert(session.to_string(), place);
        }
    }

    /// Moves a thread into a folder of its own project, or to the project's
    /// top level. A thread never changes project this way: its project is
    /// the repository it works in.
    pub fn move_thread(&mut self, session: &str, folder: Option<FolderId>, cwd: &Path) -> bool {
        let project = self.place_of(session, cwd).project;
        let Some(project) = project else {
            return false;
        };
        if let Some(folder) = folder
            && self.folder(folder).is_none_or(|f| f.project != project)
        {
            return false;
        }
        self.threads.insert(
            session.to_string(),
            Place {
                project: Some(project),
                folder,
            },
        );
        true
    }

    /// The model a thread last ran on, if known.
    pub fn thread_model(&self, session: &str) -> Option<&str> {
        self.thread_models.get(session).map(String::as_str)
    }

    /// Records the model a thread runs on. Whether that changed anything,
    /// so callers save only when it did.
    pub fn set_thread_model(&mut self, session: &str, model: &str) -> bool {
        if self.thread_model(session) == Some(model) {
            return false;
        }
        self.thread_models
            .insert(session.to_string(), model.to_string());
        true
    }

    /// The title a thread last showed, if known.
    pub fn thread_title(&self, session: &str) -> Option<&str> {
        self.thread_titles.get(session).map(String::as_str)
    }

    /// Records the title a thread shows. Blank titles aren't kept. Whether
    /// that changed anything, so callers save only when it did.
    pub fn set_thread_title(&mut self, session: &str, title: &str) -> bool {
        let title = title.trim();
        if title.is_empty() || self.thread_title(session) == Some(title) {
            return false;
        }
        self.thread_titles
            .insert(session.to_string(), title.to_string());
        true
    }

    /// Drops a deleted thread's model and title. Whether there were any.
    pub fn forget_thread(&mut self, session: &str) -> bool {
        let model = self.thread_models.remove(session).is_some();
        let title = self.thread_titles.remove(session).is_some();
        model || title
    }

    /// Where a thread shows: where it was put, or else the project whose
    /// repository holds its folder (the innermost, if they nest).
    pub fn place_of(&self, session: &str, cwd: &Path) -> Place {
        if let Some(place) = self.threads.get(session) {
            return *place;
        }
        let project = self
            .projects
            .iter()
            .filter(|p| cwd.starts_with(&p.path))
            .max_by_key(|p| p.path.components().count())
            .map(|p| p.id);
        Place {
            project,
            folder: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Library, Place};
    use proptest::prelude::*;
    use std::path::Path;

    #[test]
    fn saves_and_loads_the_same_library() {
        let mut library = Library::default();
        let project = library.add_project(Path::new("/code/web"));
        let folder = library.add_folder(project, "Merges").unwrap();
        library.place(
            "s1",
            Place {
                project: Some(project),
                folder: Some(folder),
            },
        );
        let dir = std::env::temp_dir().join(format!("sense-library-{}", std::process::id()));
        let path = dir.join("library.json");
        library.save(&path).unwrap();
        assert_eq!(Library::load(&path), library);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Each thread keeps its own model across a restart; the default for
    /// new threads is separate. A library saved before threads kept models
    /// still loads, with none known.
    #[test]
    fn thread_models_are_kept_per_thread() {
        let mut library = Library {
            model: Some("default/model".into()),
            ..Library::default()
        };
        assert!(library.set_thread_model("s1", "openai/gpt-5"));
        assert!(!library.set_thread_model("s1", "openai/gpt-5"), "unchanged");
        assert!(library.set_thread_model("s2", "anthropic/claude-haiku-5.5"));
        let dir = std::env::temp_dir().join(format!("sense-library-models-{}", std::process::id()));
        let path = dir.join("library.json");
        library.save(&path).unwrap();
        let loaded = Library::load(&path);
        assert_eq!(loaded.thread_model("s1"), Some("openai/gpt-5"));
        assert_eq!(
            loaded.thread_model("s2"),
            Some("anthropic/claude-haiku-5.5")
        );
        assert_eq!(loaded.thread_model("s3"), None);
        assert_eq!(loaded.model.as_deref(), Some("default/model"));

        let mut loaded = loaded;
        assert!(loaded.forget_thread("s1"));
        assert!(!loaded.forget_thread("s1"));
        assert_eq!(loaded.thread_model("s1"), None);

        std::fs::write(
            &path,
            r#"{"projects":[],"folders":[],"threads":{},"next_id":0}"#,
        )
        .unwrap();
        assert_eq!(Library::load(&path).thread_model("s2"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A thread's title survives a restart even when fx kept none; blank
    /// titles don't replace one that's there, and deleting forgets it.
    #[test]
    fn thread_titles_are_kept_per_thread() {
        let mut library = Library::default();
        assert!(library.set_thread_title("s1", "Fix the add bug"));
        assert!(
            !library.set_thread_title("s1", "Fix the add bug"),
            "unchanged"
        );
        assert!(!library.set_thread_title("s1", "   "), "blank");
        assert!(library.set_thread_title("s1", "  Fix add() in calc.py "));
        assert!(library.set_thread_model("s1", "openai/gpt-5"));
        let dir = std::env::temp_dir().join(format!("sense-library-titles-{}", std::process::id()));
        let path = dir.join("library.json");
        library.save(&path).unwrap();
        let mut loaded = Library::load(&path);
        assert_eq!(loaded.thread_title("s1"), Some("Fix add() in calc.py"));
        assert_eq!(loaded.thread_title("s2"), None);
        assert!(loaded.forget_thread("s1"));
        assert_eq!(loaded.thread_title("s1"), None);
        assert_eq!(loaded.thread_model("s1"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_file_is_kept_aside_and_the_next_save_does_not_destroy_it() {
        let dir = std::env::temp_dir().join(format!("sense-library-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.json");
        std::fs::write(&path, "{ not json").unwrap();
        let mut library = Library::load(&path);
        assert_eq!(library, Library::default());
        library.add_project(Path::new("/code/web"));
        library.save(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("library.json.damaged")).unwrap(),
            "{ not json"
        );
        assert_eq!(Library::load(&path), library);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unplaced_threads_go_to_the_innermost_project_holding_their_folder() {
        let mut library = Library::default();
        let outer = library.add_project(Path::new("/code"));
        let inner = library.add_project(Path::new("/code/web"));
        assert_eq!(
            library.place_of("s", Path::new("/code/web/app")).project,
            Some(inner)
        );
        assert_eq!(
            library.place_of("s", Path::new("/code/other")).project,
            Some(outer)
        );
        assert_eq!(library.place_of("s", Path::new("/elsewhere")).project, None);
        // "/code/webby" is not inside "/code/web".
        assert_eq!(
            library.place_of("s", Path::new("/code/webby")).project,
            Some(outer)
        );
    }

    #[test]
    fn the_innermost_project_wins_whichever_was_added_first() {
        let mut library = Library::default();
        let inner = library.add_project(Path::new("/code/web"));
        library.add_project(Path::new("/code"));
        assert_eq!(
            library.place_of("s", Path::new("/code/web/app")).project,
            Some(inner)
        );
    }

    #[test]
    fn adding_a_project_twice_keeps_one() {
        let mut library = Library::default();
        let first = library.add_project(Path::new("/code/web"));
        assert_eq!(library.add_project(Path::new("/code/web")), first);
        assert_eq!(library.projects.len(), 1);
    }

    #[test]
    fn folder_names_are_trimmed_and_never_blank() {
        let mut library = Library::default();
        let project = library.add_project(Path::new("/code/web"));
        let folder = library.add_folder(project, "New folder").unwrap();
        library.rename_folder(folder, "  Merges ");
        assert_eq!(library.folder(folder).unwrap().name, "Merges");
        library.rename_folder(folder, "   ");
        assert_eq!(library.folder(folder).unwrap().name, "Merges");
    }

    #[derive(Clone, Debug)]
    enum Op {
        AddProject(u8),
        RemoveProject(u8),
        AddFolder(u8),
        DeleteFolder(u8),
        Move(u8, Option<u8>),
        /// Records a place for a thread directly, as a new thread does:
        /// project and folder picked independently, so often mismatched.
        Place(u8, Option<u8>, Option<u8>),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..3u8).prop_map(Op::AddProject),
            (0..3u8).prop_map(Op::RemoveProject),
            (0..3u8).prop_map(Op::AddFolder),
            (0..6u8).prop_map(Op::DeleteFolder),
            (0..4u8, proptest::option::of(0..6u8)).prop_map(|(t, f)| Op::Move(t, f)),
            (
                0..4u8,
                proptest::option::of(0..3u8),
                proptest::option::of(0..6u8)
            )
                .prop_map(|(t, p, f)| Op::Place(t, p, f)),
        ]
    }

    proptest! {
        /// Whatever the user does, every thread sits in exactly one valid
        /// place: no folder of a missing project, no folder of another
        /// project, no folder that was deleted.
        #[test]
        fn every_thread_always_has_a_valid_place(ops in proptest::collection::vec(op(), 0..60)) {
            let mut library = Library::default();
            let cwd = |thread: u8| format!("/repo{}/t{thread}", thread % 3);
            for op in ops {
                match op {
                    Op::AddProject(n) => _ = library.add_project(Path::new(&format!("/repo{n}"))),
                    Op::RemoveProject(n) => {
                        if let Some(p) = library.projects.get(n as usize).map(|p| p.id) {
                            library.remove_project(p);
                        }
                    }
                    Op::AddFolder(n) => {
                        if let Some(p) = library.projects.get(n as usize).map(|p| p.id) {
                            library.add_folder(p, "f");
                        }
                    }
                    Op::DeleteFolder(n) => {
                        if let Some(f) = library.folders.get(n as usize).map(|f| f.id) {
                            library.delete_folder(f);
                        }
                    }
                    Op::Place(thread, project, folder) => {
                        let project = project.and_then(|n| library.projects.get(n as usize)).map(|p| p.id);
                        let folder = folder.and_then(|n| library.folders.get(n as usize)).map(|f| f.id);
                        let session = format!("t{thread}");
                        let before = library.threads.get(&session).copied();
                        library.place(&session, Place { project, folder });
                        // Independent of `place`'s own check: a folder needs
                        // its own project named alongside it.
                        let fits = match folder {
                            None => true,
                            Some(f) => project == Some(library.folder(f).unwrap().project),
                        };
                        if fits {
                            prop_assert_eq!(library.threads.get(&session), Some(&Place { project, folder }), "a valid place was refused");
                        } else {
                            prop_assert_eq!(library.threads.get(&session).copied(), before, "an invalid place was recorded");
                        }
                    }
                    Op::Move(thread, folder) => {
                        let folder = folder.and_then(|n| library.folders.get(n as usize)).map(|f| f.id);
                        let session = format!("t{thread}");
                        let before = library.place_of(&session, Path::new(&cwd(thread)));
                        let moved = library.move_thread(&session, folder, Path::new(&cwd(thread)));
                        let after = library.place_of(&session, Path::new(&cwd(thread)));
                        if moved {
                            prop_assert_eq!(after.folder, folder);
                            prop_assert_eq!(after.project, before.project, "a move changed the thread's project");
                        } else {
                            prop_assert_eq!(after, before, "a refused move changed something");
                        }
                    }
                }
                for (session, place) in &library.threads {
                    if let Some(project) = place.project {
                        prop_assert!(library.project(project).is_some(), "{} in a removed project", session);
                    }
                    if let Some(folder) = place.folder {
                        let folder = library.folder(folder);
                        prop_assert!(folder.is_some(), "{} in a deleted folder", session);
                        prop_assert_eq!(Some(folder.unwrap().project), place.project, "{} in another project's folder", session);
                    }
                }
            }
        }
    }
}
