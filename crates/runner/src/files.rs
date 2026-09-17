use std::fs;
use std::path::Path;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ReadResult};
use sha2::{Digest, Sha256};

use crate::PathSandbox;

pub const VERSION_ABSENT: &str = "absent";
pub const DEFAULT_READ_LIMIT: usize = 1024 * 1024;
pub const DEFAULT_FIND_LIMIT: usize = 10_000;

impl PathSandbox {
    pub fn version_of(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }

    pub fn version(&self, relative: &str) -> Result<String, ErrorBody> {
        let path = self.resolve(relative)?;
        if !path.exists() {
            return Ok(VERSION_ABSENT.to_string());
        }
        let bytes = fs::read(&path).map_err(io_err)?;
        Ok(Self::version_of(&bytes))
    }

    pub fn read_file(&self, relative: &str) -> Result<ReadResult, ErrorBody> {
        self.read_file_limited(relative, DEFAULT_READ_LIMIT)
    }

    pub fn read_file_limited(&self, relative: &str, limit: usize) -> Result<ReadResult, ErrorBody> {
        let path = self.resolve(relative)?;
        let meta = fs::symlink_metadata(&path).map_err(io_err)?;
        if !meta.file_type().is_file() {
            return Err(ErrorBody::new(
                ErrorCode::SpecialFileRejected,
                "not a regular file",
            ));
        }
        let bytes = fs::read(&path).map_err(io_err)?;
        let truncated = bytes.len() > limit;
        if truncated && limit == 0 {
            return Err(ErrorBody::new(
                ErrorCode::OutputLimit,
                "read output limit is zero",
            ));
        }
        let slice = if truncated { &bytes[..limit] } else { &bytes };
        Ok(ReadResult {
            path: normalize_rel(relative),
            content: String::from_utf8_lossy(slice).into_owned(),
            version: Self::version_of(&bytes),
            truncated,
            coordination: None,
        })
    }

    pub fn find(&self, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        self.find_limited(glob, DEFAULT_FIND_LIMIT)
    }

    pub fn find_limited(&self, glob: Option<&str>, limit: usize) -> Result<FindResult, ErrorBody> {
        let mut paths = Vec::new();
        let mut truncated = false;
        visit(
            self.root(),
            self.root(),
            glob,
            limit,
            &mut paths,
            &mut truncated,
        )?;
        paths.sort();
        Ok(FindResult {
            paths,
            truncated,
            coordination: None,
        })
    }
}

fn visit(
    root: &Path,
    dir: &Path,
    glob: Option<&str>,
    limit: usize,
    out: &mut Vec<String>,
    truncated: &mut bool,
) -> Result<(), ErrorBody> {
    let entries = fs::read_dir(dir).map_err(io_err)?;
    for entry in entries {
        if out.len() >= limit {
            *truncated = true;
            return Ok(());
        }
        let entry = entry.map_err(io_err)?;
        let path = entry.path();
        let ft = entry.file_type().map_err(io_err)?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            visit(root, &path, glob, limit, out, truncated)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| ErrorBody::new(ErrorCode::PathEscape, "find left workspace"))?
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(pat) = glob.filter(|p| !p.is_empty()) {
            if !glob_match(pat, &rel) {
                continue;
            }
        }
        out.push(rel);
    }
    Ok(())
}

fn glob_match(pat: &str, path: &str) -> bool {
    if matches!(pat, "*" | "**" | "**/*") {
        return true;
    }
    if let Some(ext) = pat.strip_prefix("*.") {
        return path.ends_with(&format!(".{ext}"));
    }
    if !pat.contains('*') {
        return path == pat || path.ends_with(&format!("/{pat}"));
    }
    let needle = pat.replace('*', "");
    needle.is_empty() || path.contains(&needle)
}

fn normalize_rel(relative: &str) -> String {
    relative.replace('\\', "/")
}

fn io_err(err: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::PathEscape, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathSandbox;
    use codespace_domain::{Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use std::path::Path;
    use tempfile::tempdir;

    fn sandbox(dir: &Path) -> PathSandbox {
        PathSandbox::new(Workspace {
            id: WorkspaceId("demo".into()),
            root: dir.to_path_buf(),
            profile: Profile::ReadOnly,
        })
    }

    #[test]
    fn read_version_round_trip() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let s = sandbox(dir.path());
        let first = s.read_file("a.txt").unwrap();
        let second = s.read_file("a.txt").unwrap();
        assert_eq!(first.content, "hello");
        assert_eq!(first.version, second.version);
        assert!(first.version.starts_with("sha256:"));
        assert!(!first.truncated);
        assert_eq!(first.path, "a.txt");
        assert_eq!(s.version("missing").unwrap(), VERSION_ABSENT);
    }

    #[test]
    fn oversized_read_sets_truncated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "abcdef").unwrap();
        let s = sandbox(dir.path());
        let result = s.read_file_limited("big.txt", 3).unwrap();
        assert_eq!(result.content, "abc");
        assert!(result.truncated);
        assert_eq!(result.version, PathSandbox::version_of(b"abcdef"));
    }

    #[test]
    fn find_returns_relative_paths_and_skips_symlinks() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("a.rs"), "a").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();
        let s = sandbox(dir.path());
        let all = s.find(None).unwrap();
        assert_eq!(all.paths, vec!["a.rs".to_string(), "sub/b.txt".to_string()]);
        assert!(all.paths.iter().all(|p| !p.starts_with('/')));
        let rs = s.find(Some("*.rs")).unwrap();
        assert_eq!(rs.paths, vec!["a.rs".to_string()]);
        let limited = s.find_limited(None, 1).unwrap();
        assert!(limited.truncated);
        assert_eq!(limited.paths.len(), 1);
    }
}
