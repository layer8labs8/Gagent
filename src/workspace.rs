//! Safe access to a directory tree.
//!
//! Every path that comes from the model or the UI goes through
//! [`Workspace::resolve`]. It rejects absolute paths, `..` components and
//! symlinks, so a tool can never read or write outside the workspace root.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ignore::WalkBuilder;

use crate::util::to_slash;

/// Directory names that are never copied, listed, searched or synced.
pub const HARD_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".gagent",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".gradle",
    ".next",
    ".nuxt",
    ".cache",
];

/// File names that are never copied.
const HARD_EXCLUDED_FILES: &[&str] = &[".DS_Store", "Thumbs.db"];

/// Largest file that is copied into the shadow workspace.
pub const MAX_SYNC_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// The sandbox mounts the workspace here.
pub const SANDBOX_MOUNT: &str = "/workspace";

/// Options controlling which files a walk reports.
#[derive(Clone, Copy, Debug)]
pub struct WalkOptions {
    /// Include files that commonly hold secrets (`.env`, private keys).
    pub include_secrets: bool,
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            include_secrets: false,
            max_file_bytes: MAX_SYNC_FILE_BYTES,
        }
    }
}

/// One file found by [`walk_files`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileEntry {
    /// Forward-slash path relative to the walked root.
    pub rel: String,
    pub size: u64,
}

/// Returns true for files that commonly contain credentials.
pub fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == ".env"
        || (lower.starts_with(".env.")
            && !lower.ends_with(".example")
            && !lower.ends_with(".sample")
            && !lower.ends_with(".template"))
    {
        return true;
    }
    if lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
    {
        return true;
    }
    [".pem", ".key", ".pfx", ".p12", ".keystore", ".jks", ".kdbx"]
        .iter()
        .any(|ext| lower.ends_with(ext))
        || lower == ".npmrc"
        || lower == ".pypirc"
        || lower == ".netrc"
}

/// A directory that all file access is confined to.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Opens a workspace; the directory must exist.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let canonical = dunce::canonicalize(root)
            .with_context(|| format!("folder does not exist: {}", root.display()))?;
        if !canonical.is_dir() {
            bail!("not a folder: {}", canonical.display());
        }
        Ok(Self { root: canonical })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Turns a user/model supplied relative path into a safe absolute path.
    ///
    /// The path does not have to exist. Symlinks anywhere along the path are
    /// refused because a sandboxed process could plant a link that points to
    /// a file on the host.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let rel = normalize_rel(rel)?;
        let mut current = self.root.clone();
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    bail!("'{rel}' passes through a symbolic link, which is not allowed");
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(anyhow!("cannot access '{rel}': {e}")),
            }
        }
        Ok(current)
    }

    /// Relative forward-slash form of an absolute path inside the workspace.
    pub fn relative(&self, abs: &Path) -> String {
        match abs.strip_prefix(&self.root) {
            Ok(rel) => to_slash(rel),
            Err(_) => to_slash(abs),
        }
    }
}

/// Validates and normalizes a relative path. Returns it with `/` separators,
/// no leading or trailing slash, and `""` for the workspace root.
pub fn normalize_rel(raw: &str) -> Result<String> {
    if raw.contains('\0') {
        bail!("path contains a NUL byte");
    }
    let mut text = raw.trim().replace('\\', "/");

    // The sandbox shows the workspace as /workspace; accept that spelling.
    if text == SANDBOX_MOUNT {
        text.clear();
    } else if let Some(rest) = text.strip_prefix("/workspace/") {
        text = rest.to_string();
    }

    let bytes = text.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    if text.starts_with('/') || text == "~" || text.starts_with("~/") || has_drive {
        bail!(
            "absolute paths are not allowed ('{raw}'); use a path relative to the workspace root"
        );
    }

    let mut parts = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("'..' is not allowed in paths ('{raw}')"),
            p => {
                if cfg!(windows) {
                    if let Some(problem) = windows_name_problem(p) {
                        bail!("'{p}' cannot be used as a file name on Windows: {problem}");
                    }
                }
                parts.push(p)
            }
        }
    }
    Ok(parts.join("/"))
}

/// Explains why `name` is not a valid file or folder name on Windows, if it is not.
///
/// On Windows such names are dangerous rather than just invalid: `CON` and `NUL`
/// open devices, and `name:stream` writes to a hidden NTFS data stream.
pub fn windows_name_problem(name: &str) -> Option<&'static str> {
    if name
        .chars()
        .any(|c| c.is_control() || "<>:\"|?*".contains(c))
    {
        return Some("it contains one of the characters < > : \" | ? *");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("it ends with a dot or a space");
    }
    let stem = name.split('.').next().unwrap_or("").trim_end();
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| {
            upper
                .strip_prefix(p)
                .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
        });
    if reserved {
        return Some("it is a reserved device name");
    }
    None
}

