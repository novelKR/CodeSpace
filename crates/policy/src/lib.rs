//! Workspace registry and path policy. No `rmcp` types.

mod environment;
mod permission;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use codespace_domain::{ApprovalsMode, ErrorBody, ErrorCode, Profile, WorkspaceId};
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
    /// Operator JSON confirmation hold. Not an MCP tool field and not a grant.
    #[serde(default)]
    pub approvals: ApprovalsMode,
    /// Operator JSON resource participation. Not an MCP tool field. Left out when off, so a
    /// workspace without it is encoded as before.
    #[serde(default, skip_serializing_if = "Resources::is_off")]
    pub resources: Resources,
}

/// Whether a workspace's executions take part in a resource authority (CSRG-U2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Participation {
    /// Executions run as they always have, without a resource authority.
    #[default]
    Off,
    /// Every new execution must be admitted and launched through the resource authority.
    /// CodeSpace admits it (CSRG-U3) but cannot launch it there yet, so none runs; none runs
    /// without the authority either.
    Required,
}

/// A workspace's `resources` settings. An unknown setting fails the load, so a setting this
/// version does not know never leaves an execution ungoverned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default)]
    pub participation: Participation,
    /// What each execution asks the authority for, with `required`. Left out, it is
    /// [`ResourceRequest::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<ResourceRequest>,
}

impl Resources {
    pub fn is_off(&self) -> bool {
        self.participation == Participation::Off && self.request.is_none()
    }

    /// The request each execution makes.
    pub fn request(&self) -> ResourceRequest {
        self.request.unwrap_or_default()
    }
}

/// How strongly the authority controls a resource, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementLevel {
    /// Counted against the authority's capacity.
    Accounted,
    /// Steered by priority and QoS.
    Cooperative,
    /// Limited by the kernel.
    Kernel,
}

/// The weakest control an execution accepts for each resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceMinimum {
    pub cpu: EnforcementLevel,
    pub memory: EnforcementLevel,
    pub pids: EnforcementLevel,
}

impl Default for ResourceMinimum {
    fn default() -> Self {
        Self {
            cpu: EnforcementLevel::Accounted,
            memory: EnforcementLevel::Accounted,
            pids: EnforcementLevel::Accounted,
        }
    }
}

/// The quantities one execution asks the authority to reserve, and the weakest control it
/// accepts. Every quantity is positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequest {
    pub cpu_milli: u64,
    pub memory_bytes: u64,
    pub tasks: u64,
    #[serde(default)]
    pub minimum: ResourceMinimum,
}

