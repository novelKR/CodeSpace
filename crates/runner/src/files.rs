use codespace_domain::{ErrorBody, ErrorCode, FindResult, ReadResult};
use codespace_fs::{self, FsError};
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

    pub async fn version(&self, relative: &str) -> Result<String, ErrorBody> {
        let path = self.resolve(relative)?;
        match codespace_fs::metadata(&path).await {
            Ok(_) => {}
            Err(FsError::NotFound) => return Ok(VERSION_ABSENT.to_string()),
            Err(err) => return Err(fs_error_body(err)),
        }
        let bytes = codespace_fs::read(&path).await.map_err(fs_error_body)?;
        Ok(Self::version_of(&bytes))
    }

    pub async fn read_file(&self, relative: &str) -> Result<ReadResult, ErrorBody> {
        self.read_file_limited(relative, DEFAULT_READ_LIMIT).await
    }

    pub async fn read_file_limited(
        &self,
        relative: &str,
        limit: usize,
    ) -> Result<ReadResult, ErrorBody> {
        let path = self.resolve(relative)?;
        let meta = codespace_fs::metadata(&path).await.map_err(fs_error_body)?;
        if meta.is_symlink {
            return Err(ErrorBody::new(
                ErrorCode::SymlinkRejected,
                "symlink files are rejected",
            ));
        }
        if !meta.is_file {
            return Err(ErrorBody::new(
                ErrorCode::SpecialFileRejected,
                "not a regular file",
            ));
        }
        let bytes = codespace_fs::read(&path).await.map_err(fs_error_body)?;
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

    pub async fn find(&self, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        self.find_limited(glob, DEFAULT_FIND_LIMIT).await
    }

    pub async fn find_limited(
        &self,
        glob: Option<&str>,
        limit: usize,
    ) -> Result<FindResult, ErrorBody> {
        // Operator-registered `workspace.root` is the trust anchor. If the
        // registration path is itself a symlink, canonicalize resolves that
        // root only. Descendants under it are never followed.
        let root = self
            .root()
            .canonicalize()
            .map_err(|err| ErrorBody::new(ErrorCode::FileOperationFailed, err.to_string()))?;
        let walked = codespace_fs::walk_files(&root)
            .await
            .map_err(fs_error_body)?;
        let mut paths = Vec::new();
        for abs in walked.files {
            let rel = abs
                .strip_prefix(&root)
                .map_err(|_| ErrorBody::new(ErrorCode::PathEscape, "find left workspace"))?
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(pat) = glob.filter(|p| !p.is_empty()) {
                if !glob_match(pat, &rel) {
                    continue;
                }
            }
            paths.push(rel);
        }
        paths.sort();
        let truncated = walked.truncated || paths.len() > limit;
        if paths.len() > limit {
            paths.truncate(limit);
        }
        Ok(FindResult {
            paths,
            truncated,
            coordination: None,
        })
    }
}

