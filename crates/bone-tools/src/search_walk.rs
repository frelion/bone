use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};
use walkdir::{IntoIter, WalkDir};

use crate::{
    ToolError, ToolLimits,
    workspace::{ResolvedPath, path_to_slashes},
};

pub(crate) enum SearchWalkEvent {
    File(PathBuf),
    Warning(String),
}

pub(crate) struct CancellationGuard {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl CancellationGuard {
    pub(crate) fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

/// A streaming workspace walk that exposes every raw directory entry before
/// applying ignore rules, so entry, ignore-file, and cancellation limits stay hard.
pub(crate) struct SearchWalk<'a> {
    iter: IntoIter,
    search_root: PathBuf,
    workspace_root: &'a Path,
    include_hidden: bool,
    limits: &'a ToolLimits,
    cancelled: &'a AtomicBool,
    rules: IgnoreRules,
    pending: VecDeque<String>,
    scanned_entries: usize,
    ignore_bytes: u64,
    truncated: bool,
    done: bool,
}

impl<'a> SearchWalk<'a> {
    pub(crate) fn new(
        search_root: &Path,
        workspace_root: &'a Path,
        include_hidden: bool,
        limits: &'a ToolLimits,
        cancelled: &'a AtomicBool,
    ) -> Self {
        Self {
            iter: WalkDir::new(search_root).follow_links(false).into_iter(),
            search_root: search_root.to_path_buf(),
            workspace_root,
            include_hidden,
            limits,
            cancelled,
            rules: IgnoreRules::new(search_root),
            pending: VecDeque::new(),
            scanned_entries: 0,
            ignore_bytes: 0,
            truncated: false,
            done: false,
        }
    }

    pub(crate) fn next_event(&mut self) -> Option<SearchWalkEvent> {
        if let Some(warning) = self.pending.pop_front() {
            return Some(SearchWalkEvent::Warning(warning));
        }
        if self.done {
            return None;
        }

        loop {
            // Check before `next`: fetching an entry can open a directory.
            if self.cancelled.load(Ordering::Relaxed) {
                self.truncated = true;
                self.done = true;
                return None;
            }
            if self.scanned_entries == self.limits.max_walk_entries {
                self.truncated = true;
                self.done = true;
                return None;
            }

            let Some(item) = self.iter.next() else {
                self.done = true;
                return None;
            };
            self.scanned_entries += 1;

            let entry = match item {
                Ok(entry) => entry,
                Err(error) => {
                    return Some(SearchWalkEvent::Warning(format!(
                        "walk error under {}: {}",
                        display_or_dot(&workspace_display(&self.search_root, self.workspace_root,)),
                        walk_error_summary(&error),
                    )));
                }
            };
            let is_directory = entry.file_type().is_dir();

            if entry.depth() > 0 {
                let ignored = if is_vcs_name(entry.file_name()) {
                    true
                } else {
                    self.rules
                        .matched(entry.path(), is_directory)
                        .unwrap_or_else(|| {
                            !self.include_hidden && is_hidden_name(entry.file_name())
                        })
                };
                if ignored {
                    if is_directory {
                        self.iter.skip_current_dir();
                    }
                    continue;
                }
            }

            if is_directory {
                match load_directory_rules(
                    entry.path(),
                    self.workspace_root,
                    self.limits,
                    self.cancelled,
                    &mut self.ignore_bytes,
                ) {
                    Ok(loaded) => {
                        if let Some(rules) = loaded.rules {
                            self.rules.insert(entry.path().to_path_buf(), rules);
                        }
                        self.pending.extend(loaded.warnings);
                    }
                    Err(LoadFailure::Cancelled) => {
                        self.truncated = true;
                        self.done = true;
                        return None;
                    }
                    Err(LoadFailure::Unsafe(warning)) => {
                        // Ignoring the ignore file could expose paths that the
                        // workspace intended to hide, so fail closed for this
                        // directory while allowing safe siblings to continue.
                        self.iter.skip_current_dir();
                        self.truncated = true;
                        return Some(SearchWalkEvent::Warning(warning));
                    }
                }
                if let Some(warning) = self.pending.pop_front() {
                    return Some(SearchWalkEvent::Warning(warning));
                }
                continue;
            }

            if entry.file_type().is_file() && !entry.file_type().is_symlink() {
                return Some(SearchWalkEvent::File(entry.into_path()));
            }
        }
    }

