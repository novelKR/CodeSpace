//! Compare helper-claimed after hashes against an independent disk read.

use std::collections::BTreeMap;

use codespace_domain::{ErrorBody, ErrorCode, FileChange, FileChangeKind};
use codespace_runner::{PathSandbox, VERSION_ABSENT};

pub fn verify_disk_matches_claimed(
    sandbox: &PathSandbox,
    claimed: &[FileChange],
    before: &BTreeMap<String, String>,
) -> Result<Vec<FileChange>, ErrorBody> {
    if claimed.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidPatch,
            "apply claimed no file changes",
        ));
    }
    let mut out = Vec::with_capacity(claimed.len());
    for change in claimed {
        let actual = sandbox.version(&change.path)?;
        match change.kind {
            FileChangeKind::Delete => {
                if actual != VERSION_ABSENT {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidPatch,
                        format!(
                            "apply claimed delete of {} but disk still has {actual}",
                            change.path
                        ),
                    ));
                }
            }
            _ => {
                let Some(claimed_after) = change.after_version.as_deref() else {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidPatch,
                        format!("apply omitted after_version for {}", change.path),
                    ));
                };
                if actual != claimed_after {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidPatch,
                        format!(
                            "disk version {actual} != claimed {claimed_after} for {}",
                            change.path
                        ),
                    ));
                }
            }
        }
        let before_version = before
            .get(&change.path)
            .cloned()
            .filter(|version| version != VERSION_ABSENT);
        let after_version = if actual == VERSION_ABSENT {
            None
        } else {
            Some(actual)
        };
        out.push(FileChange {
            path: change.path.clone(),
            before_version,
            after_version,
            kind: change.kind,
        });
    }
    Ok(out)
}

pub fn overlay_before(
    sandbox: &PathSandbox,
    changes: &[FileChange],
) -> Result<(Vec<FileChange>, BTreeMap<String, String>), ErrorBody> {
    let mut before = BTreeMap::new();
    let mut out = Vec::with_capacity(changes.len());
    for change in changes {
        let version = sandbox.version(&change.path)?;
        before.insert(change.path.clone(), version.clone());
        out.push(FileChange {
            path: change.path.clone(),
            before_version: (version != VERSION_ABSENT).then_some(version),
            after_version: change.after_version.clone(),
            kind: change.kind,
        });
    }
    Ok((out, before))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use tempfile::tempdir;

    fn sandbox(dir: &std::path::Path) -> PathSandbox {
        PathSandbox::new(Workspace {
            id: WorkspaceId("demo".into()),
            root: dir.to_path_buf(),
            profile: Profile::WorkspaceWrite,
        })
    }

    #[test]
    fn mismatching_claimed_hash_is_invalid() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let s = sandbox(dir.path());
        let actual = s.version("a.txt").unwrap();
        let err = verify_disk_matches_claimed(
            &s,
            &[FileChange {
                path: "a.txt".into(),
                before_version: None,
                after_version: Some("sha256:deadbeef".into()),
                kind: FileChangeKind::Add,
            }],
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPatch);
        assert!(err.message.contains(&actual));
    }

    #[test]
    fn matching_hash_is_applied() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let s = sandbox(dir.path());
        let actual = s.version("a.txt").unwrap();
        let changes = verify_disk_matches_claimed(
            &s,
            &[FileChange {
                path: "a.txt".into(),
                before_version: None,
                after_version: Some(actual.clone()),
                kind: FileChangeKind::Add,
            }],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(changes[0].after_version.as_deref(), Some(actual.as_str()));
    }
}
