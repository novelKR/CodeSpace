//! In-memory resource serialization. Not a SQLite schema and not a thread queue.

use std::collections::HashMap;

use codespace_domain::{ErrorBody, ErrorCode};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Resource {
    Environment(String),
    Workspace(String),
    Path { workspace: String, path: String },
    Process(String),
    Operation(String),
    Watch(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Exclusive,
    /// Typed but unused by live `read` / `find` (those stay unlocked).
    SharedRead,
}

#[derive(Debug, Clone)]
enum ExclusiveHolder {
    /// Request-owned RAII exclusive (`WriteGuard`).
    Request,
    /// Process-owned exclusive (`ProcessId`).
    Process(String),
}

#[derive(Default)]
pub(crate) struct ResourceSerializer {
    exclusive: HashMap<Resource, ExclusiveHolder>,
    shared: HashMap<Resource, u32>,
}

impl ResourceSerializer {
    pub(crate) fn try_exclusive_write(&mut self, workspace_id: &str) -> Result<(), ErrorBody> {
        let resource = Resource::Workspace(workspace_id.to_string());
        if let Some(ExclusiveHolder::Process(_)) = self.exclusive.get(&resource) {
            return Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "workspace has a busy shell",
            ));
        }
        if self.exclusive.contains_key(&resource) || self.shared_count(&resource) > 0 {
            return Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "workspace write lock is held",
            ));
        }
        self.exclusive.insert(resource, ExclusiveHolder::Request);
        Ok(())
    }

    pub(crate) fn release_write(&mut self, workspace_id: &str) {
        let resource = Resource::Workspace(workspace_id.to_string());
        if matches!(
            self.exclusive.get(&resource),
            Some(ExclusiveHolder::Request)
        ) {
            self.exclusive.remove(&resource);
        }
    }

    pub(crate) fn mark_shell_busy(
        &mut self,
        workspace_id: &str,
        process_id: &str,
    ) -> Result<(), ErrorBody> {
        let resource = Resource::Workspace(workspace_id.to_string());
        if self.exclusive.contains_key(&resource) || self.shared_count(&resource) > 0 {
            return Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "workspace write lock is held",
            ));
        }
        self.exclusive
            .insert(resource, ExclusiveHolder::Process(process_id.to_string()));
        Ok(())
    }

    pub(crate) fn clear_shell(&mut self, workspace_id: &str) {
        let resource = Resource::Workspace(workspace_id.to_string());
        if matches!(
            self.exclusive.get(&resource),
            Some(ExclusiveHolder::Process(_))
        ) {
            self.exclusive.remove(&resource);
        }
    }

    pub(crate) fn release_process(&mut self, process_id: &str) {
        self.exclusive.retain(|_, holder| match holder {
            ExclusiveHolder::Process(id) => id != process_id,
            ExclusiveHolder::Request => true,
        });
    }

    pub(crate) fn release_all_processes(&mut self) {
        self.exclusive.retain(|_, holder| match holder {
            ExclusiveHolder::Process(_) => false,
            ExclusiveHolder::Request => true,
        });
    }

    pub(crate) fn try_lock(&mut self, resource: Resource, mode: LockMode) -> Result<(), ErrorBody> {
        match mode {
            LockMode::SharedRead => {
                if self.exclusive.contains_key(&resource) {
                    return Err(busy(&resource));
                }
                *self.shared.entry(resource).or_insert(0) += 1;
                Ok(())
            }
            LockMode::Exclusive => {
                if self.exclusive.contains_key(&resource) || self.shared_count(&resource) > 0 {
                    return Err(busy(&resource));
                }
                self.exclusive.insert(resource, ExclusiveHolder::Request);
                Ok(())
            }
        }
    }

    pub(crate) fn unlock(&mut self, resource: &Resource, mode: LockMode) {
        match mode {
            LockMode::SharedRead => {
                if let Some(count) = self.shared.get_mut(resource) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.shared.remove(resource);
                    }
                }
            }
            LockMode::Exclusive => {
                self.exclusive.remove(resource);
            }
        }
    }

    fn shared_count(&self, resource: &Resource) -> u32 {
        self.shared.get(resource).copied().unwrap_or(0)
    }
}

fn busy(resource: &Resource) -> ErrorBody {
    let detail = match resource {
        Resource::Workspace(_) => "workspace write lock is held",
        Resource::Environment(_) => "environment lock is held",
        Resource::Path { .. } => "path lock is held",
        Resource::Process(_) => "process lock is held",
        Resource::Operation(_) => "operation lock is held",
        Resource::Watch(_) => "watch lock is held",
    };
    ErrorBody::new(ErrorCode::WorkspaceBusy, detail)
}

pub struct ResourceGuard<'a> {
    store: &'a super::Store,
    resource: Resource,
    mode: LockMode,
}

impl<'a> ResourceGuard<'a> {
    pub(crate) fn new(store: &'a super::Store, resource: Resource, mode: LockMode) -> Self {
        Self {
            store,
            resource,
            mode,
        }
    }
}

impl Drop for ResourceGuard<'_> {
    fn drop(&mut self) {
        self.store.release_resource(&self.resource, self.mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[test]
    fn shared_read_does_not_take_exclusive() {
        let store = Store::memory().unwrap();
        let first = store
            .try_lock(
                Resource::Path {
                    workspace: "demo".into(),
                    path: "a.txt".into(),
                },
                LockMode::SharedRead,
            )
            .unwrap();
        let second = store
            .try_lock(
                Resource::Path {
                    workspace: "demo".into(),
                    path: "a.txt".into(),
                },
                LockMode::SharedRead,
            )
            .unwrap();
        drop(first);
        drop(second);
        let _exclusive = store
            .try_lock(
                Resource::Path {
                    workspace: "demo".into(),
                    path: "a.txt".into(),
                },
                LockMode::Exclusive,
            )
            .unwrap();
    }

    #[test]
    fn exclusive_blocks_shared() {
        let store = Store::memory().unwrap();
        let resource = Resource::Environment("local".into());
        let _guard = store
            .try_lock(resource.clone(), LockMode::Exclusive)
            .unwrap();
        assert!(store.try_lock(resource, LockMode::SharedRead).is_err());
    }

    #[test]
    fn read_find_do_not_require_shared_lock() {
        let store = Store::memory().unwrap();
        let _write = store.try_acquire_write("demo").unwrap();
        // SharedRead exists as a mode only. Live read/find do not take it.
        assert!(store
            .try_lock(Resource::Workspace("demo".into()), LockMode::SharedRead)
            .is_err());
    }
}
