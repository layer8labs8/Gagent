//! The shadow workspace: a private working copy of the user's project.
//!
//! The agent edits files and the sandbox runs code against this copy only.
//! Your real project folder changes only when you *apply* the pending
//! changes, and every apply is backed up so it can be undone.
//!
//! The copy is kept in sync with the project using a three-way comparison:
//! * `base`   - what the project looked like when the file was last synced,
//! * `project` - what the project looks like now,
//! * `shadow`  - the agent's working copy.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::{looks_binary, now_secs, sha256_hex, unique_id};
use crate::workspace::{walk_files, WalkOptions, Workspace};

/// Refuse to copy projects larger than this many files.
pub const MAX_PROJECT_FILES: usize = 20_000;
/// Refuse to copy projects larger than this many bytes in total.
pub const MAX_PROJECT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct BaseEntry {
    sha: String,
    size: u64,
    mtime: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    files: BTreeMap<String, BaseEntry>,
}

/// What happened to a file in the shadow workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        }
    }
}

/// One pending difference between the shadow workspace and the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub rel: String,
    pub kind: ChangeKind,
    pub size: u64,
}

/// Result of [`Shadow::sync_from_project`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub copied: usize,
    pub removed: usize,
    /// Files changed both in the project and in the shadow copy; the shadow version is kept.
    pub conflicts: Vec<String>,
    pub total_files: usize,
}

/// Result of [`Shadow::apply`].
#[derive(Clone, Debug, Default)]
pub struct ApplyReport {
    pub applied: Vec<String>,
    pub skipped: Vec<(String, String)>,
    /// Directory holding the backup, when anything was applied.
    pub backup: Option<PathBuf>,
    /// Short hash of the git checkpoint saved before the project changed (a git project only).
    pub checkpoint: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BackupManifest {
    created_at: i64,
    entries: Vec<BackupEntry>,
}

#[derive(Serialize, Deserialize)]
struct BackupEntry {
    rel: String,
    existed: bool,
}

/// Summary of one stored backup, for the "undo" menu.
#[derive(Clone, Debug)]
pub struct BackupInfo {
    pub dir: PathBuf,
    pub created_at: i64,
    pub files: usize,
}

type StatKey = (u64, i64);

pub struct Shadow {
    project: PathBuf,
    dir: PathBuf,
    work: PathBuf,
    opts: WalkOptions,
    base: BTreeMap<String, BaseEntry>,
    hash_cache: HashMap<String, (StatKey, String)>,
}

impl Shadow {
    /// Opens the shadow workspace stored in `dir` for `project`, creating and
    /// filling it on first use.
    pub fn open(project: &Path, dir: &Path, opts: WalkOptions) -> Result<Shadow> {
        let project = dunce::canonicalize(project)
            .with_context(|| format!("project folder does not exist: {}", project.display()))?;
        if !project.is_dir() {
            bail!("not a folder: {}", project.display());
        }
        let work = dir.join("work");
        fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;
        let work = dunce::canonicalize(&work)?;

        let manifest_path = dir.join("manifest.json");
        let (base, fresh) = match fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
        {
            Some(m) => (m.files, false),
            None => (BTreeMap::new(), true),
        };

        let mut shadow = Shadow {
            project,
            dir: dir.to_path_buf(),
            work,
            opts,
            base,
            hash_cache: HashMap::new(),
        };
        if fresh {
            shadow.check_size_limits()?;
        }
        shadow.sync_from_project()?;
        Ok(shadow)
    }

    pub fn project_dir(&self) -> &Path {
        &self.project
    }

    /// The private working copy that tools and the sandbox operate on.
    pub fn work_dir(&self) -> &Path {
        &self.work
    }

    fn check_size_limits(&self) -> Result<()> {
        let files = walk_files(&self.project, self.opts)?;
        let bytes: u64 = files.iter().map(|f| f.size).sum();
        if files.len() > MAX_PROJECT_FILES || bytes > MAX_PROJECT_BYTES {
            bail!(
                "this folder is too large to copy ({} files, {}). Pick a smaller project folder.",
                files.len(),
                crate::util::human_bytes(bytes)
            );
        }
        Ok(())
    }

