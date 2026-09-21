//! In-memory resource serialization. Not a SQLite schema and not a thread queue.
//!
//! FIFO order starts when an eligible request reaches `acquire()`, not when
//! the MCP message arrives. Request-owned conflicts wait. A confirmed live
//! process is a barrier: new acquires fail immediately and trailing waiters
//! are closed with `WORKSPACE_BUSY`. Spawn reservation (`Spawning`) is not
//! that barrier; spawn failure releases and wakes the next waiter.
//!
//! Live MCP mutations take only `Resource::Workspace`. Before any code takes
//! two resources at once, define a canonical order or `acquire_many`. Path
//! occupancy is typed but unused. `busy()` still maps unused resource kinds
//! to `WORKSPACE_BUSY`; split that taxonomy before those keys are public.

use std::collections::{HashMap, VecDeque};

use codespace_domain::{ErrorBody, ErrorCode, MAX_WAITERS_PER_RESOURCE};
use tokio::sync::oneshot;

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
    /// Exec granted the lease but Runner spawn has not been confirmed.
    Spawning(String),
    /// Confirmed or unknown live process (`ProcessId`).
    Process(String),
}

struct Waiter {
    id: u64,
    mode: LockMode,
    owner: ExclusiveHolder,
    tx: oneshot::Sender<Result<(), ErrorBody>>,
}

#[derive(Default)]
pub(crate) struct ResourceSerializer {
    exclusive: HashMap<Resource, ExclusiveHolder>,
    shared: HashMap<Resource, u32>,
    waiters: HashMap<Resource, VecDeque<Waiter>>,
    next_waiter: u64,
}

pub(crate) enum AcquireOutcome {
    Granted,
    Busy(ErrorBody),
    Waiting {
        id: u64,
        rx: oneshot::Receiver<Result<(), ErrorBody>>,
    },
}

