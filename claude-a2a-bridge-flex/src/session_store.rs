// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Per-context session state for this bridge policy.
//!
//! `SessionEntry<T>` holds the common `created_at` field plus a generic
//! `platform` field for policy-specific data (e.g. the upstream
//! session ID + sequence number).
//!
//! # Storage layout
//! One `RemoteDataStorage` instance is created per agent via
//! `store_builder.remote("claude-bridge-{agentId}", ttl_millis)`.
//! The bucket is already isolated per agent. Keys are prefixed to avoid
//! collisions with task store keys in the same bucket:
//!
//! `session:{contextId}`
//!
//! The store is already scoped to `{policy_name}` by the PDK.

use pdk::data_storage::{DataStorage, StoreMode};
use pdk::logger;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Session state entry stored per `contextId`.
///
/// `T` is the platform-specific session data, treated here as opaque.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionEntry<T> {
    /// Unix timestamp (seconds) when this session was first created. Immutable.
    pub created_at: u64,

    /// Platform-specific session fields (e.g. upstream session ID + sequence number).
    pub platform: T,
}

/// Shared access layer for session state, parameterised over the platform blob `T`.
///
/// The storage bucket is already isolated per agent by the caller. Keys are prefixed
/// as `session:{contextId}` to avoid collisions with task store keys in the same bucket.
pub struct SessionStore<'a, S: DataStorage> {
    storage: &'a S,
}

impl<'a, S: DataStorage> SessionStore<'a, S> {
    /// Construct a `SessionStore`.
    pub fn new(storage: &'a S) -> Self {
        Self { storage }
    }

    /// Load a session entry for the given `context_id`.
    ///
    /// Returns `None` if missing or corrupt.
    pub async fn load<T>(&self, context_id: &str) -> Option<SessionEntry<T>>
    where
        T: DeserializeOwned,
    {
        match self.storage.get::<SessionEntry<T>>(&self.session_key(context_id)).await {
            Ok(Some((entry, _version))) => Some(entry),
            Ok(None) => None,
            Err(e) => {
                logger::warn!(
                    "[claude-bridge] session load failed for context_id={}: {:?}",
                    context_id,
                    e
                );
                None
            }
        }
    }

    /// First write only. Returns `false` if a session already exists for this `context_id`.
    pub async fn create<T>(&self, context_id: &str, entry: &SessionEntry<T>) -> bool
    where
        T: Serialize,
    {
        match self.storage.store(&self.session_key(context_id), &StoreMode::Absent, entry).await {
            Ok(_) => true,
            Err(e) => {
                logger::warn!(
                    "[claude-bridge] session create failed (already exists?) for context_id={}: {:?}",
                    context_id,
                    e
                );
                false
            }
        }
    }

    /// Unconditional overwrite. Last-write-wins across nodes.
    pub async fn save<T>(&self, context_id: &str, entry: &SessionEntry<T>)
    where
        T: Serialize,
    {
        if let Err(e) = self.storage.store(&self.session_key(context_id), &StoreMode::Always, entry).await {
            logger::warn!(
                "[claude-bridge] session save failed for context_id={}: {:?}",
                context_id,
                e
            );
        }
    }

    /// Deletes the session record for the given `context_id`.
    ///
    /// Tasks are stored under separate key prefixes and must be cleaned up
    /// independently via `TaskStore`.
    pub async fn delete(&self, context_id: &str) {
        if let Err(e) = self.storage.delete(&self.session_key(context_id)).await {
            logger::warn!(
                "[claude-bridge] session delete failed for context_id={}: {:?}",
                context_id,
                e
            );
        }
    }