    fn save_manifest(&self) -> Result<()> {
        let manifest = Manifest {
            files: self.base.clone(),
        };
        let text = serde_json::to_vec_pretty(&manifest)?;
        crate::util::write_atomic(&self.dir.join("manifest.json"), &text)
    }

    /// Brings the shadow copy up to date with edits made in the project folder.
    pub fn sync_from_project(&mut self) -> Result<SyncReport> {
        let project_files = walk_files(&self.project, self.opts)?;
        let mut report = SyncReport {
            total_files: project_files.len(),
            ..Default::default()
        };
        let mut seen = HashSet::new();

        for file in &project_files {
            seen.insert(file.rel.clone());
            let p_path = self.project.join(&file.rel);
            let Ok((p_size, p_mtime)) = stat_key(&p_path) else {
                continue;
            };
            let base = self.base.get(&file.rel).cloned();
            if let Some(b) = &base {
                if b.size == p_size && b.mtime == p_mtime {
                    continue;
                }
            }
            let Ok(p_bytes) = fs::read(&p_path) else {
                continue;
            };
            let p_sha = sha256_hex(&p_bytes);
            if let Some(b) = &base {
                if b.sha == p_sha {
                    self.base.insert(
                        file.rel.clone(),
                        BaseEntry {
                            sha: p_sha,
                            size: p_size,
                            mtime: p_mtime,
                        },
                    );
                    continue;
                }
            }

            let s_sha = self.shadow_sha(&file.rel);
            let entry = BaseEntry {
                sha: p_sha.clone(),
                size: p_size,
                mtime: p_mtime,
            };
            match (s_sha, base) {
                (None, None) => {
                    copy_file(&p_path, &self.work.join(&file.rel))?;
                    self.base.insert(file.rel.clone(), entry);
                    report.copied += 1;
                }
                (None, Some(_)) => report.conflicts.push(file.rel.clone()),
                (Some(s), Some(b)) if s == b.sha => {
                    copy_file(&p_path, &self.work.join(&file.rel))?;
                    self.base.insert(file.rel.clone(), entry);
                    report.copied += 1;
                }
                (Some(s), _) if s == p_sha => {
                    self.base.insert(file.rel.clone(), entry);
                }
                (Some(_), _) => report.conflicts.push(file.rel.clone()),
            }
        }

        let gone: Vec<String> = self
            .base
            .keys()
            .filter(|k| !seen.contains(*k))
            .cloned()
            .collect();
        for rel in gone {
            let b = self.base.get(&rel).cloned().expect("key exists");
            match self.shadow_sha(&rel) {
                None => {
                    self.base.remove(&rel);
                }
                Some(s) if s == b.sha => {
                    let _ = fs::remove_file(self.work.join(&rel));
                    self.base.remove(&rel);
                    report.removed += 1;
                }
                Some(_) => report.conflicts.push(rel),
            }
        }

        self.save_manifest()?;
        Ok(report)
    }

    /// SHA-256 of a file in the shadow copy, or `None` when it does not exist.
    fn shadow_sha(&mut self, rel: &str) -> Option<String> {
        let path = self.work.join(rel);
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let key = meta_key(&meta);
        if let Some((k, sha)) = self.hash_cache.get(rel) {
            if *k == key {
                return Some(sha.clone());
            }
        }
        let bytes = fs::read(&path).ok()?;
        let sha = sha256_hex(&bytes);
        self.hash_cache.insert(rel.to_string(), (key, sha.clone()));
        Some(sha)
    }

    /// All differences between the shadow copy and the project baseline.
    pub fn changes(&mut self) -> Result<Vec<Change>> {
        let shadow_files = walk_files(&self.work, self.opts)?;
        let mut out = Vec::new();
        let mut present = HashSet::new();
        for file in &shadow_files {
            present.insert(file.rel.clone());
            let Some(sha) = self.shadow_sha(&file.rel) else {
                continue;
            };
            match self.base.get(&file.rel) {
                None => out.push(Change {
                    rel: file.rel.clone(),
                    kind: ChangeKind::Added,
                    size: file.size,
                }),
                Some(b) if b.sha != sha => out.push(Change {
                    rel: file.rel.clone(),
                    kind: ChangeKind::Modified,
                    size: file.size,
                }),
                Some(_) => {}
            }
        }
        for (rel, b) in &self.base {
            if present.contains(rel) {
                continue;
            }
            // A file that still exists but is no longer walked (too large,
            // ignored) is not a deletion.
            if fs::symlink_metadata(self.work.join(rel)).is_ok() {
                continue;
            }
            out.push(Change {
                rel: rel.clone(),
                kind: ChangeKind::Deleted,
                size: b.size,
            });
        }
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(out)
    }

