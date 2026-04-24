//! Schema version migration registry.
//!
//! Design: docs/architecture/design/state-schema.md §7.4
//! Issue: alternatefutures/admin#134
//!
//! Migrations are applied at hydration time when schema_version < LATEST_SCHEMA_VERSION.
//! Each migration is additive only (no field removal until a deprecation cycle).

use async_trait::async_trait;
use bytes::Bytes;
use std::collections::BTreeMap;
use tracing::debug;

use crate::error::{StateError, StateResult};
use crate::proto::{AgentStateRecord, WorkingMemoryRecord};
use crate::LATEST_SCHEMA_VERSION;

/// A single schema migration step.
///
/// Migrations must be:
///   - Deterministic (same input always produces same output)
///   - Idempotent (safe to re-apply if migration was partially applied)
///   - Additive only (no field removal until deprecation cycle)
#[async_trait]
pub trait StateMigration: Send + Sync {
    /// Source schema version this migration reads from.
    fn from_version(&self) -> u32;
    /// Target schema version this migration produces.
    fn to_version(&self) -> u32;

    /// Migrate a raw AgentStateRecord proto bytes from `from_version` to `to_version`.
    async fn migrate_agent_state(&self, raw: Bytes) -> StateResult<AgentStateRecord>;

    /// Migrate a raw WorkingMemoryRecord proto bytes (may be None for migrations
    /// that don't touch working memory).
    async fn migrate_working_memory(
        &self,
        raw: Bytes,
    ) -> StateResult<Option<WorkingMemoryRecord>> {
        Ok(None) // default: no-op
    }
}

/// Registry of all known migrations, keyed by from_version.
pub struct MigrationRegistry {
    migrations: BTreeMap<u32, Box<dyn StateMigration>>,
}

impl MigrationRegistry {
    /// Create an empty registry. Register migrations with `register()`.
    pub fn new() -> Self {
        Self {
            migrations: BTreeMap::new(),
        }
    }

    /// Create a registry pre-populated with all known migrations.
    pub fn with_all_migrations() -> Self {
        let mut registry = Self::new();
        // Register migrations here as they are added:
        // registry.register(Box::new(MigrationV1toV2 {}));
        // (No migrations yet — we're at schema_version=1)
        registry
    }

    /// Register a migration step.
    pub fn register(&mut self, migration: Box<dyn StateMigration>) {
        self.migrations.insert(migration.from_version(), migration);
    }

    /// Apply all necessary migrations to bring `raw` from `current_version` to
    /// `LATEST_SCHEMA_VERSION`. Returns the fully-migrated AgentStateRecord.
    pub async fn apply_to_latest(
        &self,
        raw: Bytes,
        current_version: u32,
    ) -> StateResult<AgentStateRecord> {
        if current_version > LATEST_SCHEMA_VERSION {
            return Err(StateError::UnsupportedSchemaVersion {
                record_type: "AgentStateRecord".into(),
                found: current_version,
                max_known: LATEST_SCHEMA_VERSION,
            });
        }

        if current_version == LATEST_SCHEMA_VERSION {
            // No migration needed — decode directly
            use prost::Message;
            return AgentStateRecord::decode(raw).map_err(StateError::ProtoError);
        }

        // Walk the migration chain: current_version → ... → LATEST_SCHEMA_VERSION
        let mut current_raw = raw;
        let mut version = current_version;

        while version < LATEST_SCHEMA_VERSION {
            let migration = self.migrations.get(&version).ok_or_else(|| {
                StateError::UnsupportedSchemaVersion {
                    record_type: "AgentStateRecord".into(),
                    found: version,
                    max_known: LATEST_SCHEMA_VERSION,
                }
            })?;

            debug!("applying migration v{} → v{}", version, migration.to_version());

            let migrated = migration.migrate_agent_state(current_raw).await?;
            use prost::Message;
            current_raw = Bytes::from(migrated.encode_to_vec());
            version = migration.to_version();
        }

        use prost::Message;
        AgentStateRecord::decode(current_raw).map_err(StateError::ProtoError)
    }
}

impl Default for MigrationRegistry {
    fn default() -> Self {
        Self::with_all_migrations()
    }
}