    pub(crate) fn scanned_entries(&self) -> usize {
        self.scanned_entries
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Default)]
struct IgnoreRules {
    root: PathBuf,
    by_directory: HashMap<PathBuf, DirectoryRules>,
}

impl IgnoreRules {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            by_directory: HashMap::new(),
        }
    }

    fn insert(&mut self, directory: PathBuf, rules: DirectoryRules) {
        self.by_directory.insert(directory, rules);
    }

    fn matched(&self, path: &Path, is_directory: bool) -> Option<bool> {
        self.first_match(path, is_directory, |rules| rules.ignore.as_ref())
            .or_else(|| self.first_match(path, is_directory, |rules| rules.gitignore.as_ref()))
            .or_else(|| self.first_match(path, is_directory, |rules| rules.exclude.as_ref()))
    }

    fn first_match(
        &self,
        path: &Path,
        is_directory: bool,
        matcher: fn(&DirectoryRules) -> Option<&Gitignore>,
    ) -> Option<bool> {
        let mut directory = path.parent();
        while let Some(current) = directory {
            if !current.starts_with(&self.root) {
                break;
            }
            if let Some(rules) = self.by_directory.get(current)
                && let Some(matcher) = matcher(rules)
            {
                match matcher.matched(path, is_directory) {
                    Match::Ignore(_) => return Some(true),
                    Match::Whitelist(_) => return Some(false),
                    Match::None => {}
                }
            }
            if current == self.root {
                break;
            }
            directory = current.parent();
        }
        None
    }
}

struct DirectoryRules {
    ignore: Option<Gitignore>,
    gitignore: Option<Gitignore>,
    exclude: Option<Gitignore>,
}

impl DirectoryRules {
    fn is_empty(&self) -> bool {
        self.ignore.is_none() && self.gitignore.is_none() && self.exclude.is_none()
    }
}

struct LoadedDirectory {
    rules: Option<DirectoryRules>,
    warnings: Vec<String>,
}

enum LoadFailure {
    Cancelled,
    Unsafe(String),
}

fn load_directory_rules(
    directory: &Path,
    workspace_root: &Path,
    limits: &ToolLimits,
    cancelled: &AtomicBool,
    total_bytes: &mut u64,
) -> Result<LoadedDirectory, LoadFailure> {
    let bytes_before = *total_bytes;
    let mut warnings = Vec::new();

    let ignore = load_ignore_file(
        directory,
        &directory.join(".ignore"),
        workspace_root,
        limits,
        cancelled,
        total_bytes,
        &mut warnings,
    );
    let ignore = match ignore {
        Ok(matcher) => matcher,
        Err(error) => {
            *total_bytes = bytes_before;
            return Err(error);
        }
    };
    let gitignore = load_ignore_file(
        directory,
        &directory.join(".gitignore"),
        workspace_root,
        limits,
        cancelled,
        total_bytes,
        &mut warnings,
    );
    let gitignore = match gitignore {
        Ok(matcher) => matcher,
        Err(error) => {
            *total_bytes = bytes_before;
            return Err(error);
        }
    };
    let exclude_path = match safe_git_exclude(directory, workspace_root) {
        Ok(path) => path,
        Err(error) => {
            *total_bytes = bytes_before;
            return Err(error);
        }
    };
    let exclude = match exclude_path {
        None => None,
        Some(path) => match load_ignore_file(
            directory,
            &path,
            workspace_root,
            limits,
            cancelled,
            total_bytes,
            &mut warnings,
        ) {
            Ok(matcher) => matcher,
            Err(error) => {
                *total_bytes = bytes_before;
                return Err(error);
            }
        },
    };

    let rules = DirectoryRules {
        ignore,
        gitignore,
        exclude,
    };
    Ok(LoadedDirectory {
        rules: (!rules.is_empty()).then_some(rules),
        warnings,
    })
}

