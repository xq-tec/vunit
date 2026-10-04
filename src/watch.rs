// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Watching the configuration file and the source directories of a workspace.
//!
//! Directories are watched instead of files, because editors save by renaming:
//!
//! - the directory of the configuration file, non-recursively;
//! - for every source pattern, its base directory (the longest prefix without glob characters),
//!   recursively if the pattern reaches into subdirectories. A missing base directory is
//!   replaced by its closest existing ancestor, watched non-recursively; once the directory is
//!   created, the project is reloaded and the watched directories are updated;
//! - the ancestors of these directories up to the workspace root, non-recursively. A watch ends
//!   when its directory is deleted, so a directory that is deleted and recreated (for example
//!   by a branch switch) is only reported by a parent that stays in place. Without it, a
//!   directory recreated while the project is reloaded would never be watched again.
//!
//! Directories inside a recursively watched one aren't watched separately. `risim-out/` and
//! `.git/` are never watched: a recursively watched directory that contains `risim-out/` is
//! replaced by a non-recursive watch of itself and recursive watches of its other
//! subdirectories. Events are debounced by [`Debouncer`] and classified by [`classify`].
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::time::Duration;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher as _;
use notify::event::ModifyKind;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::sources::file_type_of;
use crate::sources::split_pattern;
use crate::spec::LibrarySpec;
use crate::spec::ProjectSpec;

/// How long the watcher waits for further events before reporting a change.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// The longest a change is delayed by events that keep arriving.
pub const MAX_DELAY: Duration = Duration::from_secs(2);

/// What a file system event changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Change {
    /// A source file or directory was created, deleted or modified.
    Sources,
    /// The configuration file was created, deleted or modified.
    Config,
}

/// The directories to watch, with whether to watch them recursively.
pub type WatchTargets = BTreeMap<Utf8PathBuf, bool>;

/// The directories to watch for a project.
///
/// `root` is the workspace root and `excluded` the output directory (`risim-out/`).
pub fn watch_targets(
    root: &Utf8Path,
    excluded: &Utf8Path,
    config_file: Option<&Utf8Path>,
    spec: &ProjectSpec,
) -> WatchTargets {
    let mut targets = WatchTargets::new();
    if let Some(parent) = config_file.and_then(Utf8Path::parent) {
        add_existing(&mut targets, parent, false, excluded, root);
    }
    for library in &spec.libraries {
        let LibrarySpec::Sources { files, .. } = library else {
            continue;
        };
        for pattern in files {
            let (base, glob_components) = split_pattern(root, &pattern.pattern);
            if glob_components.is_empty() {
                // A file name without glob characters.
                if let Some(parent) = base.parent() {
                    add_existing(&mut targets, parent, false, excluded, root);
                }
            } else {
                let recursive = glob_components.len() > 1 || glob_components.contains(&"**");
                add_existing(&mut targets, &base, recursive, excluded, root);
            }
        }
    }
    // A second watch inside a recursive one would only duplicate its events, and notify's
    // inotify backend would stop watching new subdirectories below it.
    let recursive: Vec<Utf8PathBuf> = targets
        .iter()
        .filter(|&(_, &recursive)| recursive)
        .map(|(dir, _)| dir.clone())
        .collect();
    targets.retain(|dir, _| {
        !recursive
            .iter()
            .any(|ancestor| ancestor != dir && dir.starts_with(ancestor))
    });
    targets
}

/// Adds `dir`, or its closest existing ancestor non-recursively, and the ancestors up to `root`.
fn add_existing(
    targets: &mut WatchTargets,
    dir: &Utf8Path,
    recursive: bool,
    excluded: &Utf8Path,
    root: &Utf8Path,
) {
    if dir.is_dir() {
        add(targets, dir, recursive, excluded);
        if let Some(parent) = dir.parent()
            && parent.starts_with(root)
        {
            add_existing(targets, parent, false, excluded, root);
        }
    } else if let Some(ancestor) = dir.ancestors().skip(1).find(|ancestor| ancestor.is_dir()) {
        add_existing(targets, ancestor, false, excluded, root);
    }
}

/// Adds `dir`, keeping recursive watches out of `excluded` and `.git` directories.
fn add(targets: &mut WatchTargets, dir: &Utf8Path, recursive: bool, excluded: &Utf8Path) {
    if dir.starts_with(excluded) || dir.file_name() == Some(".git") {
        return;
    }
    if !(recursive && excluded.starts_with(dir)) {
        let entry = targets.entry(dir.to_owned()).or_insert(recursive);
        *entry |= recursive;
        return;
    }
    targets.entry(dir.to_owned()).or_insert(false);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<Utf8PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .filter_map(|entry| Utf8PathBuf::from_path_buf(entry.path()).ok())
        .collect();
    children.sort_unstable();
    for child in children {
        add(targets, &child, true, excluded);
    }
}