impl ResourceSerializer {
    pub(crate) fn try_exclusive_write(&mut self, workspace_id: &str) -> Result<(), ErrorBody> {
        let resource = Resource::Workspace(workspace_id.to_string());
        self.wake(&resource);
        if self.process_held(&resource) {
            return Err(shell_busy());
        }
        if self.exclusive.contains_key(&resource)
            || self.shared_count(&resource) > 0
            || self.has_waiters(&resource)
        {
            return Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "workspace write lock is held",
            ));
        }
        self.grant(resource, LockMode::Exclusive, ExclusiveHolder::Request);
        Ok(())
    }

    pub(crate) fn acquire_exclusive_write(&mut self, workspace_id: &str) -> AcquireOutcome {
        self.acquire(
            Resource::Workspace(workspace_id.to_string()),
            LockMode::Exclusive,
            ExclusiveHolder::Request,
        )
    }

    pub(crate) fn release_write(&mut self, workspace_id: &str) {
        let resource = Resource::Workspace(workspace_id.to_string());
        if matches!(
            self.exclusive.get(&resource),
            Some(ExclusiveHolder::Request)
        ) {
            self.exclusive.remove(&resource);
            self.wake(&resource);
        }
    }

    pub(crate) fn mark_shell_busy(
        &mut self,
        workspace_id: &str,
        process_id: &str,
    ) -> Result<(), ErrorBody> {
        let resource = Resource::Workspace(workspace_id.to_string());
        self.wake(&resource);
        if self.exclusive.contains_key(&resource)
            || self.shared_count(&resource) > 0
            || self.has_waiters(&resource)
        {
            return Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "workspace write lock is held",
            ));
        }
        self.grant(
            resource,
            LockMode::Exclusive,
            ExclusiveHolder::Process(process_id.to_string()),
        );
        Ok(())
    }

    pub(crate) fn acquire_shell_busy(
        &mut self,
        workspace_id: &str,
        process_id: &str,
    ) -> AcquireOutcome {
        self.acquire(
            Resource::Workspace(workspace_id.to_string()),
            LockMode::Exclusive,
            ExclusiveHolder::Spawning(process_id.to_string()),
        )
    }

    pub(crate) fn confirm_process(&mut self, process_id: &str) {
        let mut targets = Vec::new();
        for (resource, holder) in &mut self.exclusive {
            if matches!(holder, ExclusiveHolder::Spawning(id) if id == process_id) {
                *holder = ExclusiveHolder::Process(process_id.to_string());
                targets.push(resource.clone());
            }
        }
        for resource in targets {
            self.fail_waiters(&resource, shell_busy());
        }
    }

    pub(crate) fn clear_shell(&mut self, workspace_id: &str) {
        let resource = Resource::Workspace(workspace_id.to_string());
        if matches!(
            self.exclusive.get(&resource),
            Some(ExclusiveHolder::Process(_))
        ) {
            self.exclusive.remove(&resource);
            self.wake(&resource);
        }
    }

    pub(crate) fn release_process(&mut self, process_id: &str) {
        let released = self.release_matching(|holder| match holder {
            ExclusiveHolder::Spawning(id) | ExclusiveHolder::Process(id) => id == process_id,
            ExclusiveHolder::Request => false,
        });
        for resource in released {
            self.wake(&resource);
        }
    }

    pub(crate) fn release_all_processes(&mut self) {
        let released = self.release_matching(|holder| {
            matches!(
                holder,
                ExclusiveHolder::Spawning(_) | ExclusiveHolder::Process(_)
            )
        });
        for resource in released {
            self.wake(&resource);
        }
    }

    pub(crate) fn try_lock(&mut self, resource: Resource, mode: LockMode) -> Result<(), ErrorBody> {
        self.wake(&resource);
        if !self.can_try_grant(&resource, mode) {
            return Err(busy(&resource));
        }
        self.grant(resource, mode, ExclusiveHolder::Request);
        Ok(())
    }

    pub(crate) fn acquire_lock(&mut self, resource: Resource, mode: LockMode) -> AcquireOutcome {
        self.acquire(resource, mode, ExclusiveHolder::Request)
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
        self.wake(resource);
    }

    pub(crate) fn cancel_waiter(&mut self, resource: &Resource, id: u64) -> bool {
        let Some(queue) = self.waiters.get_mut(resource) else {
            return false;
        };
        let before = queue.len();
        queue.retain(|waiter| waiter.id != id);
        let removed = queue.len() != before;
        if queue.is_empty() {
            self.waiters.remove(resource);
        }
        if removed {
            self.wake(resource);
        }
        removed
    }

    #[cfg(test)]
    pub(crate) fn waiter_count(&self, resource: &Resource) -> usize {
        self.waiters.get(resource).map(VecDeque::len).unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn exclusive_kind(&self, resource: &Resource) -> Option<&'static str> {
        match self.exclusive.get(resource) {
            Some(ExclusiveHolder::Request) => Some("request"),
            Some(ExclusiveHolder::Spawning(_)) => Some("spawning"),
            Some(ExclusiveHolder::Process(_)) => Some("process"),
            None => None,
        }
    }

    fn acquire(
        &mut self,
        resource: Resource,
        mode: LockMode,
        owner: ExclusiveHolder,
    ) -> AcquireOutcome {
        if self.process_held(&resource) {
            return AcquireOutcome::Busy(process_held_busy(&resource));
        }
        let fairness_blocks = mode == LockMode::SharedRead && self.has_exclusive_waiter(&resource);
        if self.can_grant(&resource, mode)
            && !fairness_blocks
            && (mode == LockMode::SharedRead || !self.has_waiters(&resource))
        {
            self.grant(resource, mode, owner);
            return AcquireOutcome::Granted;
        }
        if self.waiter_count_unlocked(&resource) >= MAX_WAITERS_PER_RESOURCE as usize {
            return AcquireOutcome::Busy(queue_full());
        }
        let (id, rx) = self.enqueue(resource.clone(), mode, owner);
        self.wake(&resource);
        AcquireOutcome::Waiting { id, rx }
    }

    fn fail_waiters(&mut self, resource: &Resource, err: ErrorBody) {
        let Some(queue) = self.waiters.remove(resource) else {
            return;
        };
        for waiter in queue {
            let _ = waiter.tx.send(Err(err.clone()));
        }
    }

    fn enqueue(
        &mut self,
        resource: Resource,
        mode: LockMode,
        owner: ExclusiveHolder,
    ) -> (u64, oneshot::Receiver<Result<(), ErrorBody>>) {
        let id = self.next_waiter;
        self.next_waiter = self.next_waiter.wrapping_add(1);
        let (tx, rx) = oneshot::channel();
        self.waiters.entry(resource).or_default().push_back(Waiter {
            id,
            mode,
            owner,
            tx,
        });
        (id, rx)
    }

    fn wake(&mut self, resource: &Resource) {
        loop {
            let Some(front_mode) = self
                .waiters
                .get(resource)
                .and_then(|queue| queue.front().map(|waiter| waiter.mode))
            else {
                self.waiters.remove(resource);
                return;
            };
            if !self.can_grant(resource, front_mode) {
                return;
            }
            let Some(waiter) = self.waiters.get_mut(resource).and_then(VecDeque::pop_front) else {
                return;
            };
            if self
                .waiters
                .get(resource)
                .is_none_or(|queue| queue.is_empty())
            {
                self.waiters.remove(resource);
            }
            self.grant(resource.clone(), waiter.mode, waiter.owner);
            if waiter.tx.send(Ok(())).is_err() {
                self.unlock_silent(resource, waiter.mode);
                continue;
            }
            if waiter.mode == LockMode::Exclusive {
                return;
            }
        }
    }

    fn unlock_silent(&mut self, resource: &Resource, mode: LockMode) {
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

    fn grant(&mut self, resource: Resource, mode: LockMode, owner: ExclusiveHolder) {
        match mode {
            LockMode::SharedRead => {
                *self.shared.entry(resource).or_insert(0) += 1;
            }
            LockMode::Exclusive => {
                self.exclusive.insert(resource, owner);
            }
        }
    }

    fn can_grant(&self, resource: &Resource, mode: LockMode) -> bool {
        match mode {
            LockMode::SharedRead => !self.exclusive.contains_key(resource),
            LockMode::Exclusive => {
                !self.exclusive.contains_key(resource) && self.shared_count(resource) == 0
            }
        }
    }

    fn can_try_grant(&self, resource: &Resource, mode: LockMode) -> bool {
        match mode {
            LockMode::SharedRead => {
                self.can_grant(resource, mode) && !self.has_exclusive_waiter(resource)
            }
            LockMode::Exclusive => self.can_grant(resource, mode) && !self.has_waiters(resource),
        }
    }

    fn process_held(&self, resource: &Resource) -> bool {
        matches!(resource, Resource::Workspace(_))
            && matches!(
                self.exclusive.get(resource),
                Some(ExclusiveHolder::Process(_))
            )
    }

    fn waiter_count_unlocked(&self, resource: &Resource) -> usize {
        self.waiters.get(resource).map(VecDeque::len).unwrap_or(0)
    }

    fn has_waiters(&self, resource: &Resource) -> bool {
        self.waiter_count_unlocked(resource) > 0
    }

    fn has_exclusive_waiter(&self, resource: &Resource) -> bool {
        self.waiters.get(resource).is_some_and(|queue| {
            queue
                .iter()
                .any(|waiter| waiter.mode == LockMode::Exclusive)
        })
    }

    fn shared_count(&self, resource: &Resource) -> u32 {
        self.shared.get(resource).copied().unwrap_or(0)
    }

    fn release_matching(
        &mut self,
        mut pred: impl FnMut(&ExclusiveHolder) -> bool,
    ) -> Vec<Resource> {
        let mut released = Vec::new();
        self.exclusive.retain(|resource, holder| {
            if pred(holder) {
                released.push(resource.clone());
                false
            } else {
                true
            }
        });
        released
    }
}