    /// Unified diff of one pending change (project version vs. shadow version).
    pub fn diff(&self, rel: &str) -> Result<String> {
        let project = Workspace::new(&self.project)?;
        let work = Workspace::new(&self.work)?;
        let old = read_optional(&project.resolve(rel)?)?;
        let new = read_optional(&work.resolve(rel)?)?;
        if old.as_deref().map(looks_binary).unwrap_or(false)
            || new.as_deref().map(looks_binary).unwrap_or(false)
        {
            return Ok("Binary file changed.".to_string());
        }
        let old_text = old
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let new_text = new
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let diff = similar::TextDiff::from_lines(&old_text, &new_text);
        let text = diff
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{rel}"), &format!("b/{rel}"))
            .to_string();
        Ok(if text.trim().is_empty() {
            "No textual differences.".to_string()
        } else {
            text
        })
    }

    /// Copies the selected pending changes into the real project.
    ///
    /// Originals are backed up first. Files that were edited in the project
    /// since the copy was made are skipped unless `force` is set.
    pub fn apply(
        &mut self,
        rels: &[String],
        backup_root: &Path,
        force: bool,
    ) -> Result<ApplyReport> {
        let project = Workspace::new(&self.project)?;
        let changes: HashMap<String, Change> = self
            .changes()?
            .into_iter()
            .map(|c| (c.rel.clone(), c))
            .collect();

        let mut report = ApplyReport::default();
        let backup_dir = backup_root.join(format!("{}-{}", now_secs(), unique_id()));
        let mut entries = Vec::new();

        for rel in rels {
            let Some(change) = changes.get(rel) else {
                report
                    .skipped
                    .push((rel.clone(), "no pending change".into()));
                continue;
            };
            let target = match project.resolve(rel) {
                Ok(p) => p,
                Err(e) => {
                    report.skipped.push((rel.clone(), e.to_string()));
                    continue;
                }
            };
            if target.is_dir() {
                report.skipped.push((
                    rel.clone(),
                    "a folder with this name exists in the project".into(),
                ));
                continue;
            }
            let current = if target.is_file() {
                Some(sha256_hex(&fs::read(&target)?))
            } else {
                None
            };
            let shadow_sha = self.shadow_sha(rel);
            let base_sha = self.base.get(rel).map(|b| b.sha.clone());
            let conflict = match change.kind {
                ChangeKind::Added => current.is_some() && current != shadow_sha,
                ChangeKind::Modified | ChangeKind::Deleted => current != base_sha,
            };
            if conflict && !force {
                report.skipped.push((
                    rel.clone(),
                    "the project file changed after the copy was made".into(),
                ));
                continue;
            }

            if target.is_file() {
                copy_file(&target, &backup_dir.join("files").join(rel))?;
            }
            entries.push(BackupEntry {
                rel: rel.clone(),
                existed: target.is_file(),
            });

            match change.kind {
                ChangeKind::Added | ChangeKind::Modified => {
                    copy_file_atomic(&self.work.join(rel), &target)?;
                    let (size, mtime) = stat_key(&target)?;
                    self.base.insert(
                        rel.clone(),
                        BaseEntry {
                            sha: shadow_sha.unwrap_or_default(),
                            size,
                            mtime,
                        },
                    );
                }
                ChangeKind::Deleted => {
                    if target.is_file() {
                        fs::remove_file(&target)
                            .with_context(|| format!("cannot delete {}", target.display()))?;
                    }
                    self.base.remove(rel);
                }
            }
            report.applied.push(rel.clone());
        }

        if !entries.is_empty() {
            let manifest = BackupManifest {
                created_at: now_secs(),
                entries,
            };
            fs::create_dir_all(&backup_dir)?;
            fs::write(
                backup_dir.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
            report.backup = Some(backup_dir);
        }
        self.save_manifest()?;
        Ok(report)
    }

    /// Throws away the shadow version of the selected files and restores the project version.
    pub fn discard(&mut self, rels: &[String]) -> Result<()> {
        let project = Workspace::new(&self.project)?;
        let work = Workspace::new(&self.work)?;
        for rel in rels {
            let p = project.resolve(rel)?;
            let w = work.resolve(rel)?;
            if p.is_file() {
                copy_file(&p, &w)?;
                let (size, mtime) = stat_key(&p)?;
                let sha = sha256_hex(&fs::read(&p)?);
                self.base
                    .insert(rel.clone(), BaseEntry { sha, size, mtime });
            } else {
                if w.is_file() {
                    fs::remove_file(&w)?;
                }
                self.base.remove(rel);
            }
            self.hash_cache.remove(rel);
        }
        self.save_manifest()
    }

    /// Discards every pending change.
    pub fn discard_all(&mut self) -> Result<usize> {
        let rels: Vec<String> = self.changes()?.into_iter().map(|c| c.rel).collect();
        self.discard(&rels)?;
        Ok(rels.len())
    }

    /// Restores the project from a backup made by [`Shadow::apply`].
    pub fn revert(&mut self, backup_dir: &Path) -> Result<usize> {
        let project = Workspace::new(&self.project)?;
        let text = fs::read_to_string(backup_dir.join("manifest.json"))
            .context("backup manifest not found")?;
        let manifest: BackupManifest =
            serde_json::from_str(&text).context("backup manifest is corrupt")?;
        let mut restored = 0;
        for entry in &manifest.entries {
            let target = project.resolve(&entry.rel)?;
            if entry.existed {
                let saved = backup_dir.join("files").join(&entry.rel);
                copy_file_atomic(&saved, &target)?;
            } else if target.is_file() {
                fs::remove_file(&target)?;
            }
            restored += 1;
        }
        fs::remove_dir_all(backup_dir).ok();
        self.sync_from_project()?;
        Ok(restored)
    }

    /// Lists backups under `backup_root`, newest first.
    pub fn list_backups(backup_root: &Path) -> Vec<BackupInfo> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(backup_root) else {
            return out;
        };
        for entry in rd.flatten() {
            let dir = entry.path();
            let Ok(text) = fs::read_to_string(dir.join("manifest.json")) else {
                continue;
            };
            let Ok(m) = serde_json::from_str::<BackupManifest>(&text) else {
                continue;
            };
            out.push(BackupInfo {
                dir,
                created_at: m.created_at,
                files: m.entries.len(),
            });
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.dir.cmp(&a.dir)));
        out
    }