/// Classifies a file system event; `None` if it doesn't concern the project.
///
/// Accesses are ignored, and so are paths in `excluded` or in a `.git` directory below the
/// workspace root (the parent of `excluded`), and existing files that aren't HDL files (for
/// example editor swap files). The paths of a deleted file or directory
/// can't be checked, so they count as source changes.
pub fn classify(
    event: &notify::Event,
    config_file: Option<&Utf8Path>,
    excluded: &Utf8Path,
) -> Option<Change> {
    if matches!(event.kind, EventKind::Access(_)) {
        return None;
    }
    let mut change = None;
    for path in &event.paths {
        let Some(path) = Utf8Path::from_path(path) else {
            continue;
        };
        if config_file == Some(path) {
            return Some(Change::Config);
        }
        let in_workspace = excluded
            .parent()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path);
        if path.starts_with(excluded)
            || in_workspace
                .components()
                .any(|component| component.as_str() == ".git")
        {
            continue;
        }
        if file_type_of(path).is_some() || !path.is_file() {
            change = Some(Change::Sources);
        }
    }
    change
}

/// Collects changes until no event has arrived for [`DEBOUNCE`], or for at most
/// [`MAX_DELAY`].
#[derive(Debug, Clone, Default)]
pub struct Debouncer {
    first: Option<Instant>,
    last: Option<Instant>,
    change: Option<Change>,
}

impl Debouncer {
    /// Records a change at `now`.
    pub fn add(&mut self, change: Change, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.change = self.change.max(Some(change));
    }

    /// When the collected changes are due, if there are any.
    pub fn deadline(&self) -> Option<Instant> {
        let first = self.first?;
        let last = self.last?;
        Some((last + DEBOUNCE).min(first + MAX_DELAY))
    }

    /// Returns the collected changes and starts over: [`Change::Config`] if the configuration
    /// file changed, otherwise [`Change::Sources`] if anything changed.
    pub const fn take(&mut self) -> Option<Change> {
        self.first = None;
        self.last = None;
        self.change.take()
    }
}

/// A file system watcher for a set of directories.
pub struct DirectoryWatcher {
    watcher: RecommendedWatcher,
    watched: WatchTargets,
}

impl fmt::Debug for DirectoryWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectoryWatcher")
            .field("watched", &self.watched)
            .finish_non_exhaustive()
    }
}

impl DirectoryWatcher {
    /// Creates a watcher that sends its events to `events`.
    ///
    /// # Errors
    ///
    /// Fails if the platform watcher can't be created.
    pub fn new(
        events: mpsc::UnboundedSender<notify::Result<notify::Event>>,
    ) -> notify::Result<Self> {
        let watcher = notify::recommended_watcher(move |event| {
            // The receiver is gone once the workspace is closed.
            let _closed = events.send(event);
        })?;
        Ok(Self {
            watcher,
            watched: WatchTargets::new(),
        })
    }

    /// Forgets all watched directories, so that the next [`update`](Self::update) watches them
    /// again.
    ///
    /// For lost events (an event queue overflow), which may include the removal of a watched
    /// directory.
    pub fn forget_all(&mut self) {
        for dir in std::mem::take(&mut self.watched).into_keys() {
            if let Err(error) = self.watcher.unwatch(dir.as_std_path()) {
                tracing::debug!(%dir, %error, "failed to unwatch");
            }
        }
    }