/// Lists all regular files below `root` that Gagent is willing to handle.
///
/// Respects `.gitignore` files inside the tree, skips the hard-excluded
/// directories, symlinks, oversized files and (optionally) secret files.
pub fn walk_files(root: &Path, opts: WalkOptions) -> Result<Vec<FileEntry>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_exclude(false)
        .git_ignore(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.depth() == 0 {
                return true;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                !HARD_EXCLUDED_DIRS.contains(&name.as_ref())
            } else {
                !HARD_EXCLUDED_FILES.contains(&name.as_ref())
            }
        });

    let mut files = Vec::new();
    for item in builder.build() {
        let entry = match item {
            Ok(e) => e,
            Err(_) => continue,
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !opts.include_secrets && is_sensitive_name(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > opts.max_file_bytes {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        files.push(FileEntry {
            rel: to_slash(rel),
            size: meta.len(),
        });
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn resolves_plain_relative_paths() {
        let (_d, ws) = ws();
        let p = ws.resolve("src/main.rs").unwrap();
        assert!(p.starts_with(ws.root()));
        assert!(p.ends_with("src/main.rs"));
        assert_eq!(ws.resolve("").unwrap(), ws.root());
        assert_eq!(ws.resolve(".").unwrap(), ws.root());
        assert_eq!(
            ws.resolve("./a//b/").unwrap(),
            ws.root().join("a").join("b")
        );
    }

    #[test]
    fn accepts_sandbox_spelling() {
        let (_d, ws) = ws();
        assert_eq!(ws.resolve("/workspace").unwrap(), ws.root());
        assert!(ws
            .resolve("/workspace/a/b.txt")
            .unwrap()
            .ends_with("a/b.txt"));
    }

    #[test]
    fn rejects_escapes() {
        let (_d, ws) = ws();
        for bad in [
            "../x",
            "a/../../x",
            "a/..",
            "/etc/passwd",
            "C:\\Windows\\system32",
            "c:/x",
            "~/secret",
            "\\\\server\\share",
            "a\0b",
        ] {
            assert!(ws.resolve(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        let (_d, ws) = ws();
        std::os::unix::fs::symlink("/etc", ws.root().join("link")).unwrap();
        assert!(ws.resolve("link").is_err());
        assert!(ws.resolve("link/passwd").is_err());
        std::os::unix::fs::symlink("/etc/passwd", ws.root().join("file_link")).unwrap();
        assert!(ws.resolve("file_link").is_err());
    }

    #[test]
    fn walk_skips_excluded_things() {
        let (d, ws) = ws();
        let root = ws.root();
        for (p, c) in [
            ("src/a.py", "print(1)"),
            ("node_modules/x/index.js", "x"),
            (".git/config", "x"),
            ("__pycache__/a.pyc", "x"),
            (".env", "SECRET=1"),
            (".env.example", "SECRET="),
            ("keys/id_rsa", "x"),
            ("README.md", "# hi"),
            ("ignored.log", "x"),
            (".gitignore", "*.log\n"),
        ] {
            let full = root.join(p);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, c).unwrap();
        }
        let names: Vec<_> = walk_files(root, WalkOptions::default())
            .unwrap()
            .into_iter()
            .map(|f| f.rel)
            .collect();
        assert_eq!(
            names,
            vec![".env.example", ".gitignore", "README.md", "src/a.py"]
        );
        let with_secrets: Vec<_> = walk_files(
            root,
            WalkOptions {
                include_secrets: true,
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|f| f.rel)
        .collect();
        assert!(with_secrets.contains(&".env".to_string()));
        assert!(with_secrets.contains(&"keys/id_rsa".to_string()));
        drop(d);
    }

    #[test]
    fn walk_skips_large_files() {
        let (_d, ws) = ws();
        fs::write(ws.root().join("big.bin"), vec![1u8; 2000]).unwrap();
        fs::write(ws.root().join("small.txt"), "x").unwrap();
        let found = walk_files(
            ws.root(),
            WalkOptions {
                max_file_bytes: 1000,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rel, "small.txt");
    }

    #[test]
    fn windows_names_are_checked() {
        for bad in [
            "con", "NUL", "Aux.txt", "com1", "LPT9.log", "a:b", "a?b", "x ", "x.", "a|b", "q\"",
        ] {
            assert!(
                windows_name_problem(bad).is_some(),
                "{bad} should be rejected"
            );
        }
        for good in [
            "main.rs",
            "console.log",
            "com10",
            "comm",
            ".env",
            "a b.txt",
            "~tmp",
        ] {
            assert!(
                windows_name_problem(good).is_none(),
                "{good} should be fine"
            );
        }
    }

    #[test]
    fn tilde_only_means_home_at_the_start() {
        assert!(normalize_rel("~").is_err());
        assert!(normalize_rel("~/x").is_err());
        assert_eq!(normalize_rel("~backup.txt").unwrap(), "~backup.txt");
    }

    #[test]
    fn sensitive_names() {
        assert!(is_sensitive_name(".env"));
        assert!(is_sensitive_name(".env.local"));
        assert!(!is_sensitive_name(".env.example"));
        assert!(is_sensitive_name("server.pem"));
        assert!(is_sensitive_name("id_rsa"));
        assert!(!is_sensitive_name("main.rs"));
    }
}
