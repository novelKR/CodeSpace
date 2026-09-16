//! Workspace registry and path policy. No `rmcp` types.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use codespace_domain::{ErrorBody, ErrorCode, Profile, WorkspaceId};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    Exec,
}

/// Client-supplied authorization theatre. Always ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientClaims {
    #[serde(default)]
    pub approved: Option<bool>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub root: PathBuf,
    pub profile: Profile,
}

#[derive(Debug, Clone, Default)]
pub struct Registry {
    workspaces: BTreeMap<String, Workspace>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    workspaces: BTreeMap<String, FileWorkspace>,
}

#[derive(Debug, Deserialize)]
struct FileWorkspace {
    root: String,
    #[serde(default)]
    profile: Profile,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, workspace: Workspace) {
        self.workspaces.insert(workspace.id.0.clone(), workspace);
    }

    pub fn get(&self, id: &str) -> Result<&Workspace, ErrorBody> {
        self.workspaces.get(id).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::WorkspaceNotFound,
                format!("unknown workspace_id `{id}`"),
            )
        })
    }

    pub fn load_path(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::load_json(&text)
    }

    pub fn load_json(text: &str) -> Result<Self, String> {
        let parsed: FileConfig = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut registry = Registry::new();
        for (id, entry) in parsed.workspaces {
            let root = PathBuf::from(&entry.root);
            if !root.is_absolute() {
                return Err(format!("workspace `{id}` root must be absolute"));
            }
            registry.insert(Workspace {
                id: WorkspaceId(id),
                root,
                profile: entry.profile,
            });
        }
        Ok(registry)
    }

    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }
}

/// `approved` / `user_id` never grant rights. Profile does.
pub fn allow(
    workspace: &Workspace,
    action: Action,
    claims: &ClientClaims,
) -> Result<(), ErrorBody> {
    let _ = claims;
    match action {
        Action::Read => Ok(()),
        Action::Write | Action::Exec => {
            if workspace.profile.allows_mutation() {
                Ok(())
            } else {
                Err(ErrorBody::new(
                    ErrorCode::Unauthorized,
                    "workspace profile is read-only",
                ))
            }
        }
    }
}

/// Resolve a model-supplied path inside a registered workspace.
/// Relative paths only. `..` that would leave the root is rejected.
/// Absolute paths and paths that land in another workspace root are rejected.
pub fn resolve_path(workspace: &Workspace, relative: &str) -> Result<PathBuf, ErrorBody> {
    let input = Path::new(relative);
    if relative.is_empty() || input.is_absolute() {
        return Err(escape("absolute or empty path"));
    }
    if relative.chars().any(|c| c == '\0') {
        return Err(escape("NUL in path"));
    }

    let mut rel = PathBuf::new();
    for component in input.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => rel.push(part),
            Component::ParentDir => {
                if !rel.pop() {
                    return Err(escape("`..` escapes the workspace"));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(escape("absolute path"));
            }
        }
    }

    let root = workspace
        .root
        .canonicalize()
        .unwrap_or_else(|_| workspace.root.clone());
    let joined = root.join(&rel);
    if !joined.starts_with(&root) {
        return Err(escape("resolved path left the workspace"));
    }
    Ok(joined)
}

fn escape(detail: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::PathEscape, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn demo(dir: &Path, profile: Profile) -> Workspace {
        Workspace {
            id: WorkspaceId("demo".into()),
            root: dir.to_path_buf(),
            profile,
        }
    }

    #[test]
    fn unknown_workspace_is_rejected() {
        let registry = Registry::new();
        let err = registry.get("nope").unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkspaceNotFound);
    }

    #[test]
    fn default_profile_is_read_only() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo"}}}"#;
        let registry = Registry::load_json(json).unwrap();
        assert_eq!(registry.get("demo").unwrap().profile, Profile::ReadOnly);
    }

    #[test]
    fn host_admin_is_not_a_profile() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","profile":"host-admin"}}}"#;
        assert!(Registry::load_json(json).is_err());
    }

    #[test]
    fn approved_true_does_not_unlock_read_only() {
        let dir = tempdir().unwrap();
        let ws = demo(dir.path(), Profile::ReadOnly);
        let claims = ClientClaims {
            approved: Some(true),
            user_id: Some("root".into()),
        };
        assert_eq!(
            allow(&ws, Action::Write, &claims).unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert!(allow(&ws, Action::Read, &claims).is_ok());
    }

    #[test]
    fn workspace_write_allows_mutation() {
        let dir = tempdir().unwrap();
        let ws = demo(dir.path(), Profile::WorkspaceWrite);
        assert!(allow(&ws, Action::Write, &ClientClaims::default()).is_ok());
    }

    #[test]
    fn rejects_absolute_and_dotdot_and_other_root() {
        let dir = tempdir().unwrap();
        let other = tempdir().unwrap();
        let ws = demo(dir.path(), Profile::ReadOnly);

        assert_eq!(
            resolve_path(&ws, "/etc/passwd").unwrap_err().code,
            ErrorCode::PathEscape
        );
        assert_eq!(
            resolve_path(&ws, "../outside").unwrap_err().code,
            ErrorCode::PathEscape
        );
        assert_eq!(
            resolve_path(&ws, "ok/../../etc").unwrap_err().code,
            ErrorCode::PathEscape
        );
        let foreign = other.path().to_string_lossy().to_string();
        assert_eq!(
            resolve_path(&ws, &foreign).unwrap_err().code,
            ErrorCode::PathEscape
        );

        let ok = resolve_path(&ws, "src/lib.rs").unwrap();
        let canon = dir.path().canonicalize().unwrap();
        assert!(ok.starts_with(&canon));
        assert!(ok.ends_with("src/lib.rs"));
    }

    #[test]
    fn model_cannot_register_a_workspace() {
        let mut registry = Registry::new();
        assert!(registry.get("sneaky").is_err());
        registry.insert(Workspace {
            id: WorkspaceId("sneaky".into()),
            root: PathBuf::from("/tmp/sneaky"),
            profile: Profile::WorkspaceWrite,
        });
        assert!(registry.get("sneaky").is_ok());
    }
}
