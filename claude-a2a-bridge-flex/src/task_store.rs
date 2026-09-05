// Copyright 2026 Salesforce, Inc. All rights reserved.

//! A2A task state storage for this bridge policy.
//!
//! ## Per-Pod Bucket Strategy
//!
//! Each replica writes tasks to its own key namespace, keyed by replica ID
//! (`HOSTNAME` env var, or a fixed fallback). A shared `replica-registry` key
//! tracks all live replica IDs so reads can fan out.
//!
//! | Key pattern                           | Purpose                        |
//! |---------------------------------------|--------------------------------|
//! | `replica-registry`                    | Vec<String> of replica IDs     |
//! | `pod:{replicaId}:task:{taskId}`       | Full task record               |
//! | `pod:{replicaId}:ctx:{contextId}`     | Vec<taskId> for context        |
//! | `pod:{replicaId}:user:{userId}`       | Vec<taskId> for user           |
//!
//! The store is already scoped to `{policy_name}` by the PDK.
//! Indexes are append-only — `update` never touches them.

use crate::time::now_unix_secs;
use crate::replica_registry;
use pdk::data_storage::{DataStorage, StoreMode};
use pdk::logger;
use serde::{Deserialize, Serialize};

// ── Roles & Parts ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Role {
    #[serde(rename = "ROLE_USER")]
    User,
    #[serde(rename = "ROLE_AGENT")]
    Agent,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Part {
    Text { text: String },
    Data { data: serde_json::Value, media_type: Option<String> },
    File {
        url: Option<String>,
        raw: Option<String>,
        filename: Option<String>,
        media_type: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub message_id: String,
    pub parts: Vec<Part>,
    /// Turn time (unix millis); orders history when merging replica slices.
    pub ts: u64,
}

// ── Task state ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum TaskState {
    #[serde(rename = "TASK_STATE_SUBMITTED")]
    Submitted,
    #[serde(rename = "TASK_STATE_WORKING")]
    Working,
    #[serde(rename = "TASK_STATE_INPUT_REQUIRED")]
    InputRequired,
    #[serde(rename = "TASK_STATE_AUTH_REQUIRED")]
    AuthRequired,
    #[serde(rename = "TASK_STATE_COMPLETED")]
    Completed,
    #[serde(rename = "TASK_STATE_FAILED")]
    Failed,
    #[serde(rename = "TASK_STATE_CANCELED")]
    Canceled,
    #[serde(rename = "TASK_STATE_REJECTED")]
    Rejected,
    #[serde(rename = "TASK_STATE_UNSPECIFIED")]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskStatus {
    pub state: TaskState,
    /// Agent question on InputRequired; failure reason on Failed.
    pub message: Option<Message>,
    /// Unix secs; updated on every write; maps to A2A lastModified.
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskArtifact {
    pub artifact_id: String,
    pub parts: Vec<Part>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskEntry {
    pub task_id: String,
    /// Embedded so load(task_id) can populate the A2A response contextId without an index lookup.
    pub context_id: String,
    /// Subject from bearer token; written to user-index on create; used for tasks/list filtering.
    pub user_id: String,
    /// messageId from the inbound A2A request.
    pub message_id: String,
    pub status: TaskStatus,
    pub artifacts: Vec<TaskArtifact>,
    /// This replica's slice of turns; full history = all slices merged, ordered by `ts` (see `load`).
    pub history: Vec<Message>,
    pub created_at: u64,
    /// Unix secs; used to resolve conflicts when the same taskId appears in multiple replica buckets.
    pub updated_at: u64,
}

// ── Index entries ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ContextIndexEntry {
    pub context_id: String,
    pub task_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UserIndexEntry {
    pub user_id: String,
    pub task_ids: Vec<String>,
}

// ── TaskStore ─────────────────────────────────────────────────────────────────

/// Access layer for task state. All task reads and writes go through this —
/// callers never construct storage keys directly.
///
/// The storage bucket is already isolated per agent by the caller.
pub struct TaskStore<'a, S: DataStorage> {
    storage: &'a S,
}

impl<'a, S: DataStorage> TaskStore<'a, S> {
    /// Construct a `TaskStore`.
    pub fn new(storage: &'a S) -> Self {
        Self { storage }
    }

    /// Write a new task in `Submitted` state.
    ///
    /// Also appends `task_id` to the ctx-index and user-index under a process-level
    /// lock. Returns `false` if `task_id` already exists.
    pub async fn create(&self, entry: &TaskEntry) -> bool {
        match self.storage.store(&self.task_key(&entry.task_id), &StoreMode::Absent, entry).await {
            Ok(_) => {}
            Err(e) => {
                logger::warn!(
                    "[claude-bridge] task create failed (already exists?) for task_id={}: {:?}",
                    entry.task_id,
                    e
                );
                return false;
            }
        }

        self.append_to_index(&self.ctx_index_key(&entry.context_id), &entry.context_id, &entry.task_id, "ctx-index").await;
        if !entry.user_id.is_empty() {
            self.append_to_index(&self.user_index_key(&entry.user_id), &entry.user_id, &entry.task_id, "user-index").await;
        }
        true
    }

    /// Create-or-update a [`TaskEntry`]: [`create`](Self::create) on first write,
    /// else [`update`](Self::update). `updated_at`/`status.timestamp` advance to
    /// now; `created_at` is preserved across updates. Returns the timestamp
    /// written, so callers can echo it in the immediate response.
    pub async fn persist(
        &self,
        task_id: &str,
        context_id: &str,
        user_id: &str,
        state: TaskState,
        message: Option<Message>,
        artifacts: Vec<TaskArtifact>,
        new_messages: Vec<Message>,
    ) -> u64 {
        let now = now_unix_secs();
        let entry = TaskEntry {
            task_id: task_id.to_string(),
            context_id: context_id.to_string(),
            user_id: user_id.to_string(),
            message_id: task_id.to_string(),
            status: TaskStatus {
                state,
                message,
                timestamp: now,
            },
            artifacts,
            history: new_messages,
            created_at: now,
            updated_at: now,
        };

        if self.create(&entry).await {
            return now;
        }

        // Task already exists — update our own slice with this turn.
        self.update_task_entry(task_id, &entry).await;
        now
    }

    /// Update this replica's existing task slice under CAS retry, so a concurrent
    /// same-pod write is preserved rather than overwritten. `entry` carries this
    /// turn's snapshot (status/artifacts) and its new history messages; the stored
    /// `created_at` and prior history are kept.
    async fn update_task_entry(&self, task_id: &str, entry: &TaskEntry) {
        const MAX_RETRIES: u32 = 3;
        let key = self.task_key(task_id);
        for attempt in 0..MAX_RETRIES {
            let (task_entry, mode) = match self.storage.get::<TaskEntry>(&key).await {
                Ok(Some((existing, version))) => {
                    // `entry` carries this turn's snapshot; keep the stored
                    // created_at and put the prior history before this turn's.
                    let mut history = existing.history;
                    history.extend(entry.history.iter().cloned());
                    (
                        TaskEntry { created_at: existing.created_at, history, ..entry.clone() },
                        StoreMode::Cas(version),
                    )
                }
                Ok(None) => (entry.clone(), StoreMode::Absent),
                Err(e) => {
                    logger::warn!(
                        "[claude-bridge] persist re-read failed for task_id={}: {:?}",
                        task_id, e
                    );
                    return;
                }
            };

            match self.storage.store(&key, &mode, &task_entry).await {
                Ok(_) => return,
                Err(pdk::data_storage::DataStorageError::CasMismatch) => {
                    logger::debug!(
                        "[claude-bridge] persist CAS conflict attempt={} task_id={}, retrying",
                        attempt, task_id
                    );
                }
                Err(e) => {
                    logger::warn!(
                        "[claude-bridge] persist write failed for task_id={}: {:?}",
                        task_id, e
                    );
                    return;
                }
            }
        }
        logger::warn!(
            "[claude-bridge] persist gave up after {} retries for task_id={}",
            MAX_RETRIES, task_id
        );
    }

    /// Overwrite a task record after platform response.
    ///
    /// Indexes are append-only and are not modified here.
    pub async fn update(&self, entry: &TaskEntry) {
        if let Err(e) = self.storage.store(&self.task_key(&entry.task_id), &StoreMode::Always, entry).await {
            logger::warn!(
                "[claude-bridge] task update failed for task_id={}: {:?}",
                entry.task_id,
                e
            );
        }
    }

    /// Direct lookup by `task_id`, fanning out across all known replicas.
    /// Snapshot fields come from the highest-`updated_at` slice; `history` is
    /// every slice's turns merged and ordered by `ts`. `None` if missing everywhere.
    pub async fn load(&self, task_id: &str) -> Option<TaskEntry> {
        let replicas = replica_registry::list_replicas(self.storage).await;
        let mut best: Option<TaskEntry> = None;
        let mut history: Vec<Message> = Vec::new();
        for replica in &replicas {
            let key = self.task_key_for(replica, task_id);
            match self.storage.get::<TaskEntry>(&key).await {
                Ok(Some((entry, _version))) => {
                    history.extend(entry.history.iter().cloned());
                    let is_better = best.as_ref().map_or(true, |b| entry.updated_at > b.updated_at);
                    if is_better {
                        best = Some(entry);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    logger::warn!(
                        "[claude-bridge] task load failed for replica={} task_id={}: {:?}",
                        replica, task_id, e
                    );
                }
            }
        }
        best.map(|mut entry| {
            history.sort_by_key(|m| m.ts);
            entry.history = history;
            entry
        })
    }

    /// Load all tasks for a conversation. Serves `tasks/list` with `contextId`.
    pub async fn list(&self, context_id: &str) -> Vec<TaskEntry> {
        let task_ids = self.collect_index_across_replicas(context_id, "ctx").await;
        self.load_tasks(task_ids).await
    }

    /// Load all tasks owned by a user. Serves `tasks/list` without `contextId`.
    pub async fn list_by_user(&self, user_id: &str) -> Vec<TaskEntry> {
        let task_ids = self.collect_index_across_replicas(user_id, "user").await;
        self.load_tasks(task_ids).await
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Fan out across all replicas for either `ctx` or `user` index, deduplicate task IDs.
    async fn collect_index_across_replicas(&self, id: &str, index_type: &str) -> Vec<String> {
        let replicas = replica_registry::list_replicas(self.storage).await;
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for replica in &replicas {
            let key = format!("pod:{}:{}:{}", replica, index_type, id);
            let ids = self.read_index(&key, id, index_type).await;
            for task_id in ids {
                if seen.insert(task_id.clone()) {
                    result.push(task_id);
                }
            }
        }
        result
    }

    async fn append_to_index(&self, key: &str, index_key: &str, task_id: &str, label: &str) {
        const MAX_RETRIES: u32 = 3;
        for attempt in 0..MAX_RETRIES {
            let (mut task_ids, version) = match self.storage.get::<Vec<String>>(key).await {
                Ok(Some((ids, ver))) => (ids, Some(ver)),
                Ok(None) => (Vec::new(), None),
                Err(e) => {
                    logger::warn!(
                        "[claude-bridge] {} read failed for key={}: {:?}",
                        label, index_key, e
                    );
                    return;
                }
            };

            task_ids.push(task_id.to_string());

            let mode = match version {
                Some(ver) => StoreMode::Cas(ver),
                None => StoreMode::Absent,
            };

            match self.storage.store(key, &mode, &task_ids).await {
                Ok(_) => return,
                Err(pdk::data_storage::DataStorageError::CasMismatch) => {
                    logger::debug!(
                        "[claude-bridge] {} CAS conflict attempt={} key={}, retrying",
                        label, attempt, index_key
                    );
                }
                Err(e) => {
                    logger::warn!(
                        "[claude-bridge] {} write failed for key={}: {:?}",
                        label, index_key, e
                    );
                    return;
                }
            }
        }
        logger::warn!(
            "[claude-bridge] {} gave up after {} retries for key={}",
            label, MAX_RETRIES, index_key
        );
    }

    async fn read_index(&self, key: &str, index_key: &str, label: &str) -> Vec<String> {
        match self.storage.get::<Vec<String>>(key).await {
            Ok(Some((ids, _))) => ids,
            Ok(None) => vec![],
            Err(e) => {
                logger::warn!(
                    "[claude-bridge] {} read failed for key={}: {:?}",
                    label, index_key, e
                );
                vec![]
            }
        }
    }

    async fn load_tasks(&self, task_ids: Vec<String>) -> Vec<TaskEntry> {
        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            if let Some(entry) = self.load(&task_id).await {
                tasks.push(entry);
            }
        }
        tasks
    }

    fn task_key(&self, task_id: &str) -> String {
        format!("pod:{}:task:{}", replica_registry::replica_id(), task_id)
    }

    fn task_key_for(&self, replica_id: &str, task_id: &str) -> String {
        format!("pod:{}:task:{}", replica_id, task_id)
    }

    fn ctx_index_key(&self, context_id: &str) -> String {
        format!("pod:{}:ctx:{}", replica_registry::replica_id(), context_id)
    }

    fn user_index_key(&self, user_id: &str) -> String {
        format!("pod:{}:user:{}", replica_registry::replica_id(), user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica_registry;
    use pdk::data_storage::{DataStorageError, StoreMode};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use bincode;

    // ── Mock DataStorage ──────────────────────────────────────────────────────

    struct MockStorage {
        data: Mutex<HashMap<String, (Vec<u8>, u64)>>,
    }

    impl MockStorage {
        fn new() -> Self {
            Self { data: Mutex::new(HashMap::new()) }
        }

        /// Directly insert a serialized value, bypassing StoreMode checks.
        fn raw_insert<T: serde::Serialize>(&self, key: &str, value: &T) {
            let bytes = bincode::serialize(value).unwrap();
            let mut map = self.data.lock().unwrap();
            map.insert(key.to_string(), (bytes, 1));
        }
    }

    impl DataStorage for MockStorage {
        async fn get_keys(&self) -> Result<Vec<String>, DataStorageError> {
            Ok(self.data.lock().unwrap().keys().cloned().collect())
        }

        async fn store<T: serde::Serialize>(
            &self,
            key: &str,
            mode: &StoreMode,
            item: &T,
        ) -> Result<(), DataStorageError> {
            let bytes = bincode::serialize(item)
                .map_err(|e| DataStorageError::Unexpected(e.to_string()))?;
            let mut map = self.data.lock().unwrap();
            match mode {
                StoreMode::Always => {
                    let v = map.get(key).map(|(_, v)| v + 1).unwrap_or(1);
                    map.insert(key.to_string(), (bytes, v));
                    Ok(())
                }
                StoreMode::Absent => {
                    if map.contains_key(key) {
                        Err(DataStorageError::CasMismatch)
                    } else {
                        map.insert(key.to_string(), (bytes, 1));
                        Ok(())
                    }
                }
                StoreMode::Cas(expected) => {
                    let expected: u64 = expected.parse().unwrap_or(0);
                    match map.get(key) {
                        Some((_, v)) if *v == expected => {
                            map.insert(key.to_string(), (bytes, expected + 1));
                            Ok(())
                        }
                        _ => Err(DataStorageError::CasMismatch),
                    }
                }
            }
        }

        async fn get<T: serde::de::DeserializeOwned>(
            &self,
            key: &str,
        ) -> Result<Option<(T, String)>, DataStorageError> {
            let map = self.data.lock().unwrap();
            match map.get(key) {
                Some((bytes, v)) => {
                    let item: T = bincode::deserialize(bytes)
                        .map_err(|e| DataStorageError::Unexpected(e.to_string()))?;
                    Ok(Some((item, v.to_string())))
                }
                None => Ok(None),
            }
        }

        async fn delete(&self, key: &str) -> Result<(), DataStorageError> {
            self.data.lock().unwrap().remove(key);
            Ok(())
        }

        async fn delete_all(&self) -> Result<(), DataStorageError> {
            self.data.lock().unwrap().clear();
            Ok(())
        }
    }

    // ── Failing storage mock ──────────────────────────────────────────────────

    struct FailingStorage;

    impl DataStorage for FailingStorage {
        async fn get_keys(&self) -> Result<Vec<String>, DataStorageError> {
            Err(DataStorageError::Unexpected("fail".into()))
        }
        async fn store<T: serde::Serialize>(
            &self, _key: &str, _mode: &StoreMode, _item: &T,
        ) -> Result<(), DataStorageError> {
            Err(DataStorageError::Unexpected("fail".into()))
        }
        async fn get<T: serde::de::DeserializeOwned>(
            &self, _key: &str,
        ) -> Result<Option<(T, String)>, DataStorageError> {
            Err(DataStorageError::Unexpected("fail".into()))
        }
        async fn delete(&self, _key: &str) -> Result<(), DataStorageError> {
            Err(DataStorageError::Unexpected("fail".into()))
        }
        async fn delete_all(&self) -> Result<(), DataStorageError> {
            Err(DataStorageError::Unexpected("fail".into()))
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_entry(task_id: &str, context_id: &str, user_id: &str) -> TaskEntry {
        TaskEntry {
            task_id: task_id.to_string(),
            context_id: context_id.to_string(),
            user_id: user_id.to_string(),
            message_id: "msg-1".to_string(),
            status: TaskStatus { state: TaskState::Submitted, message: None, timestamp: 0 },
            artifacts: vec![],
            history: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }

    fn make_entry_with_updated_at(task_id: &str, context_id: &str, user_id: &str, updated_at: u64) -> TaskEntry {
        let mut e = make_entry(task_id, context_id, user_id);
        e.updated_at = updated_at;
        e
    }

    // ── create ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_returns_true_for_new_task() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        assert!(store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await);
    }

    #[tokio::test]
    async fn create_returns_false_when_task_already_exists() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        assert!(!store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await);
    }

    #[tokio::test]
    async fn create_writes_ctx_index() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        store.create(&make_entry("task-bbb", "ctx-1", "user-1")).await;
        assert_eq!(store.list("ctx-1").await.len(), 2);
    }

    #[tokio::test]
    async fn create_writes_user_index() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        store.create(&make_entry("task-bbb", "ctx-2", "user-1")).await;
        assert_eq!(store.list_by_user("user-1").await.len(), 2);
    }

    #[tokio::test]
    async fn create_returns_false_on_storage_error() {
        let storage = FailingStorage;
        let store = TaskStore::new(&storage);
        assert!(!store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await);
    }

    // ── load ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn load_returns_entry_after_create() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        let loaded = store.load("task-aaa").await.unwrap();
        assert_eq!(loaded.task_id, "task-aaa");
        assert_eq!(loaded.context_id, "ctx-1");
        assert_eq!(loaded.user_id, "user-1");
    }

    #[tokio::test]
    async fn load_returns_none_for_missing_task() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        assert!(store.load("task-missing").await.is_none());
    }

    #[tokio::test]
    async fn load_returns_none_on_storage_error() {
        let storage = FailingStorage;
        let store = TaskStore::new(&storage);
        assert!(store.load("task-aaa").await.is_none());
    }

    // ── update ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_overwrites_task_record() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        let mut updated = make_entry("task-aaa", "ctx-1", "user-1");
        updated.status.state = TaskState::Completed;
        store.update(&updated).await;
        assert_eq!(store.load("task-aaa").await.unwrap().status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn update_does_not_modify_indexes() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        let mut updated = make_entry("task-aaa", "ctx-1", "user-1");
        updated.status.state = TaskState::Completed;
        store.update(&updated).await;
        assert_eq!(store.list("ctx-1").await.len(), 1);
    }

    #[tokio::test]
    async fn update_does_not_panic_on_storage_error() {
        let storage = FailingStorage;
        let store = TaskStore::new(&storage);
        store.update(&make_entry("task-aaa", "ctx-1", "user-1")).await;
    }

    // ── list ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_returns_empty_for_unknown_context() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        assert!(store.list("ctx-unknown").await.is_empty());
    }

    #[tokio::test]
    async fn list_returns_only_tasks_for_given_context() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        store.create(&make_entry("task-bbb", "ctx-2", "user-1")).await;
        let tasks = store.list("ctx-1").await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "task-aaa");
    }

    // ── list_by_user ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_by_user_returns_empty_for_unknown_user() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        assert!(store.list_by_user("user-unknown").await.is_empty());
    }

    #[tokio::test]
    async fn list_by_user_returns_only_tasks_for_given_user() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        store.create(&make_entry("task-bbb", "ctx-2", "user-2")).await;
        let tasks = store.list_by_user("user-1").await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "task-aaa");
    }

    #[tokio::test]
    async fn list_by_user_spans_multiple_contexts() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        store.create(&make_entry("task-bbb", "ctx-2", "user-1")).await;
        store.create(&make_entry("task-ccc", "ctx-3", "user-2")).await;
        assert_eq!(store.list_by_user("user-1").await.len(), 2);
    }

    // ── persist ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn persist_stamps_created_updated_and_status_timestamp_on_first_write() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);

        let returned = store
            .persist("task-p1", "ctx-1", "user-1", TaskState::Completed, None, vec![], vec![])
            .await;

        let entry = store.load("task-p1").await.unwrap();
        assert_ne!(returned, 0, "persist must stamp a real clock value, not the 0 sentinel");
        assert_eq!(entry.created_at, returned);
        assert_eq!(entry.updated_at, returned);
        assert_eq!(entry.status.timestamp, returned);
    }

    #[tokio::test]
    async fn persist_preserves_created_at_and_advances_updated_at_on_rewrite() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);

        store
            .persist("task-p2", "ctx-1", "user-1", TaskState::InputRequired, None, vec![], vec![])
            .await;
        let first = store.load("task-p2").await.unwrap();

        // Re-persist the same task_id, as a HITL resume completing the task does.
        store
            .persist("task-p2", "ctx-1", "user-1", TaskState::Completed, None, vec![], vec![])
            .await;
        let second = store.load("task-p2").await.unwrap();

        assert_eq!(second.created_at, first.created_at, "created_at stamped once, preserved across updates");
        assert!(second.updated_at >= first.updated_at, "updated_at advances (>= : clock is second-granular)");
        assert_eq!(second.status.timestamp, second.updated_at, "status.timestamp mirrors updated_at");
        assert_eq!(second.status.state, TaskState::Completed, "re-persist overwrote the state");
    }

    // ── key format ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn key_format_uses_pod_prefix() {
        let storage = MockStorage::new();
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        let replica = replica_registry::replica_id();
        let keys = storage.data.lock().unwrap();
        assert!(keys.contains_key(&format!("pod:{}:task:task-aaa", replica)));
        assert!(keys.contains_key(&format!("pod:{}:ctx:ctx-1", replica)));
        assert!(keys.contains_key(&format!("pod:{}:user:user-1", replica)));
    }

    // ── replica registration ──────────────────────────────────────────────────

    #[tokio::test]
    async fn replica_registration_adds_replica_to_registry() {
        let storage = MockStorage::new();
        replica_registry::register(&storage).await;
        let replicas: Vec<String> = storage.get::<Vec<String>>(replica_registry::REGISTRY_KEY).await
            .unwrap().unwrap().0;
        assert!(replicas.contains(&replica_registry::replica_id().to_string()));
    }

    #[tokio::test]
    async fn replica_registration_is_idempotent() {
        let storage = MockStorage::new();
        replica_registry::register(&storage).await;
        replica_registry::register(&storage).await;
        replica_registry::register(&storage).await;
        let replicas: Vec<String> = storage.get::<Vec<String>>(replica_registry::REGISTRY_KEY).await
            .unwrap().unwrap().0;
        let count = replicas.iter().filter(|r| r.as_str() == replica_registry::replica_id()).count();
        assert_eq!(count, 1, "replica should appear exactly once after multiple registrations");
    }

    // ── cross-replica load ────────────────────────────────────────────────────

    #[tokio::test]
    async fn load_returns_highest_updated_at_across_replicas() {
        // Simulate two pods sharing the same MockStorage (shared storage backend).
        // We seed data directly to simulate what pod-A and pod-B would have written.
        let storage = MockStorage::new();

        // Seed registry with two replicas
        let replicas = vec!["pod-A".to_string(), "pod-B".to_string()];
        storage.raw_insert(replica_registry::REGISTRY_KEY, &replicas);

        // pod-A wrote task with updated_at=100
        let entry_a = make_entry_with_updated_at("task-aaa", "ctx-1", "user-1", 100);
        storage.raw_insert("pod:pod-A:task:task-aaa", &entry_a);

        // pod-B wrote same task with updated_at=200 (newer)
        let entry_b = make_entry_with_updated_at("task-aaa", "ctx-1", "user-1", 200);
        storage.raw_insert("pod:pod-B:task:task-aaa", &entry_b);

        let store = TaskStore::new(&storage);
        let loaded = store.load("task-aaa").await.unwrap();
        assert_eq!(loaded.updated_at, 200, "should return the entry with the highest updated_at");
    }

    #[tokio::test]
    async fn load_returns_task_from_other_replicas_bucket() {
        // pod-B has the task; current pod (pod-default) does not.
        let storage = MockStorage::new();

        let replicas = vec!["pod-B".to_string(), replica_registry::replica_id().to_string()];
        storage.raw_insert(replica_registry::REGISTRY_KEY, &replicas);

        let entry = make_entry("task-aaa", "ctx-1", "user-1");
        storage.raw_insert("pod:pod-B:task:task-aaa", &entry);

        let store = TaskStore::new(&storage);
        let loaded = store.load("task-aaa").await;
        assert!(loaded.is_some(), "should find the task in pod-B's bucket");
        assert_eq!(loaded.unwrap().task_id, "task-aaa");
    }

    // ── append_to_index CAS retry ─────────────────────────────────────────────

    /// Storage that rejects the first `conflicts` CAS writes with `CasMismatch`
    /// before accepting. Models N concurrent workers each racing to update the
    /// same index key.
    struct ConflictingStorage {
        inner: MockStorage,
        conflicts_remaining: Mutex<u32>,
    }

    impl ConflictingStorage {
        fn new(conflicts: u32) -> Self {
            Self { inner: MockStorage::new(), conflicts_remaining: Mutex::new(conflicts) }
        }
    }

    impl DataStorage for ConflictingStorage {
        async fn get_keys(&self) -> Result<Vec<String>, DataStorageError> {
            self.inner.get_keys().await
        }
        async fn store<T: serde::Serialize>(
            &self, key: &str, mode: &StoreMode, item: &T,
        ) -> Result<(), DataStorageError> {
            // Only inject CAS conflicts on index keys (contain ":ctx:" or ":user:"),
            // not on task record writes (":task:"). This isolates the test to
            // append_to_index retry behaviour without breaking create().
            let is_index_key = key.contains(":ctx:") || key.contains(":user:");
            if is_index_key && matches!(mode, StoreMode::Cas(_) | StoreMode::Absent) {
                let mut rem = self.conflicts_remaining.lock().unwrap();
                if *rem > 0 {
                    *rem -= 1;
                    // Simulate another worker winning the race: write item unconditionally
                    // so the version bumps. The retry re-reads a fresh version and succeeds.
                    let _ = self.inner.store(key, &StoreMode::Always, item).await;
                    return Err(DataStorageError::CasMismatch);
                }
            }
            self.inner.store(key, mode, item).await
        }
        async fn get<T: serde::de::DeserializeOwned>(
            &self, key: &str,
        ) -> Result<Option<(T, String)>, DataStorageError> {
            self.inner.get(key).await
        }
        async fn delete(&self, key: &str) -> Result<(), DataStorageError> {
            self.inner.delete(key).await
        }
        async fn delete_all(&self) -> Result<(), DataStorageError> {
            self.inner.delete_all().await
        }
    }

    #[tokio::test]
    async fn append_to_index_succeeds_after_one_cas_conflict() {
        // Simulates one concurrent writer beating us — retry must recover and
        // the task must appear in the index.
        let storage = ConflictingStorage::new(1);
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        assert_eq!(store.list("ctx-1").await.len(), 1);
    }

    #[tokio::test]
    async fn append_to_index_succeeds_after_two_cas_conflicts() {
        // Two concurrent writers both beat us — two retries must be enough.
        let storage = ConflictingStorage::new(2);
        let store = TaskStore::new(&storage);
        store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        assert_eq!(store.list("ctx-1").await.len(), 1);
    }

    #[tokio::test]
    async fn append_to_index_gives_up_after_max_retries_exceeded() {
        // More conflicts than MAX_RETRIES (3) — the append is silently dropped.
        // The task record itself must still be written (create returns true).
        let storage = ConflictingStorage::new(4);
        let store = TaskStore::new(&storage);
        let created = store.create(&make_entry("task-aaa", "ctx-1", "user-1")).await;
        assert!(created, "task record must be written even when index append fails");
        // Index entry is lost, but the call must not panic or return an error.
        // list() may return 0 or 1 depending on which retry count got through.
        // The key assertion is: no panic, create returned true.
    }

    // ── cross-replica list deduplication ─────────────────────────────────────

    #[tokio::test]
    async fn list_deduplicates_across_replicas() {
        // task-aaa appears in both pod-A and pod-B's ctx index — should be returned once.
        let storage = MockStorage::new();

        let replicas = vec!["pod-A".to_string(), "pod-B".to_string()];
        storage.raw_insert(replica_registry::REGISTRY_KEY, &replicas);

        let task_ids_a: Vec<String> = vec!["task-aaa".to_string()];
        let task_ids_b: Vec<String> = vec!["task-aaa".to_string()];
        storage.raw_insert("pod:pod-A:ctx:ctx-1", &task_ids_a);
        storage.raw_insert("pod:pod-B:ctx:ctx-1", &task_ids_b);

        let entry = make_entry("task-aaa", "ctx-1", "user-1");
        storage.raw_insert("pod:pod-A:task:task-aaa", &entry);

        let store = TaskStore::new(&storage);
        let tasks = store.list("ctx-1").await;
        assert_eq!(tasks.len(), 1, "task-aaa should appear only once despite being in both indexes");
    }

    #[tokio::test]
    async fn list_returns_union_of_tasks_across_replicas() {
        // pod-A has task-aaa, pod-B has task-bbb, same contextId.
        let storage = MockStorage::new();

        let replicas = vec!["pod-A".to_string(), "pod-B".to_string()];
        storage.raw_insert(replica_registry::REGISTRY_KEY, &replicas);

        let task_ids_a: Vec<String> = vec!["task-aaa".to_string()];
        let task_ids_b: Vec<String> = vec!["task-bbb".to_string()];
        storage.raw_insert("pod:pod-A:ctx:ctx-1", &task_ids_a);
        storage.raw_insert("pod:pod-B:ctx:ctx-1", &task_ids_b);

        let entry_a = make_entry("task-aaa", "ctx-1", "user-1");
        let entry_b = make_entry("task-bbb", "ctx-1", "user-1");
        storage.raw_insert("pod:pod-A:task:task-aaa", &entry_a);
        storage.raw_insert("pod:pod-B:task:task-bbb", &entry_b);

        let store = TaskStore::new(&storage);
        let tasks = store.list("ctx-1").await;
        assert_eq!(tasks.len(), 2, "should return tasks from both replicas");
        let ids: Vec<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
        assert!(ids.contains(&"task-aaa"));
        assert!(ids.contains(&"task-bbb"));
    }
}