impl Default for ResourceRequest {
    /// One CPU, 512 MiB and 64 tasks, each at least accounted.
    fn default() -> Self {
        Self {
            cpu_milli: 1_000,
            memory_bytes: 512 << 20,
            tasks: 64,
            minimum: ResourceMinimum::default(),
        }
    }
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
            approvals: ApprovalsMode::Off,
            resources: Resources::default(),
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
    #[serde(default)]
    approvals: ApprovalsMode,
    #[serde(default)]
    resources: Resources,
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
            if let Some(request) = entry.resources.request {
                if request.cpu_milli == 0 || request.memory_bytes == 0 || request.tasks == 0 {
                    return Err(format!(
                        "workspace `{id}` resources.request quantities must be positive"
                    ));
                }
            }
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
                approvals: entry.approvals,
                resources: entry.resources,
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
        assert_eq!(ws.approvals, ApprovalsMode::Off);
    }

    #[test]
    fn approvals_confirm_is_operator_config() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","profile":"workspace-write","approvals":"confirm"}}}"#;
        let registry = Registry::load_json(json).unwrap();
        let ws = registry.get("demo").unwrap();
        assert_eq!(ws.approvals, ApprovalsMode::Confirm);
        assert!(allow(ws, Action::Write, &ClientClaims::default()).is_ok());
    }

    #[test]
    fn resource_participation_is_off_unless_required() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo"}}}"#;
        let registry = Registry::load_json(json).unwrap();
        let ws = registry.get("demo").unwrap();
        assert_eq!(ws.resources.participation, Participation::Off);
        // Off is not encoded, so a workspace without it reaches the runner as before.
        let encoded = serde_json::to_value(ws).unwrap();
        assert!(encoded.get("resources").is_none(), "{encoded}");

        for json in [
            r#"{"workspaces":{"demo":{"root":"/tmp/demo","resources":{}}}}"#,
            r#"{"workspaces":{"demo":{"root":"/tmp/demo","resources":{"participation":"off"}}}}"#,
        ] {
            let registry = Registry::load_json(json).unwrap();
            assert!(registry.get("demo").unwrap().resources.is_off());
        }

        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","resources":{"participation":"required"}}}}"#;
        let registry = Registry::load_json(json).unwrap();
        let ws = registry.get("demo").unwrap();
        assert_eq!(ws.resources.participation, Participation::Required);
        let encoded = serde_json::to_value(ws).unwrap();
        assert_eq!(
            encoded["resources"],
            serde_json::json!({"participation": "required"})
        );
        let decoded: Workspace = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.resources, ws.resources);
    }

    #[test]
    fn a_resource_request_is_operator_config_with_positive_quantities() {
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","resources":{"participation":"required"}}}}"#;
        let registry = Registry::load_json(json).unwrap();
        assert_eq!(
            registry.get("demo").unwrap().resources.request(),
            ResourceRequest::default()
        );
        let json = r#"{"workspaces":{"demo":{"root":"/tmp/demo","resources":{"participation":"required","request":{"cpu_milli":250,"memory_bytes":1048576,"tasks":4,"minimum":{"cpu":"cooperative","memory":"accounted","pids":"kernel"}}}}}}"#;
        let registry = Registry::load_json(json).unwrap();
        let ws = registry.get("demo").unwrap();
        let request = ws.resources.request();
        assert_eq!(
            (request.cpu_milli, request.memory_bytes, request.tasks),
            (250, 1 << 20, 4)
        );
        assert_eq!(request.minimum.cpu, EnforcementLevel::Cooperative);
        assert_eq!(request.minimum.pids, EnforcementLevel::Kernel);
        let decoded: Workspace = serde_json::from_value(serde_json::to_value(ws).unwrap()).unwrap();
        assert_eq!(decoded.resources, ws.resources);
        for request in [
            r#"{"cpu_milli":0,"memory_bytes":1,"tasks":1}"#,
            r#"{"cpu_milli":1,"memory_bytes":0,"tasks":1}"#,
            r#"{"cpu_milli":1,"memory_bytes":1,"tasks":0}"#,
            r#"{"cpu_milli":1,"memory_bytes":1}"#,
            r#"{"cpu_milli":1,"memory_bytes":1,"tasks":1,"minimum":{"cpu":"strict","memory":"accounted","pids":"accounted"}}"#,
            r#"{"cpu_milli":1,"memory_bytes":1,"tasks":1,"burst":1}"#,
        ] {
            let json = format!(
                r#"{{"workspaces":{{"demo":{{"root":"/tmp/demo","resources":{{"participation":"required","request":{request}}}}}}}}}"#
            );
            assert!(Registry::load_json(&json).is_err(), "{request}");
        }
    }

    #[test]
    fn unknown_resource_settings_fail_config_load() {
        for resources in [
            r#"{"participaton":"required"}"#,
            r#"{"participation":"require"}"#,
            r#"{"participation":"required","budget":{}}"#,
            r#""required""#,
            "null",
        ] {
            let json = format!(
                r#"{{"workspaces":{{"demo":{{"root":"/tmp/demo","resources":{resources}}}}}}}"#
            );
            assert!(Registry::load_json(&json).is_err(), "{resources}");
        }
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
