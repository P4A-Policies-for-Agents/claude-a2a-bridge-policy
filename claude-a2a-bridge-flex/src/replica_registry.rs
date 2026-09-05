// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Pod replica registry — tracks which replicas have written to the shared store.
//!
//! Call [`register`] once at policy startup (in `configure()`). All stores that
//! use per-replica key buckets call [`list_replicas`] to fan out reads, and
//! [`replica_id`] to scope their write keys.
//!
//! | Key              | Value             |
//! |------------------|-------------------|
//! | `replica-registry` | `Vec<String>` of replica IDs |

use pdk::data_storage::{DataStorage, StoreMode};
use pdk::logger;
use std::sync::OnceLock;
use uuid::Uuid;

pub const REGISTRY_KEY: &str = "replica-registry";

static REPLICA_ID: OnceLock<String> = OnceLock::new();

/// Set this replica's ID from the Flex name, once at startup.
pub fn init_replica_id(flex_name: &str) {
    if !flex_name.is_empty() {
        let _ = REPLICA_ID.set(flex_name.to_string());
    }
}

/// This replica's ID, falling back to a random UUID if unset.
pub fn replica_id() -> &'static str {
    REPLICA_ID.get_or_init(|| Uuid::new_v4().to_string())
}

/// Register this replica in the shared registry.
///
/// Call once at policy startup. Skips the write if this replica ID is already present.
pub async fn register<S: DataStorage>(storage: &S) {
    let id = replica_id().to_string();
    let mut ids = match storage.get::<Vec<String>>(REGISTRY_KEY).await {
        Ok(Some((ids, _))) => ids,
        Ok(None) => Vec::new(),
        Err(e) => {
            logger::warn!("[claude-bridge] replica-registry read failed: {:?}", e);
            return;
        }
    };

    if ids.contains(&id) {
        return;
    }
    ids.push(id);

    if let Err(e) = storage.store(REGISTRY_KEY, &StoreMode::Always, &ids).await {
        logger::warn!("[claude-bridge] replica-registry write failed: {:?}", e);
    }
}

/// Read the full list of registered replica IDs.
///
/// Falls back to `[replica_id()]` on error or empty registry so single-pod
/// deployments work before registration has run.
pub async fn list_replicas<S: DataStorage>(storage: &S) -> Vec<String> {
    match storage.get::<Vec<String>>(REGISTRY_KEY).await {
        Ok(Some((ids, _))) => ids,
        Ok(None) => vec![replica_id().to_string()],
        Err(e) => {
            logger::warn!("[claude-bridge] replica-registry read failed: {:?}", e);
            vec![replica_id().to_string()]
        }
    }
}
