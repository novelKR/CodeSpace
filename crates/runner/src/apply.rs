//! One runner-side patch transaction: versions, preflight, snapshot, apply, verify, rollback.

use codespace_domain::{ErrorBody, ErrorCode, FindResult, PatchStatus, ReadResult};
use codespace_policy::Workspace;

use crate::api::{RunnerApplyPatchRequest, RunnerApplyPatchResult};
use crate::process::InProcessRunner;
use crate::PathSandbox;

impl InProcessRunner {
    pub async fn read_file(&self, ws: &Workspace, path: &str) -> Result<ReadResult, ErrorBody> {
        ws.require_file_read()?;
        self.touch_watch(ws);
        PathSandbox::new(ws.clone()).read_file(path).await
    }

    pub async fn find_files(
        &self,
        ws: &Workspace,
        glob: Option<&str>,
    ) -> Result<FindResult, ErrorBody> {
        ws.require_file_read()?;
        self.touch_watch(ws);
        PathSandbox::new(ws.clone()).find(glob).await
    }

    pub async fn file_version(&self, ws: &Workspace, path: &str) -> Result<String, ErrorBody> {
        ws.require_file_read()?;
        self.touch_watch(ws);
        PathSandbox::new(ws.clone()).version(path).await
    }

    pub async fn apply_patch_txn(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, ErrorBody> {
        ws.require_file_write()?;
        self.touch_watch(ws);
        let sandbox = PathSandbox::new(ws.clone());
        for (path, expected) in &req.expected_versions {
            let actual = sandbox.version(path).await?;
            if &actual != expected {
                return Err(ErrorBody::new(
                    ErrorCode::VersionConflict,
                    format!("version conflict for {path}"),
                ));
            }
        }
        if req.check_only {
            let preview = crate::patch_helper::preflight(&ws.root, &req.patch).await?;
            let (changes, _) =
                crate::patch_verify::overlay_before(&sandbox, &preview.changes).await?;
            return Ok(RunnerApplyPatchResult {
                status: PatchStatus::Checked,
                files: preview.files,
                changes,
            });
        }
        let planned = crate::patch_helper::preflight(&ws.root, &req.patch).await?;
        let (_, before) = crate::patch_verify::overlay_before(&sandbox, &planned.changes).await?;
        let snaps = crate::rollback::snapshot(&sandbox, &planned.files).await?;
        match crate::patch_helper::apply(&ws.root, &req.patch, false).await {
            Ok(applied) => {
                let changes = crate::patch_verify::verify_disk_matches_claimed(
                    &sandbox,
                    &applied.changes,
                    &before,
                )
                .await?;
                Ok(RunnerApplyPatchResult {
                    status: PatchStatus::Applied,
                    files: applied.files,
                    changes,
                })
            }
            Err(_) => {
                let complete = crate::rollback::restore(&sandbox, &snaps).await;
                Ok(RunnerApplyPatchResult {
                    status: if complete {
                        PatchStatus::FailedRolledBack
                    } else {
                        PatchStatus::FailedPartial
                    },
                    files: planned.files,
                    changes: planned.changes,
                })
            }
        }
    }
}