    /// Forgets the watched directories that `event` removes or renames, together with those
    /// below them, so that the next [`update`](Self::update) watches them again if they exist.
    ///
    /// A watch ends when its directory is deleted, and follows it when it is renamed. Without
    /// this, a directory deleted and recreated before the next update would stay unwatched.
    pub fn forget_removed(&mut self, event: &notify::Event) {
        if !matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        ) {
            return;
        }
        for path in &event.paths {
            let Some(path) = Utf8Path::from_path(path) else {
                continue;
            };
            let removed: Vec<Utf8PathBuf> = self
                .watched
                .keys()
                .filter(|dir| dir.starts_with(path))
                .cloned()
                .collect();
            for dir in removed {
                self.watched.remove(&dir);
                if let Err(error) = self.watcher.unwatch(dir.as_std_path()) {
                    tracing::debug!(%dir, %error, "failed to unwatch");
                }
            }
        }
    }

    /// Watches exactly `targets`. Returns the directories that can't be watched.
    pub fn update(&mut self, targets: &WatchTargets) -> Vec<(Utf8PathBuf, notify::Error)> {
        let removed: Vec<Utf8PathBuf> = self
            .watched
            .iter()
            .filter(|&(dir, recursive)| targets.get(dir) != Some(recursive))
            .map(|(dir, _)| dir.clone())
            .collect();
        for dir in removed {
            self.watched.remove(&dir);
            if let Err(error) = self.watcher.unwatch(dir.as_std_path()) {
                // The directory may have been deleted, which ends its watch anyway.
                tracing::debug!(%dir, %error, "failed to unwatch");
            }
        }
        let mut errors = Vec::new();
        for (dir, &recursive) in targets {
            if self.watched.contains_key(dir) {
                continue;
            }
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match self.watcher.watch(dir.as_std_path(), mode) {
                Ok(()) => {
                    self.watched.insert(dir.clone(), recursive);
                },
                Err(error) => errors.push((dir.clone(), error)),
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use notify::event::AccessKind;
    use notify::event::CreateKind;

    use super::*;
    use crate::test_support::TempRoot as Temp;

    fn mkdir(temp: &Temp, rel_path: &str) {
        fs::create_dir_all(temp.root.join(rel_path)).unwrap();
    }

    fn targets(temp: &Temp, patterns: &[&str]) -> Vec<(String, bool)> {
        let root = &temp.root;
        let mut spec = ProjectSpec::new();
        spec.add_library("lib", patterns.iter().copied());
        let config = root.join("risim-config.toml");
        watch_targets(root, &root.join("risim-out"), Some(&config), &spec)
            .into_iter()
            .map(|(dir, recursive)| {
                let relative = dir.strip_prefix(root).unwrap().to_string();
                (relative, recursive)
            })
            .collect()
    }

    fn owned(targets: &[(&str, bool)]) -> Vec<(String, bool)> {
        targets
            .iter()
            .map(|&(dir, recursive)| (dir.to_owned(), recursive))
            .collect()
    }

    #[test]
    fn targets_follow_the_pattern_bases() {
        let temp = Temp::new();
        mkdir(&temp, "src/sub");
        mkdir(&temp, "tb");
        assert_eq!(
            targets(
                &temp,
                &["src/*.vhd", "src/sub/**/*.vhd", "tb/*/x.vhd", "tb/top.vhd"]
            ),
            owned(&[("", false), ("src", false), ("src/sub", true), ("tb", true)])
        );
        // A recursive watch wins over a non-recursive one of the same directory.
        assert_eq!(
            targets(&temp, &["src/*.vhd", "src/**/*.vhd"]),
            owned(&[("", false), ("src", true)])
        );
    }

    #[test]
    fn missing_bases_watch_their_closest_ancestor() {
        let temp = Temp::new();
        mkdir(&temp, "a");
        assert_eq!(
            targets(&temp, &["a/b/c/**/*.vhd", "x/top.vhd"]),
            owned(&[("", false), ("a", false)])
        );
    }

    #[test]
    fn ancestors_up_to_the_root_are_watched() {
        let temp = Temp::new();
        mkdir(&temp, "tb");
        let mut spec = ProjectSpec::new();
        spec.add_library("lib", ["tb/*.vhd"]);
        let watched: Vec<_> = watch_targets(&temp.root, &temp.root.join("risim-out"), None, &spec)
            .into_iter()
            .map(|(dir, recursive)| (dir.strip_prefix(&temp.root).unwrap().to_string(), recursive))
            .collect();
        // The workspace root stays watched when `tb` is deleted and recreated.
        assert_eq!(watched, owned(&[("", false), ("tb", false)]));
    }

    #[test]
    fn directories_inside_a_recursive_watch_are_left_out() {
        let temp = Temp::new();
        mkdir(&temp, "src/a/b");
        assert_eq!(
            targets(&temp, &["src/**/*.vhd", "src/a/b/*.vhd"]),
            owned(&[("", false), ("src", true)])
        );
    }

    #[test]
    fn recursive_watches_skip_the_output_and_git_directories() {
        let temp = Temp::new();
        mkdir(&temp, "risim-out/libraries/lib");
        mkdir(&temp, ".git/objects");
        mkdir(&temp, "src/deep");
        mkdir(&temp, "tb");
        assert_eq!(
            targets(&temp, &["**/*.vhd"]),
            owned(&[("", false), ("src", true), ("tb", true)])
        );
        assert_eq!(
            targets(&temp, &["risim-out/**/*.vhd"]),
            owned(&[("", false)])
        );
    }

    fn event(kind: EventKind, paths: &[&Utf8Path]) -> notify::Event {
        let mut event = notify::Event::new(kind);
        for path in paths {
            event = event.add_path(path.as_std_path().to_owned());
        }
        event
    }

    #[test]
    fn classifies_events() {
        let temp = Temp::new();
        let config = temp.write("risim-config.toml", "");
        let source = temp.write("src/a.vhd", "");
        let swap = temp.write("src/.a.vhd.swp", "");
        let output = temp.write("risim-out/state.json", "");
        let git = temp.write(".git/index", "");
        let deleted = temp.root.join("src/gone");
        let excluded = temp.root.join("risim-out");
        let modify = EventKind::Modify(ModifyKind::Any);
        let classify =
            |kind, paths: &[&Utf8Path]| classify(&event(kind, paths), Some(&config), &excluded);

        assert_eq!(classify(modify, &[&source]), Some(Change::Sources));
        assert_eq!(classify(modify, &[&source, &config]), Some(Change::Config));
        assert_eq!(
            classify(EventKind::Create(CreateKind::Folder), &[&deleted]),
            Some(Change::Sources)
        );
        assert_eq!(classify(modify, &[&swap]), None);
        assert_eq!(classify(modify, &[&output]), None);
        assert_eq!(classify(modify, &[&git]), None);
        assert_eq!(
            classify(EventKind::Access(AccessKind::Any), &[&source]),
            None
        );

        // Only `.git` directories inside the workspace are ignored.
        let nested_root = temp.root.join(".git/worktree");
        let nested_source = temp.write(".git/worktree/src/a.vhd", "");
        assert_eq!(
            super::classify(
                &event(modify, &[&nested_source]),
                None,
                &nested_root.join("risim-out")
            ),
            Some(Change::Sources)
        );
    }

    #[test]
    fn debouncer_waits_for_quiet_but_not_forever() {
        let start = Instant::now();
        let mut debouncer = Debouncer::default();
        assert_eq!(debouncer.deadline(), None);
        debouncer.add(Change::Sources, start);
        assert_eq!(debouncer.deadline(), Some(start + DEBOUNCE));
        let later = start + Duration::from_millis(150);
        debouncer.add(Change::Config, later);
        debouncer.add(Change::Sources, later);
        assert_eq!(debouncer.deadline(), Some(later + DEBOUNCE));
        debouncer.add(Change::Sources, start + MAX_DELAY);
        assert_eq!(debouncer.deadline(), Some(start + MAX_DELAY));
        assert_eq!(debouncer.take(), Some(Change::Config));
        assert_eq!(debouncer.deadline(), None);
        assert_eq!(debouncer.take(), None);
    }

    /// Receives events until one of a kind matching `kind` reports `path`, passing them to
    /// `forget_removed` like the workspace does; `false` if none arrives within a second.
    async fn reported(
        watcher: &mut DirectoryWatcher,
        events: &mut mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
        kind: fn(&EventKind) -> bool,
        path: &Utf8Path,
    ) -> bool {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .expect("the watcher stays open")
                    .expect("a watch event");
                watcher.forget_removed(&event);
                if kind(&event.kind) && event.paths.iter().any(|reported| reported == path) {
                    return;
                }
            }
        })
        .await
        .is_ok()
    }

    #[test]
    fn directory_recreated_during_a_reload_is_watched_again() {
        let temp = Temp::new();
        mkdir(&temp, "tb");
        let tb = temp.root.join("tb");
        let mut spec = ProjectSpec::new();
        spec.add_library("lib", ["tb/*.vhd"]);
        let current_targets =
            || watch_targets(&temp.root, &temp.root.join("risim-out"), None, &spec);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (sender, mut events) = mpsc::unbounded_channel();
            let mut watcher = DirectoryWatcher::new(sender).unwrap();
            assert!(watcher.update(&current_targets()).is_empty());

            let removed = |kind: &EventKind| matches!(kind, EventKind::Remove(_));
            let created = |kind: &EventKind| matches!(kind, EventKind::Create(_));
            fs::remove_dir_all(&tb).unwrap();
            assert!(
                reported(&mut watcher, &mut events, removed, &tb).await,
                "the deletion"
            );
            // A reload computes its targets while `tb` is missing, and `tb` is recreated
            // before the reload updates the watches.
            let reload_targets = current_targets();
            mkdir(&temp, "tb");
            assert!(watcher.update(&reload_targets).is_empty());
            assert!(
                reported(&mut watcher, &mut events, created, &tb).await,
                "the recreation triggers the next reload"
            );

            assert!(watcher.update(&current_targets()).is_empty());
            let file = temp.write("tb/new.vhd", "");
            assert!(
                reported(&mut watcher, &mut events, created, &file).await,
                "a file in the recreated directory"
            );
        });
    }
}