fn busy(resource: &Resource) -> ErrorBody {
    // Unused resource kinds are not a public occupancy contract yet.
    // Split this taxonomy before Environment/Path/Process/Operation/Watch
    // become live MCP locks.
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

fn shell_busy() -> ErrorBody {
    ErrorBody::new(ErrorCode::WorkspaceBusy, "workspace has a busy shell")
}

fn queue_full() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::ResourceQueueFull,
        format!("resource waiter queue exceeds {MAX_WAITERS_PER_RESOURCE}"),
    )
}

fn process_held_busy(resource: &Resource) -> ErrorBody {
    match resource {
        Resource::Workspace(_) => shell_busy(),
        _ => busy(resource),
    }
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
    use std::sync::{Arc, Mutex};

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

    async fn wait_for_waiters(store: &Store, resource: &Resource, n: usize) {
        for _ in 0..200 {
            if store.waiter_count(resource) >= n {
                return;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("expected {n} waiters, got {}", store.waiter_count(resource));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_exclusive_fifo_order() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Workspace("demo".into());
        let first = store.acquire_write("demo").await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));

        let store_b = store.clone();
        let order_b = order.clone();
        let b = tokio::spawn(async move {
            let guard = store_b.acquire_write("demo").await.unwrap();
            order_b.lock().expect("order").push("b");
            drop(guard);
        });
        wait_for_waiters(&store, &resource, 1).await;

        let store_c = store.clone();
        let order_c = order.clone();
        let c = tokio::spawn(async move {
            let guard = store_c.acquire_write("demo").await.unwrap();
            order_c.lock().expect("order").push("c");
            drop(guard);
        });
        wait_for_waiters(&store, &resource, 2).await;

        drop(first);
        b.await.unwrap();
        c.await.unwrap();
        assert_eq!(*order.lock().expect("order"), vec!["b", "c"]);
    }

    #[tokio::test]
    async fn process_held_workspace_rejects_without_queueing() {
        let store = Store::memory().unwrap();
        store.mark_shell_busy("demo", "proc-1").unwrap();
        let err = match store.acquire_write("demo").await {
            Err(err) => err,
            Ok(_) => panic!("process-held write should be busy"),
        };
        assert_eq!(err.code, ErrorCode::WorkspaceBusy);
        assert_eq!(store.waiter_count(&Resource::Workspace("demo".into())), 0);
        let err = match store
            .lock(Resource::Workspace("demo".into()), LockMode::SharedRead)
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("process-held shared read should be busy"),
        };
        assert_eq!(err.code, ErrorCode::WorkspaceBusy);
        assert_eq!(store.waiter_count(&Resource::Workspace("demo".into())), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_waiter_does_not_block_next_grant() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Workspace("demo".into());
        let first = store.acquire_write("demo").await.unwrap();
        let store_b = store.clone();
        let b = tokio::spawn(async move {
            store_b.acquire_write("demo").await.unwrap();
        });
        wait_for_waiters(&store, &resource, 1).await;
        b.abort();
        let _ = b.await;
        for _ in 0..50 {
            if store.waiter_count(&resource) == 0 {
                break;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(store.waiter_count(&resource), 0);
        drop(first);
        let _next = store.acquire_write("demo").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_read_waits_behind_request_exclusive() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Path {
            workspace: "demo".into(),
            path: "a.txt".into(),
        };
        let exclusive = store
            .lock(resource.clone(), LockMode::Exclusive)
            .await
            .unwrap();
        let store_r = store.clone();
        let resource_r = resource.clone();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let reader = tokio::spawn(async move {
            let _shared = store_r
                .lock(resource_r, LockMode::SharedRead)
                .await
                .unwrap();
            held_tx.send(()).expect("held");
            let _ = release_rx.await;
        });
        wait_for_waiters(&store, &resource, 1).await;
        drop(exclusive);
        held_rx.await.expect("shared granted");
        assert!(store
            .try_lock(resource.clone(), LockMode::Exclusive)
            .is_err());
        release_tx.send(()).expect("release");
        reader.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_read_does_not_barge_ahead_of_exclusive_waiter() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Path {
            workspace: "demo".into(),
            path: "a.txt".into(),
        };
        let first = store
            .lock(resource.clone(), LockMode::Exclusive)
            .await
            .unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));

        let store_ex = store.clone();
        let resource_ex = resource.clone();
        let order_ex = order.clone();
        let (ex_held_tx, ex_held_rx) = tokio::sync::oneshot::channel();
        let (ex_release_tx, ex_release_rx) = tokio::sync::oneshot::channel();
        let exclusive_waiter = tokio::spawn(async move {
            let _guard = store_ex
                .lock(resource_ex, LockMode::Exclusive)
                .await
                .unwrap();
            order_ex.lock().expect("order").push("exclusive");
            ex_held_tx.send(()).expect("held");
            let _ = ex_release_rx.await;
        });
        wait_for_waiters(&store, &resource, 1).await;

        let store_sh = store.clone();
        let resource_sh = resource.clone();
        let order_sh = order.clone();
        let (sh_held_tx, sh_held_rx) = tokio::sync::oneshot::channel();
        let (sh_release_tx, sh_release_rx) = tokio::sync::oneshot::channel();
        let shared_waiter = tokio::spawn(async move {
            let _guard = store_sh
                .lock(resource_sh, LockMode::SharedRead)
                .await
                .unwrap();
            order_sh.lock().expect("order").push("shared");
            sh_held_tx.send(()).expect("held");
            let _ = sh_release_rx.await;
        });
        wait_for_waiters(&store, &resource, 2).await;

        drop(first);
        ex_held_rx.await.expect("exclusive granted");
        assert_eq!(*order.lock().expect("order"), vec!["exclusive"]);
        ex_release_tx.send(()).expect("release exclusive");
        exclusive_waiter.await.unwrap();
        sh_held_rx.await.expect("shared granted");
        sh_release_tx.send(()).expect("release shared");
        shared_waiter.await.unwrap();
        assert_eq!(*order.lock().expect("order"), vec!["exclusive", "shared"]);
    }

    #[tokio::test]
    async fn workspaces_are_independent() {
        let store = Store::memory().unwrap();
        let _demo = store.acquire_write("demo").await.unwrap();
        let _other = store.acquire_write("other").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exec_waits_behind_request_owned_write() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Workspace("demo".into());
        let write = store.acquire_write("demo").await.unwrap();
        let store_exec = store.clone();
        let exec = tokio::spawn(async move {
            let reservation = store_exec.acquire_shell_busy("demo", "proc-wait").await?;
            reservation.confirm();
            Ok::<(), ErrorBody>(())
        });
        wait_for_waiters(&store, &resource, 1).await;
        drop(write);
        exec.await.unwrap().unwrap();
        assert_eq!(store.exclusive_kind(&resource), Some("process"));
        assert_eq!(
            store.try_acquire_write("demo").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
        store.release_process("proc-wait");
        let _next = store.try_acquire_write("demo").unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirm_live_process_fails_trailing_waiters() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Workspace("demo".into());
        let write = store.acquire_write("demo").await.unwrap();
        let store_exec = store.clone();
        let exec = tokio::spawn(async move {
            let reservation = store_exec.acquire_shell_busy("demo", "proc-live").await?;
            reservation.confirm();
            Ok::<(), ErrorBody>(())
        });
        wait_for_waiters(&store, &resource, 1).await;
        let store_patch = store.clone();
        let trailing = tokio::spawn(async move {
            match store_patch.acquire_write("demo").await {
                Ok(_) => panic!("trailing waiter should be busy"),
                Err(err) => err,
            }
        });
        wait_for_waiters(&store, &resource, 2).await;
        drop(write);
        exec.await.unwrap().unwrap();
        let err = trailing.await.unwrap();
        assert_eq!(err.code, ErrorCode::WorkspaceBusy);
        assert_eq!(store.waiter_count(&resource), 0);
        let late = match store.acquire_write("demo").await {
            Err(err) => err,
            Ok(_) => panic!("late write should be busy"),
        };
        assert_eq!(late.code, ErrorCode::WorkspaceBusy);
        store.release_process("proc-live");
        let _next = store.acquire_write("demo").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_abort_wakes_next_waiter() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Workspace("demo".into());
        let write = store.acquire_write("demo").await.unwrap();
        let store_exec = store.clone();
        let exec = tokio::spawn(async move {
            let reservation = store_exec.acquire_shell_busy("demo", "proc-fail").await?;
            reservation.abort();
            Ok::<(), ErrorBody>(())
        });
        wait_for_waiters(&store, &resource, 1).await;
        let store_patch = store.clone();
        let next = tokio::spawn(async move {
            let _guard = store_patch.acquire_write("demo").await?;
            Ok::<(), ErrorBody>(())
        });
        wait_for_waiters(&store, &resource, 2).await;
        drop(write);
        exec.await.unwrap().unwrap();
        next.await.unwrap().unwrap();
        assert_eq!(store.exclusive_kind(&resource), None);
    }

    #[tokio::test]
    async fn pre_dispatch_drop_releases_spawn_reservation() {
        let store = Store::memory().unwrap();
        let resource = Resource::Workspace("demo".into());
        {
            let _reservation = store.acquire_shell_busy("demo", "proc-drop").await.unwrap();
            assert_eq!(store.exclusive_kind(&resource), Some("spawning"));
        }
        assert_eq!(store.exclusive_kind(&resource), None);
        let _write = store.acquire_write("demo").await.unwrap();
    }

    #[tokio::test]
    async fn dispatching_drop_keeps_spawn_reservation() {
        let store = Store::memory().unwrap();
        let resource = Resource::Workspace("demo".into());
        {
            let mut reservation = store.acquire_shell_busy("demo", "proc-arm").await.unwrap();
            reservation.arm_dispatch();
        }
        assert_eq!(store.exclusive_kind(&resource), Some("spawning"));
        store.release_process("proc-arm");
        assert_eq!(store.exclusive_kind(&resource), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queue_depth_bound_rejects_without_enqueue() {
        let store = Arc::new(Store::memory().unwrap());
        let resource = Resource::Workspace("demo".into());
        let first = store.acquire_write("demo").await.unwrap();
        let mut waiters = Vec::new();
        for _ in 0..MAX_WAITERS_PER_RESOURCE {
            let store_w = store.clone();
            waiters.push(tokio::spawn(async move {
                let _guard = store_w.acquire_write("demo").await?;
                std::future::pending::<()>().await;
                Ok::<(), ErrorBody>(())
            }));
        }
        wait_for_waiters(&store, &resource, MAX_WAITERS_PER_RESOURCE as usize).await;
        let err = match store.acquire_write("demo").await {
            Err(err) => err,
            Ok(_) => panic!("queue should be full"),
        };
        assert_eq!(err.code, ErrorCode::ResourceQueueFull);
        assert_eq!(
            store.waiter_count(&resource),
            MAX_WAITERS_PER_RESOURCE as usize
        );
        waiters.pop().unwrap().abort();
        for _ in 0..50 {
            if store.waiter_count(&resource) < MAX_WAITERS_PER_RESOURCE as usize {
                break;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(store.waiter_count(&resource) < MAX_WAITERS_PER_RESOURCE as usize);
        let recovered = tokio::spawn({
            let store = store.clone();
            async move {
                let _guard = store.acquire_write("demo").await?;
                std::future::pending::<()>().await;
                Ok::<(), ErrorBody>(())
            }
        });
        wait_for_waiters(&store, &resource, MAX_WAITERS_PER_RESOURCE as usize).await;
        for waiter in waiters {
            waiter.abort();
        }
        recovered.abort();
        drop(first);
    }
}