fn safe_git_exclude(
    directory: &Path,
    workspace_root: &Path,
) -> Result<Option<PathBuf>, LoadFailure> {
    let dot_git = directory.join(".git");
    let metadata = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(unsafe_source(
                directory,
                &dot_git,
                workspace_root,
                format!("could not inspect it ({:?})", error.kind()),
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(unsafe_source(
            directory,
            &dot_git,
            workspace_root,
            "VCS metadata marker is a symbolic link".to_owned(),
        ));
    }
    if metadata.is_file() {
        // Worktree gitdir pointers may target paths outside the workspace.
        // Local excludes are therefore safely disabled for this repository.
        return Ok(None);
    }
    if !metadata.is_dir() {
        return Err(unsafe_source(
            directory,
            &dot_git,
            workspace_root,
            "VCS metadata marker is not a regular file or directory".to_owned(),
        ));
    }

    let info = dot_git.join("info");
    match fs::symlink_metadata(&info) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(unsafe_source(
                directory,
                &info,
                workspace_root,
                "VCS info directory is a symbolic link".to_owned(),
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(unsafe_source(
                directory,
                &info,
                workspace_root,
                "VCS info path is not a directory".to_owned(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(unsafe_source(
                directory,
                &info,
                workspace_root,
                format!("could not inspect it ({:?})", error.kind()),
            ));
        }
    }
    Ok(Some(info.join("exclude")))
}

#[allow(clippy::too_many_arguments)]
fn load_ignore_file(
    matcher_root: &Path,
    path: &Path,
    workspace_root: &Path,
    limits: &ToolLimits,
    cancelled: &AtomicBool,
    total_bytes: &mut u64,
    warnings: &mut Vec<String>,
) -> Result<Option<Gitignore>, LoadFailure> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(LoadFailure::Cancelled);
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(unsafe_source(
                matcher_root,
                path,
                workspace_root,
                format!("could not inspect it ({:?})", error.kind()),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(unsafe_source(
            matcher_root,
            path,
            workspace_root,
            "it is not a regular, non-symlink file".to_owned(),
        ));
    }

    let file = File::open(path).map_err(|error| {
        unsafe_source(
            matcher_root,
            path,
            workspace_root,
            format!("could not open it ({:?})", error.kind()),
        )
    })?;
    let snapshot = file.metadata().map_err(|error| {
        unsafe_source(
            matcher_root,
            path,
            workspace_root,
            format!("could not inspect the open file ({:?})", error.kind()),
        )
    })?;
    if !snapshot.is_file() {
        return Err(unsafe_source(
            matcher_root,
            path,
            workspace_root,
            "the opened source is not a regular file".to_owned(),
        ));
    }
    if snapshot.len() > limits.max_ignore_file_bytes {
        return Err(unsafe_source(
            matcher_root,
            path,
            workspace_root,
            format!(
                "it is {} bytes; limit is {}",
                snapshot.len(),
                limits.max_ignore_file_bytes
            ),
        ));
    }
    let next_total = total_bytes.checked_add(snapshot.len()).ok_or_else(|| {
        unsafe_source(
            matcher_root,
            path,
            workspace_root,
            "cumulative ignore size overflowed".to_owned(),
        )
    })?;
    if next_total > limits.max_ignore_total_bytes {
        return Err(unsafe_source(
            matcher_root,
            path,
            workspace_root,
            format!(
                "cumulative ignore size would exceed {} bytes",
                limits.max_ignore_total_bytes
            ),
        ));
    }
    *total_bytes = next_total;

    let mut bytes = Vec::new();
    CancellableReader::new(file, cancelled)
        .take(snapshot.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            if cancelled.load(Ordering::Relaxed) {
                LoadFailure::Cancelled
            } else {
                unsafe_source(
                    matcher_root,
                    path,
                    workspace_root,
                    format!("could not read it ({:?})", error.kind()),
                )
            }
        })?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(LoadFailure::Cancelled);
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > snapshot.len() {
        return Err(unsafe_source(
            matcher_root,
            path,
            workspace_root,
            "it grew while being read".to_owned(),
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        unsafe_source(
            matcher_root,
            path,
            workspace_root,
            "it is not valid UTF-8".to_owned(),
        )
    })?;

    let mut builder = GitignoreBuilder::new(matcher_root);
    let mut invalid_patterns = 0usize;
    for (index, mut line) in text.lines().enumerate() {
        if index == 0 {
            line = line.trim_start_matches('\u{feff}');
        }
        if builder.add_line(Some(path.to_path_buf()), line).is_err() {
            invalid_patterns += 1;
        }
    }
    let matcher = builder.build().map_err(|_| {
        unsafe_source(
            matcher_root,
            path,
            workspace_root,
            "its ignore matcher could not be built".to_owned(),
        )
    })?;
    if invalid_patterns > 0 {
        warnings.push(format!(
            "ignore file {} contained {invalid_patterns} invalid pattern(s); valid rules were retained",
            workspace_display(path, workspace_root),
        ));
    }
    Ok((!matcher.is_empty()).then_some(matcher))
}

pub(crate) struct CancellableReader<'a, R> {
    inner: R,
    cancelled: &'a AtomicBool,
}

impl<'a, R> CancellableReader<'a, R> {
    pub(crate) fn new(inner: R, cancelled: &'a AtomicBool) -> Self {
        Self { inner, cancelled }
    }
}

impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) {
            // EOF is reliably terminal even through decoder/buffer wrappers;
            // `Interrupted` is commonly retried and could spin after cancel.
            return Ok(0);
        }
        self.inner.read(buffer)
    }
}