pub(crate) fn fs_error_body(err: FsError) -> ErrorBody {
    match err {
        FsError::NotFound => ErrorBody::new(ErrorCode::FileNotFound, err.message()),
        FsError::NotDirectory => ErrorBody::new(ErrorCode::PathNotDirectory, err.message()),
        FsError::SymlinkRejected => ErrorBody::new(ErrorCode::SymlinkRejected, err.message()),
        FsError::NotRegularFile => ErrorBody::new(ErrorCode::SpecialFileRejected, err.message()),
        FsError::Io(msg) => ErrorBody::new(ErrorCode::FileOperationFailed, msg),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathSandbox;
    use codespace_domain::{ErrorCode, Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use tempfile::tempdir;

    fn sandbox(dir: &Path) -> PathSandbox {
        PathSandbox::new(Workspace::new(
            WorkspaceId("demo".into()),
            dir.to_path_buf(),
            Profile::ReadOnly,
        ))
    }

    #[tokio::test]
    async fn read_version_round_trip() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let s = sandbox(dir.path());
        let first = s.read_file("a.txt").await.unwrap();
        let second = s.read_file("a.txt").await.unwrap();
        assert_eq!(first.content, "hello");
        assert_eq!(first.version, second.version);
        assert!(first.version.starts_with("sha256:"));
        assert!(!first.truncated);
        assert_eq!(first.path, "a.txt");
        assert_eq!(s.version("missing").await.unwrap(), VERSION_ABSENT);
    }

    #[tokio::test]
    async fn oversized_read_sets_truncated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "abcdef").unwrap();
        let s = sandbox(dir.path());
        let result = s.read_file_limited("big.txt", 3).await.unwrap();
        assert_eq!(result.content, "abc");
        assert!(result.truncated);
        assert_eq!(result.version, PathSandbox::version_of(b"abcdef"));
    }

    #[tokio::test]
    async fn find_returns_relative_paths_and_skips_symlinks() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("a.rs"), "a").unwrap();
        symlink("/etc/passwd", dir.path().join("link")).unwrap();
        let s = sandbox(dir.path());
        let all = s.find(None).await.unwrap();
        assert_eq!(all.paths, vec!["a.rs".to_string(), "sub/b.txt".to_string()]);
        assert!(all.paths.iter().all(|p| !p.starts_with('/')));
        let rs = s.find(Some("*.rs")).await.unwrap();
        assert_eq!(rs.paths, vec!["a.rs".to_string()]);
        let limited = s.find_limited(None, 1).await.unwrap();
        assert!(limited.truncated);
        assert_eq!(limited.paths.len(), 1);
    }

    #[tokio::test]
    async fn read_rejects_path_through_directory_symlink() {
        let dir = tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "leak").unwrap();
        std::fs::create_dir(dir.path().join("ok")).unwrap();
        symlink(outside.path(), dir.path().join("via")).unwrap();
        let s = sandbox(dir.path());
        let err = s.read_file("via/secret.txt").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkRejected);
        let found = s.find(None).await.unwrap();
        assert!(!found.paths.iter().any(|p| p.contains("secret")));
    }

    #[tokio::test]
    async fn fs_error_body_matches_fs_error() {
        assert_eq!(
            fs_error_body(FsError::NotFound).code,
            ErrorCode::FileNotFound
        );
        assert_eq!(
            fs_error_body(FsError::NotDirectory).code,
            ErrorCode::PathNotDirectory
        );
        assert_eq!(
            fs_error_body(FsError::SymlinkRejected).code,
            ErrorCode::SymlinkRejected
        );
        assert_eq!(
            fs_error_body(FsError::NotRegularFile).code,
            ErrorCode::SpecialFileRejected
        );
        assert_eq!(
            fs_error_body(FsError::Io("disk full".into())).code,
            ErrorCode::FileOperationFailed
        );
    }

    #[tokio::test]
    async fn read_missing_file_is_file_not_found() {
        let dir = tempdir().unwrap();
        let s = sandbox(dir.path());
        let err = s.read_file("missing.txt").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::FileNotFound);
    }

    #[tokio::test]
    async fn read_through_regular_file_is_path_not_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("foo"), "not-a-dir").unwrap();
        let s = sandbox(dir.path());
        let err = s.read_file("foo/bar.txt").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::PathNotDirectory);
    }

    #[tokio::test]
    async fn read_dotdot_is_path_escape() {
        let dir = tempdir().unwrap();
        let s = sandbox(dir.path());
        let err = s.read_file("../outside").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::PathEscape);
    }

    #[tokio::test]
    async fn read_symlink_is_symlink_rejected() {
        let dir = tempdir().unwrap();
        symlink("/etc/passwd", dir.path().join("link")).unwrap();
        let s = sandbox(dir.path());
        let err = s.read_file("link").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkRejected);
    }

    #[tokio::test]
    async fn read_fifo_is_special_file_rejected() {
        let dir = tempdir().unwrap();
        let fifo = dir.path().join("pipe.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success(), "mkfifo should exist");
        let s = sandbox(dir.path());
        let err = s.read_file("pipe.fifo").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecialFileRejected);
    }

    #[tokio::test]
    async fn find_on_deleted_root_is_file_operation_failed() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let s = sandbox(&root);
        drop(dir);
        let err = s.find(None).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::FileOperationFailed);
    }
}