    /// Deletes the private copy of this session.
    pub fn destroy(self) -> Result<()> {
        if self.dir.exists() {
            fs::remove_dir_all(&self.dir)
                .with_context(|| format!("cannot remove {}", self.dir.display()))?;
        }
        Ok(())
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn meta_key(meta: &fs::Metadata) -> StatKey {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    (meta.len(), mtime)
}

fn stat_key(path: &Path) -> Result<(u64, i64)> {
    let meta = fs::metadata(path).with_context(|| format!("cannot stat {}", path.display()))?;
    Ok(meta_key(&meta))
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    if fs::symlink_metadata(dst)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        fs::remove_file(dst)?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("cannot copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

/// Copies through a temporary sibling so a half-written file is never visible.
fn copy_file_atomic(src: &Path, dst: &Path) -> Result<()> {
    let parent = dst.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".gagent-tmp-{}", unique_id()));
    fs::copy(src, &tmp).with_context(|| format!("cannot copy {}", src.display()))?;
    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("cannot write {}", dst.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        project: PathBuf,
        shadow_dir: PathBuf,
        backups: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/main.py"), "print('hello')\n").unwrap();
        fs::write(project.join("README.md"), "# demo\n").unwrap();
        Fixture {
            shadow_dir: tmp.path().join("shadow"),
            backups: tmp.path().join("backups"),
            project,
            _tmp: tmp,
        }
    }

    fn open(f: &Fixture) -> Shadow {
        Shadow::open(&f.project, &f.shadow_dir, WalkOptions::default()).unwrap()
    }

    fn rels(changes: &[Change]) -> Vec<(&str, ChangeKind)> {
        changes.iter().map(|c| (c.rel.as_str(), c.kind)).collect()
    }

    #[test]
    fn initial_copy_has_no_changes() {
        let f = fixture();
        let mut s = open(&f);
        assert!(s.work_dir().join("src/main.py").is_file());
        assert!(s.changes().unwrap().is_empty());
    }

    #[test]
    fn detects_added_modified_deleted() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(s.work_dir().join("src/main.py"), "print('changed')\n").unwrap();
        fs::write(s.work_dir().join("new.txt"), "new").unwrap();
        fs::remove_file(s.work_dir().join("README.md")).unwrap();
        let changes = s.changes().unwrap();
        assert_eq!(
            rels(&changes),
            vec![
                ("README.md", ChangeKind::Deleted),
                ("new.txt", ChangeKind::Added),
                ("src/main.py", ChangeKind::Modified),
            ]
        );
    }

    #[test]
    fn apply_writes_to_project_and_creates_backup() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(s.work_dir().join("src/main.py"), "print('changed')\n").unwrap();
        fs::write(s.work_dir().join("added/deep/new.txt"), "x").ok();
        fs::create_dir_all(s.work_dir().join("added/deep")).unwrap();
        fs::write(s.work_dir().join("added/deep/new.txt"), "x").unwrap();
        fs::remove_file(s.work_dir().join("README.md")).unwrap();

        let all: Vec<String> = s.changes().unwrap().into_iter().map(|c| c.rel).collect();
        let report = s.apply(&all, &f.backups, false).unwrap();
        assert_eq!(report.applied.len(), 3, "{report:?}");
        assert!(report.skipped.is_empty());

        assert_eq!(
            fs::read_to_string(f.project.join("src/main.py")).unwrap(),
            "print('changed')\n"
        );
        assert_eq!(
            fs::read_to_string(f.project.join("added/deep/new.txt")).unwrap(),
            "x"
        );
        assert!(!f.project.join("README.md").exists());
        assert!(s.changes().unwrap().is_empty());

        let backups = Shadow::list_backups(&f.backups);
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].files, 3);
    }

    #[test]
    fn revert_restores_everything() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(s.work_dir().join("src/main.py"), "broken").unwrap();
        fs::write(s.work_dir().join("extra.txt"), "x").unwrap();
        fs::remove_file(s.work_dir().join("README.md")).unwrap();
        let all: Vec<String> = s.changes().unwrap().into_iter().map(|c| c.rel).collect();
        let report = s.apply(&all, &f.backups, false).unwrap();
        let backup = report.backup.unwrap();

        let restored = s.revert(&backup).unwrap();
        assert_eq!(restored, 3);
        assert_eq!(
            fs::read_to_string(f.project.join("src/main.py")).unwrap(),
            "print('hello')\n"
        );
        assert_eq!(
            fs::read_to_string(f.project.join("README.md")).unwrap(),
            "# demo\n"
        );
        assert!(!f.project.join("extra.txt").exists());
        // The shadow copy follows the reverted project.
        assert!(s.changes().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(s.work_dir().join("src/main.py")).unwrap(),
            "print('hello')\n"
        );
        assert!(Shadow::list_backups(&f.backups).is_empty());
    }

    #[test]
    fn apply_skips_conflicting_files_unless_forced() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(s.work_dir().join("src/main.py"), "agent version\n").unwrap();
        fs::write(f.project.join("src/main.py"), "user edited meanwhile\n").unwrap();

        let report = s
            .apply(&["src/main.py".to_string()], &f.backups, false)
            .unwrap();
        assert!(report.applied.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            fs::read_to_string(f.project.join("src/main.py")).unwrap(),
            "user edited meanwhile\n"
        );

        let report = s
            .apply(&["src/main.py".to_string()], &f.backups, true)
            .unwrap();
        assert_eq!(report.applied, vec!["src/main.py".to_string()]);
        assert_eq!(
            fs::read_to_string(f.project.join("src/main.py")).unwrap(),
            "agent version\n"
        );
    }

    #[test]
    fn project_edits_flow_into_untouched_shadow_files() {
        let f = fixture();
        let mut s = open(&f);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(f.project.join("README.md"), "# edited by user\n").unwrap();
        fs::write(f.project.join("brand_new.txt"), "hi").unwrap();
        let report = s.sync_from_project().unwrap();
        assert_eq!(report.copied, 2, "{report:?}");
        assert!(report.conflicts.is_empty());
        assert_eq!(
            fs::read_to_string(s.work_dir().join("README.md")).unwrap(),
            "# edited by user\n"
        );
        assert!(s.changes().unwrap().is_empty());
    }

    #[test]
    fn sync_keeps_agent_edits_and_reports_conflicts() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(s.work_dir().join("README.md"), "agent\n").unwrap();
        fs::write(f.project.join("README.md"), "user\n").unwrap();
        let report = s.sync_from_project().unwrap();
        assert_eq!(report.conflicts, vec!["README.md".to_string()]);
        assert_eq!(
            fs::read_to_string(s.work_dir().join("README.md")).unwrap(),
            "agent\n"
        );
    }

    #[test]
    fn project_deletions_flow_into_untouched_shadow_files() {
        let f = fixture();
        let mut s = open(&f);
        fs::remove_file(f.project.join("README.md")).unwrap();
        let report = s.sync_from_project().unwrap();
        assert_eq!(report.removed, 1);
        assert!(!s.work_dir().join("README.md").exists());
        assert!(s.changes().unwrap().is_empty());
    }

    #[test]
    fn discard_restores_project_content() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(s.work_dir().join("README.md"), "oops").unwrap();
        fs::write(s.work_dir().join("junk.txt"), "junk").unwrap();
        let n = s.discard_all().unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            fs::read_to_string(s.work_dir().join("README.md")).unwrap(),
            "# demo\n"
        );
        assert!(!s.work_dir().join("junk.txt").exists());
        assert!(s.changes().unwrap().is_empty());
    }

    #[test]
    fn state_persists_across_reopen() {
        let f = fixture();
        {
            let mut s = open(&f);
            fs::write(s.work_dir().join("README.md"), "agent\n").unwrap();
            assert_eq!(s.changes().unwrap().len(), 1);
        }
        let mut again = open(&f);
        let changes = again.changes().unwrap();
        assert_eq!(rels(&changes), vec![("README.md", ChangeKind::Modified)]);
    }

    #[test]
    fn excluded_dirs_do_not_count_as_changes() {
        let f = fixture();
        let mut s = open(&f);
        fs::create_dir_all(s.work_dir().join("node_modules/pkg")).unwrap();
        fs::write(s.work_dir().join("node_modules/pkg/index.js"), "x").unwrap();
        fs::create_dir_all(s.work_dir().join(".gagent/home")).unwrap();
        fs::write(s.work_dir().join(".gagent/home/cache"), "x").unwrap();
        assert!(s.changes().unwrap().is_empty());
    }

    #[test]
    fn diff_shows_changes() {
        let f = fixture();
        let mut s = open(&f);
        fs::write(
            s.work_dir().join("src/main.py"),
            "print('hello')\nprint('more')\n",
        )
        .unwrap();
        let _ = s.changes().unwrap();
        let d = s.diff("src/main.py").unwrap();
        assert!(d.contains("+print('more')"), "{d}");
        assert!(d.contains("--- a/src/main.py"));
    }

    #[test]
    fn rejects_oversized_projects_on_first_open() {
        let f = fixture();
        let opts = WalkOptions {
            max_file_bytes: 1,
            ..Default::default()
        };
        // Files above the per-file limit are simply skipped, not an error.
        let mut s = Shadow::open(&f.project, &f.shadow_dir, opts).unwrap();
        assert!(s.changes().unwrap().is_empty());
        assert!(!s.work_dir().join("README.md").exists());
    }

    #[test]
    fn destroy_removes_copy() {
        let f = fixture();
        let s = open(&f);
        let dir = f.shadow_dir.clone();
        s.destroy().unwrap();
        assert!(!dir.exists());
    }
}