    fn session_key(&self, context_id: &str) -> String {
        format!("session:{}", context_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdk::data_storage::{DataStorageError, StoreMode};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use bincode;

    // ── Minimal platform blob used across all tests ───────────────────────────

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct TestPlatform {
        session_id: String,
        sequence_number: u32,
    }

    fn platform(id: &str, seq: u32) -> TestPlatform {
        TestPlatform { session_id: id.to_string(), sequence_number: seq }
    }

    fn entry(id: &str, seq: u32) -> SessionEntry<TestPlatform> {
        SessionEntry { created_at: 0, platform: platform(id, seq) }
    }

    // ── CAS-aware mock DataStorage ────────────────────────────────────────────

    struct MockStorage {
        data: Mutex<HashMap<String, (Vec<u8>, u64)>>,
    }

    impl MockStorage {
        fn new() -> Self {
            Self { data: Mutex::new(HashMap::new()) }
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

    // ── load ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn load_returns_none_for_missing_key() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        assert!(store.load::<TestPlatform>("ctx-unknown").await.is_none());
    }

    #[tokio::test]
    async fn load_returns_entry_after_create() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-1", &entry("sess-1", 0)).await;
        let loaded = store.load::<TestPlatform>("ctx-1").await.unwrap();
        assert_eq!(loaded.platform, platform("sess-1", 0));
    }

    #[tokio::test]
    async fn load_returns_none_on_storage_error() {
        let storage = FailingStorage;
        let store = SessionStore::new(&storage);
        assert!(store.load::<TestPlatform>("ctx-err").await.is_none());
    }

    // ── create ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_returns_true_for_new_key() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        assert!(store.create("ctx-new", &entry("sess-2", 0)).await);
    }

    #[tokio::test]
    async fn create_returns_false_when_key_already_exists() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-dup", &entry("sess-3", 0)).await;
        assert!(!store.create("ctx-dup", &entry("sess-3b", 0)).await);
    }

    #[tokio::test]
    async fn create_does_not_overwrite_existing_entry() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-noover", &entry("sess-orig", 0)).await;
        store.create("ctx-noover", &entry("sess-replaced", 99)).await;
        let loaded = store.load::<TestPlatform>("ctx-noover").await.unwrap();
        assert_eq!(loaded.platform.session_id, "sess-orig");
    }

    #[tokio::test]
    async fn create_returns_false_on_storage_error() {
        let storage = FailingStorage;
        let store = SessionStore::new(&storage);
        assert!(!store.create("ctx-fail", &entry("s", 0)).await);
    }

    // ── save ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn save_persists_entry() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.save("ctx-save", &entry("sess-4", 0)).await;
        assert!(store.load::<TestPlatform>("ctx-save").await.is_some());
    }

    #[tokio::test]
    async fn save_overwrites_existing_entry() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-upd", &entry("sess-5", 0)).await;
        store.save("ctx-upd", &entry("sess-5", 2)).await;
        let loaded = store.load::<TestPlatform>("ctx-upd").await.unwrap();
        assert_eq!(loaded.platform.sequence_number, 2);
    }

    #[tokio::test]
    async fn save_does_not_panic_on_storage_error() {
        let storage = FailingStorage;
        let store = SessionStore::new(&storage);
        store.save("ctx-fail", &entry("s", 0)).await;
    }

    // ── delete ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_entry() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-del", &entry("sess-6", 0)).await;
        store.delete("ctx-del").await;
        assert!(store.load::<TestPlatform>("ctx-del").await.is_none());
    }

    #[tokio::test]
    async fn delete_does_not_panic_on_storage_error() {
        let storage = FailingStorage;
        let store = SessionStore::new(&storage);
        store.delete("ctx-fail").await;
    }

    // ── key isolation ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn different_context_ids_are_independent() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-a", &entry("sess-a", 0)).await;
        store.create("ctx-b", &entry("sess-b", 0)).await;
        assert_eq!(store.load::<TestPlatform>("ctx-a").await.unwrap().platform.session_id, "sess-a");
        assert_eq!(store.load::<TestPlatform>("ctx-b").await.unwrap().platform.session_id, "sess-b");
    }

    #[tokio::test]
    async fn session_key_uses_session_prefix() {
        let storage = MockStorage::new();
        let store = SessionStore::new(&storage);
        store.create("ctx-key", &entry("s", 0)).await;
        let keys = storage.data.lock().unwrap();
        assert!(keys.contains_key("session:ctx-key"));
    }
}
