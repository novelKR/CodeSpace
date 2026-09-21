use codespace_domain::{
    ErrorBody, ErrorCode, FindResult, ReadResult, DEFAULT_FIND_LIMIT, DEFAULT_READ_LIMIT,
};
use codespace_fs::{self, FsError};
use sha2::{Digest, Sha256};

use crate::PathSandbox;

pub const VERSION_ABSENT: &str = "absent";

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
        self.read_file_window(relative, None, None).await
    }

    pub async fn read_file_window(
        &self,
        relative: &str,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ReadResult, ErrorBody> {
        let limit = resolve_limit(limit, DEFAULT_READ_LIMIT, "read")?;
        let offset = offset.unwrap_or(0);
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
        let (slice, truncated) = byte_window(&bytes, offset, limit);
        let content_lossy = std::str::from_utf8(slice).is_err();
        Ok(ReadResult {
            path: normalize_rel(relative),
            content: String::from_utf8_lossy(slice).into_owned(),
            version: Self::version_of(&bytes),
            truncated,
            offset,
            byte_count: slice.len() as u64,
            content_lossy,
            next_offset: truncated.then_some(offset + slice.len() as u64),
            coordination: None,
        })
    }

    pub async fn find(&self, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        self.find_window(glob, None, None).await
    }

    pub async fn find_window(
        &self,
        glob: Option<&str>,
        offset: Option<u64>,
        limit: Option<u32>,
    ) -> Result<FindResult, ErrorBody> {
        let limit = resolve_limit(limit, DEFAULT_FIND_LIMIT, "find")?;
        let offset = offset.unwrap_or(0);
        // Operator-registered `workspace.root` is the trust anchor. If the
        // registration path is itself a symlink, canonicalize resolves that
        // root only. Descendants under it are never followed.
        let root = self
            .root()
            .canonicalize()
            .map_err(|err| fs_error_body(FsError::Io(err.to_string())))?;
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
        let page = path_window(&paths, offset, limit, walked.truncated);
        Ok(FindResult {
            paths: page.paths,
            truncated: page.truncated,
            offset,
            incomplete: page.incomplete,
            listing_version: listing_version(&paths, page.incomplete),
            next_offset: page.next_offset,
            coordination: None,
        })
    }
}

fn resolve_limit(requested: Option<u32>, max: u32, what: &str) -> Result<usize, ErrorBody> {
    let limit = requested.unwrap_or(max);
    if limit == 0 || limit > max {
        return Err(ErrorBody::new(
            ErrorCode::OutputLimit,
            format!("{what} limit must be between 1 and {max}"),
        ));
    }
    Ok(limit as usize)
}

fn byte_window(bytes: &[u8], offset: u64, limit: usize) -> (&[u8], bool) {
    let len = bytes.len() as u64;
    if offset >= len {
        return (&[], false);
    }
    let start = offset as usize;
    let end = start.saturating_add(limit).min(bytes.len());
    (&bytes[start..end], (end as u64) < len)
}

struct PathPage {
    paths: Vec<String>,
    truncated: bool,
    incomplete: bool,
    next_offset: Option<u64>,
}

