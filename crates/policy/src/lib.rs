//! Workspace registry and path policy. No `rmcp` types.

mod environment;
mod permission;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use codespace_domain::{ErrorBody, ErrorCode, Profile, WorkspaceId};
use serde::{Deserialize, Serialize};

pub use environment::{
    require_exec, require_file_read, require_file_write, Environment, EnvironmentDispatchError,
    EnvironmentKind, DEFAULT_ENVIRONMENT_ID,
};
pub use permission::{NetworkAxis, PathAccess, PathRule, PermissionProfile};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub root: PathBuf,
    pub profile: Profile,
    #[serde(default = "default_environment_id")]
    pub environment_id: String,
    #[serde(default)]
    pub environment_kind: EnvironmentKind,
    /// Operator JSON, like `environment`. Not an MCP tool field.
    #[serde(default)]
    pub network: NetworkAxis,
}

fn default_environment_id() -> String {
    DEFAULT_ENVIRONMENT_ID.to_string()
}

impl Workspace {
    pub fn new(id: WorkspaceId, root: PathBuf, profile: Profile) -> Self {
        Self {
            id,
            root,
            profile,
            environment_id: DEFAULT_ENVIRONMENT_ID.to_string(),
            environment_kind: EnvironmentKind::Host,
            network: NetworkAxis::Restricted,
        }
    }

    pub fn require_exec(&self) -> Result<(), ErrorBody> {
        require_exec(self.environment_kind).map_err(EnvironmentDispatchError::into_error_body)
    }

    pub fn require_file_read(&self) -> Result<(), ErrorBody> {
        require_file_read(self.environment_kind).map_err(EnvironmentDispatchError::into_error_body)
    }

    pub fn require_file_write(&self) -> Result<(), ErrorBody> {
        require_file_write(self.environment_kind).map_err(EnvironmentDispatchError::into_error_body)
    }
}

#[derive(Debug, Clone)]
pub struct Registry {
    environments: BTreeMap<String, Environment>,
    workspaces: BTreeMap<String, Workspace>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    environments: BTreeMap<String, FileEnvironment>,
    #[serde(default)]
    workspaces: BTreeMap<String, FileWorkspace>,
}

#[derive(Debug, Deserialize)]
struct FileEnvironment {
    kind: EnvironmentKind,
}

#[derive(Debug, Deserialize)]
struct FileWorkspace {
    root: String,
    #[serde(default)]
    profile: Profile,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    network: NetworkAxis,
}

impl Registry {
    pub fn new() -> Self {
        let mut environments = BTreeMap::new();
        let local = Environment::local_host();
        environments.insert(local.id.clone(), local);
        Self {
            environments,
            workspaces: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, workspace: Workspace) {
        self.workspaces.insert(workspace.id.0.clone(), workspace);
    }

    pub fn insert_environment(&mut self, environment: Environment) {
        self.environments
            .insert(environment.id.clone(), environment);
    }

    pub fn get(&self, id: &str) -> Result<&Workspace, ErrorBody> {
        self.workspaces.get(id).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::WorkspaceNotFound,
                format!("unknown workspace_id `{id}`"),
            )
        })
    }

    pub fn environment(&self, id: &str) -> Result<&Environment, ErrorBody> {
        self.environments.get(id).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("unknown environment `{id}`"),
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
        for (id, entry) in parsed.environments {
            registry.insert_environment(Environment {
                id,
                kind: entry.kind,
            });
        }
        for (id, entry) in parsed.workspaces {
            let root = PathBuf::from(&entry.root);
            if !root.is_absolute() {
                return Err(format!("workspace `{id}` root must be absolute"));
            }
            let environment_id = entry
                .environment
                .unwrap_or_else(|| DEFAULT_ENVIRONMENT_ID.to_string());
            let environment = registry.environments.get(&environment_id).ok_or_else(|| {
                format!("workspace `{id}` references unknown environment `{environment_id}`")
            })?;
            registry.insert(Workspace {
                id: WorkspaceId(id),
                root,
                profile: entry.profile,
                environment_id: environment.id.clone(),
                environment_kind: environment.kind,
                network: entry.network,
            });
        }
        Ok(registry)
    }

    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
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
            if PermissionProfile::from_workspace_profile(workspace.profile).allows(action) {
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
        Workspace::new(WorkspaceId("demo".into()), dir.to_path_buf(), profile)
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
        let ws = registry.get("demo").unwrap();
        assert_eq!(ws.profile, Profile::ReadOnly);
        assert_eq!(ws.environment_id, DEFAULT_ENVIRONMENT_ID);
        assert_eq!(ws.environment_kind, EnvironmentKind::Host);
        assert_eq!(ws.network, NetworkAxis::Restricted);
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
        registry.insert(Workspace::new(
            WorkspaceId("sneaky".into()),
            PathBuf::from("/tmp/sneaky"),
            Profile::WorkspaceWrite,
        ));
        assert!(registry.get("sneaky").is_ok());
    }

    #[test]
    fn unknown_environment_fails_config_load() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","environment":"missing"}}}"#;
        let err = Registry::load_json(json).unwrap_err();
        assert!(err.contains("unknown environment"));
    }

    #[test]
    fn operator_network_enabled_loads() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","network":"enabled"}}}"#;
        let registry = Registry::load_json(json).unwrap();
        let ws = registry.get("demo").unwrap();
        assert_eq!(ws.network, NetworkAxis::Enabled);
        assert_eq!(ws.profile, Profile::ReadOnly);
    }

    #[test]
    fn unknown_network_fails_config_load() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","network":"open"}}}"#;
        assert!(Registry::load_json(json).is_err());
    }

    #[test]
    fn linux_container_environment_loads_but_is_not_an_exec_path() {
        let json = r#"{
            "environments": {"box": {"kind": "linux-container"}},
            "workspaces": {"demo": {"root": "/tmp/demo", "environment": "box"}}
        }"#;
        let registry = Registry::load_json(json).unwrap();
        let ws = registry.get("demo").unwrap();
        assert_eq!(ws.environment_kind, EnvironmentKind::LinuxContainer);
        let err = require_exec(ws.environment_kind).unwrap_err();
        assert!(matches!(
            err,
            EnvironmentDispatchError::UnsupportedExec {
                kind: EnvironmentKind::LinuxContainer
            }
        ));
        assert_eq!(ws.require_exec().unwrap_err().code, ErrorCode::Unauthorized);
        assert_eq!(
            ws.require_file_read().unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            ws.require_file_write().unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert!(ws.require_exec().unwrap_err().operation_id.is_none());
        assert!(ws.require_file_read().unwrap_err().operation_id.is_none());
        assert!(ws.require_file_write().unwrap_err().operation_id.is_none());
    }
}