fn unsafe_source(
    directory: &Path,
    source: &Path,
    workspace_root: &Path,
    reason: String,
) -> LoadFailure {
    LoadFailure::Unsafe(format!(
        "skipped directory {}: unsafe ignore source {}: {reason}",
        display_or_dot(&workspace_display(directory, workspace_root)),
        workspace_display(source, workspace_root),
    ))
}

fn workspace_display(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root)
        .map(path_to_slashes)
        .unwrap_or_else(|_| ".".to_owned())
}

fn walk_error_summary(error: &walkdir::Error) -> String {
    error.io_error().map_or_else(
        || "filesystem traversal error".to_owned(),
        |source| format!("I/O error ({:?})", source.kind()),
    )
}

fn is_vcs_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    [".git", ".hg", ".svn"]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

pub(crate) fn reject_vcs_root(resolved: &ResolvedPath) -> Result<(), ToolError> {
    if Path::new(&resolved.display)
        .components()
        .any(|component| is_vcs_name(component.as_os_str()))
    {
        return Err(ToolError::PermissionDenied {
            path: display_or_dot(&resolved.display).to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn push_bounded(
    values: &mut Vec<String>,
    used_bytes: &mut usize,
    max_bytes: usize,
    value: String,
) -> bool {
    let cost = value.len() + usize::from(!values.is_empty());
    if used_bytes.saturating_add(cost) > max_bytes {
        return false;
    }
    *used_bytes += cost;
    values.push(value);
    true
}

pub(crate) fn push_bounded_warning(
    warnings: &mut Vec<String>,
    used_bytes: &mut usize,
    max_bytes: usize,
    warning: String,
    workspace_root: &Path,
) -> bool {
    push_bounded(
        warnings,
        used_bytes,
        max_bytes,
        redact_workspace(warning, workspace_root),
    )
}

fn redact_workspace(value: String, workspace_root: &Path) -> String {
    let root = workspace_root.display().to_string();
    if root.is_empty() || root == std::path::MAIN_SEPARATOR_STR {
        value
    } else {
        value.replace(&root, ".")
    }
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

pub(crate) fn display_or_dot(display: &str) -> &str {
    if display.is_empty() { "." } else { display }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::atomic::AtomicBool};

    use super::*;

    fn collect_files(walk: &mut SearchWalk<'_>) -> (Vec<String>, Vec<String>) {
        let mut files = Vec::new();
        let mut warnings = Vec::new();
        while let Some(event) = walk.next_event() {
            match event {
                SearchWalkEvent::File(path) => {
                    files.push(path.file_name().unwrap().to_string_lossy().into_owned());
                }
                SearchWalkEvent::Warning(warning) => warnings.push(warning),
            }
        }
        files.sort();
        (files, warnings)
    }

    #[test]
    fn entry_limit_is_checked_before_fetching_a_child() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("child.txt"), "child").unwrap();
        let limits = ToolLimits {
            max_walk_entries: 1,
            ..ToolLimits::default()
        };
        let cancelled = AtomicBool::new(false);
        let mut walk = SearchWalk::new(temp.path(), temp.path(), false, &limits, &cancelled);

        assert!(walk.next_event().is_none());
        assert_eq!(walk.scanned_entries(), 1);
        assert!(walk.truncated());
    }

    #[test]
    fn cancellable_reader_stops_before_reading_more_bytes() {
        let cancelled = AtomicBool::new(true);
        let mut reader = CancellableReader::new(&b"unread"[..], &cancelled);
        let mut output = Vec::new();

        reader.read_to_end(&mut output).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn cancellation_guard_signals_only_while_armed() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let _guard = CancellationGuard::new(cancelled.clone());
        }
        assert!(cancelled.load(Ordering::Relaxed));

        let cancelled = Arc::new(AtomicBool::new(false));
        let mut guard = CancellationGuard::new(cancelled.clone());
        guard.disarm();
        drop(guard);
        assert!(!cancelled.load(Ordering::Relaxed));
    }

    #[test]
    fn warning_paths_hide_the_workspace_prefix() {
        let root = Path::new("/private/workspace");
        assert_eq!(
            redact_workspace("search error under /private/workspace/src".to_owned(), root),
            "search error under ./src"
        );
    }

    #[test]
    fn selected_vcs_roots_are_rejected() {
        let resolved = ResolvedPath {
            absolute: PathBuf::from("/private/workspace/.GiT"),
            display: ".GiT".to_owned(),
        };

        assert!(matches!(
            reject_vcs_root(&resolved),
            Err(ToolError::PermissionDenied { path }) if path == ".GiT"
        ));
    }

    #[test]
    fn vcs_metadata_names_are_ascii_case_insensitive() {
        assert!(is_vcs_name(OsStr::new(".GIT")));
        assert!(is_vcs_name(OsStr::new(".Hg")));
        assert!(is_vcs_name(OsStr::new(".SvN")));
        assert!(!is_vcs_name(OsStr::new(".github")));
    }

    #[test]
    fn bounded_local_rules_ignore_and_can_whitelist_hidden_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".ignore"), "*.tmp\n!.keep.tmp\n!.hidden\n").unwrap();
        fs::write(temp.path().join("drop.tmp"), "drop").unwrap();
        fs::write(temp.path().join(".keep.tmp"), "keep").unwrap();
        fs::write(temp.path().join(".hidden"), "keep").unwrap();
        fs::write(temp.path().join("visible.txt"), "keep").unwrap();
        let limits = ToolLimits::default();
        let cancelled = AtomicBool::new(false);
        let mut walk = SearchWalk::new(temp.path(), temp.path(), false, &limits, &cancelled);

        let (files, warnings) = collect_files(&mut walk);
        assert_eq!(files, [".hidden", ".keep.tmp", "visible.txt"]);
        assert!(warnings.is_empty());
        assert!(!walk.truncated());
    }

    #[test]
    fn local_git_exclude_is_loaded_without_walking_vcs_metadata() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".git/info")).unwrap();
        fs::write(temp.path().join(".git/info/exclude"), "secret.txt\n").unwrap();
        fs::write(temp.path().join("secret.txt"), "secret").unwrap();
        fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        let limits = ToolLimits::default();
        let cancelled = AtomicBool::new(false);
        let mut walk = SearchWalk::new(temp.path(), temp.path(), true, &limits, &cancelled);

        let (files, warnings) = collect_files(&mut walk);
        assert_eq!(files, ["visible.txt"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn oversized_ignore_file_fails_closed_for_its_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".ignore"), "1234").unwrap();
        fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        let limits = ToolLimits {
            max_ignore_file_bytes: 3,
            ..ToolLimits::default()
        };
        let cancelled = AtomicBool::new(false);
        let mut walk = SearchWalk::new(temp.path(), temp.path(), true, &limits, &cancelled);

        let (files, warnings) = collect_files(&mut walk);
        assert!(files.is_empty());
        assert!(walk.truncated());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unsafe ignore source .ignore"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_ignore_file_fails_closed_without_opening_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        symlink("/dev/zero", temp.path().join(".ignore")).unwrap();
        fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        let limits = ToolLimits::default();
        let cancelled = AtomicBool::new(false);
        let mut walk = SearchWalk::new(temp.path(), temp.path(), true, &limits, &cancelled);

        let (files, warnings) = collect_files(&mut walk);
        assert!(files.is_empty());
        assert!(walk.truncated());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("regular, non-symlink file"));
    }
}