fn listing_version(paths: &[String], incomplete: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(if incomplete {
        b"incomplete\0".as_slice()
    } else {
        b"complete\0".as_slice()
    });
    for path in paths {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn path_window(paths: &[String], offset: u64, limit: usize, walk_truncated: bool) -> PathPage {
    let incomplete = walk_truncated;
    let len = paths.len() as u64;
    if offset >= len {
        return PathPage {
            paths: Vec::new(),
            truncated: false,
            incomplete,
            next_offset: None,
        };
    }
    let start = offset as usize;
    let remaining = paths.len() - start;
    let truncated = remaining > limit;
    let end = start.saturating_add(limit).min(paths.len());
    let page = paths[start..end].to_vec();
    PathPage {
        next_offset: truncated.then_some(offset + page.len() as u64),
        paths: page,
        truncated,
        incomplete,
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
        assert_eq!(first.offset, 0);
        assert_eq!(first.byte_count, 5);
        assert!(!first.content_lossy);
        assert!(first.next_offset.is_none());
        assert_eq!(s.version("missing").await.unwrap(), VERSION_ABSENT);
    }

    #[tokio::test]
    async fn oversized_read_sets_truncated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "abcdef").unwrap();
        let s = sandbox(dir.path());
        let result = s.read_file_window("big.txt", None, Some(3)).await.unwrap();
        assert_eq!(result.content, "abc");
        assert!(result.truncated);
        assert_eq!(result.offset, 0);
        assert_eq!(result.byte_count, 3);
        assert!(!result.content_lossy);
        assert_eq!(result.next_offset, Some(3));
        assert_eq!(result.version, PathSandbox::version_of(b"abcdef"));
        let rest = s
            .read_file_window("big.txt", result.next_offset, Some(3))
            .await
            .unwrap();
        assert_eq!(rest.content, "def");
        assert!(!rest.truncated);
        assert_eq!(rest.offset, 3);
        assert_eq!(rest.byte_count, 3);
        assert!(!rest.content_lossy);
        assert!(rest.next_offset.is_none());
        assert_eq!(rest.version, result.version);
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
        let limited = s.find_window(None, None, Some(1)).await.unwrap();
        assert!(limited.truncated);
        assert!(!limited.incomplete);
        assert_eq!(limited.paths.len(), 1);
        assert_eq!(limited.offset, 0);
        assert_eq!(limited.next_offset, Some(1));
        assert!(limited.listing_version.starts_with("sha256:"));
        let page = s
            .find_window(None, limited.next_offset, Some(1))
            .await
            .unwrap();
        assert!(!page.truncated);
        assert!(!page.incomplete);
        assert_eq!(page.offset, 1);
        assert_eq!(page.paths.len(), 1);
        assert!(page.next_offset.is_none());
        assert_eq!(page.listing_version, limited.listing_version);
        assert_ne!(page.paths, limited.paths);
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

    #[tokio::test]
    async fn read_window_past_eof_is_empty() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let s = sandbox(dir.path());
        let result = s.read_file_window("a.txt", Some(10), None).await.unwrap();
        assert_eq!(result.content, "");
        assert!(!result.truncated);
        assert_eq!(result.offset, 10);
        assert_eq!(result.byte_count, 0);
        assert!(!result.content_lossy);
        assert!(result.next_offset.is_none());
        assert_eq!(result.version, PathSandbox::version_of(b"hi"));
    }

    #[tokio::test]
    async fn read_limit_zero_or_above_cap_is_output_limit() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let s = sandbox(dir.path());
        let zero = s
            .read_file_window("a.txt", None, Some(0))
            .await
            .unwrap_err();
        assert_eq!(zero.code, ErrorCode::OutputLimit);
        let over = s
            .read_file_window("a.txt", None, Some(DEFAULT_READ_LIMIT + 1))
            .await
            .unwrap_err();
        assert_eq!(over.code, ErrorCode::OutputLimit);
    }

    #[tokio::test]
    async fn find_limit_zero_or_above_cap_is_output_limit() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let s = sandbox(dir.path());
        let zero = s.find_window(None, None, Some(0)).await.unwrap_err();
        assert_eq!(zero.code, ErrorCode::OutputLimit);
        let over = s
            .find_window(None, None, Some(DEFAULT_FIND_LIMIT + 1))
            .await
            .unwrap_err();
        assert_eq!(over.code, ErrorCode::OutputLimit);
    }

    #[tokio::test]
    async fn read_lossy_utf8_reports_file_byte_count() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("bin"), [0x61, 0xff, 0x62]).unwrap();
        let s = sandbox(dir.path());
        let first = s.read_file_window("bin", None, Some(2)).await.unwrap();
        assert!(first.truncated);
        assert!(first.content_lossy);
        assert_eq!(first.byte_count, 2);
        assert_eq!(first.next_offset, Some(2));
        assert_ne!(first.content.len() as u64, first.byte_count);
        let rest = s
            .read_file_window("bin", first.next_offset, Some(2))
            .await
            .unwrap();
        assert_eq!(rest.content, "b");
        assert!(!rest.truncated);
        assert!(!rest.content_lossy);
        assert_eq!(rest.byte_count, 1);
        assert!(rest.next_offset.is_none());
        assert_eq!(rest.version, first.version);
    }

    #[tokio::test]
    async fn read_window_split_utf8() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("hangul.txt"), "가").unwrap();
        let s = sandbox(dir.path());
        let first = s
            .read_file_window("hangul.txt", None, Some(1))
            .await
            .unwrap();
        assert!(first.content_lossy);
        assert_eq!(first.byte_count, 1);
        assert_eq!(first.next_offset, Some(1));
        assert_eq!(first.version, PathSandbox::version_of("가".as_bytes()));
    }

    #[test]
    fn find_walk_truncated_cannot_return_retry_loop() {
        let paths = vec!["a".into(), "b".into()];
        let page = path_window(&paths, 2, 10, true);
        assert!(page.paths.is_empty());
        assert!(!page.truncated);
        assert!(page.incomplete);
        assert!(page.next_offset.is_none());
    }

    #[test]
    fn listing_version_includes_completeness() {
        let paths = vec!["a.rs".into()];
        assert_ne!(
            listing_version(&paths, false),
            listing_version(&paths, true)
        );
        assert!(listing_version(&paths, false).starts_with("sha256:"));
    }

    #[tokio::test]
    async fn find_listing_version_changes_between_pages() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "a").unwrap();
        std::fs::write(dir.path().join("b.rs"), "b").unwrap();
        let s = sandbox(dir.path());
        let first = s.find_window(None, None, Some(1)).await.unwrap();
        assert_eq!(first.paths, vec!["a.rs".to_string()]);
        assert_eq!(first.next_offset, Some(1));
        std::fs::write(dir.path().join("0.rs"), "z").unwrap();
        let second = s
            .find_window(None, first.next_offset, Some(1))
            .await
            .unwrap();
        assert_ne!(second.listing_version, first.listing_version);
    }
}
