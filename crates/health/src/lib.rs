//! Runtime health supervision and durable health snapshots.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use thiserror::Error;
use tv_bot_core_types::SystemHealthSnapshot;
use tv_bot_persistence::{PersistenceError, RuntimePersistence, SystemHealthStore};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeHealthInputs {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub reconnect_count: u64,
    pub feed_degraded: bool,
}

#[derive(Debug, Error)]
pub enum RuntimeHealthError {
    #[error("health persistence failed: {source}")]
    Persistence {
        #[source]
        source: PersistenceError,
    },
    #[error("health supervisor lock is poisoned")]
    Poisoned,
}

#[derive(Clone)]
pub struct RuntimeHealthSupervisor {
    store: Arc<dyn SystemHealthStore>,
    state: Arc<Mutex<RuntimeHealthState>>,
}

#[derive(Clone, Debug, Default)]
struct RuntimeHealthState {
    latest_snapshot: Option<SystemHealthSnapshot>,
    error_count: u64,
    db_write_latency_ms: Option<u64>,
    queue_lag_ms: Option<u64>,
}

impl RuntimeHealthSupervisor {
    pub fn from_persistence(persistence: &RuntimePersistence) -> Result<Self, RuntimeHealthError> {
        let store = persistence.system_health_store();
        let existing_snapshots = store
            .list_system_health()
            .map_err(|source| RuntimeHealthError::Persistence { source })?;
        let latest_snapshot = existing_snapshots.last().cloned();

        Ok(Self {
            store,
            state: Arc::new(Mutex::new(RuntimeHealthState {
                error_count: latest_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.error_count)
                    .unwrap_or(0),
                db_write_latency_ms: latest_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.db_write_latency_ms),
                queue_lag_ms: latest_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.queue_lag_ms),
                latest_snapshot,
            })),
        })
    }

    pub fn note_error(&self) -> Result<u64, RuntimeHealthError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeHealthError::Poisoned)?;
        state.error_count = state.error_count.saturating_add(1);
        Ok(state.error_count)
    }

    pub fn record_db_write_latency(&self, latency_ms: u64) -> Result<(), RuntimeHealthError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeHealthError::Poisoned)?;
        state.db_write_latency_ms = Some(latency_ms);
        Ok(())
    }

    pub fn record_queue_lag(&self, lag_ms: u64) -> Result<(), RuntimeHealthError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeHealthError::Poisoned)?;
        state.queue_lag_ms = Some(lag_ms);
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Option<SystemHealthSnapshot>, RuntimeHealthError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RuntimeHealthError::Poisoned)?;
        Ok(state.latest_snapshot.clone())
    }

    pub fn capture(
        &self,
        inputs: RuntimeHealthInputs,
        updated_at: DateTime<Utc>,
    ) -> Result<Option<SystemHealthSnapshot>, RuntimeHealthError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeHealthError::Poisoned)?;

        let snapshot = SystemHealthSnapshot {
            cpu_percent: inputs.cpu_percent,
            memory_bytes: inputs.memory_bytes,
            reconnect_count: inputs.reconnect_count,
            db_write_latency_ms: state.db_write_latency_ms,
            queue_lag_ms: state.queue_lag_ms,
            error_count: state.error_count,
            feed_degraded: inputs.feed_degraded,
            updated_at,
        };

        if state
            .latest_snapshot
            .as_ref()
            .is_some_and(|latest| same_health_signature(latest, &snapshot))
        {
            return Ok(None);
        }

        self.store
            .append_system_health(snapshot.clone())
            .map_err(|source| RuntimeHealthError::Persistence { source })?;
        state.latest_snapshot = Some(snapshot.clone());

        Ok(Some(snapshot))
    }
}

fn same_health_signature(left: &SystemHealthSnapshot, right: &SystemHealthSnapshot) -> bool {
    left.cpu_percent == right.cpu_percent
        && left.memory_bytes == right.memory_bytes
        && left.reconnect_count == right.reconnect_count
        && left.db_write_latency_ms == right.db_write_latency_ms
        && left.queue_lag_ms == right.queue_lag_ms
        && left.error_count == right.error_count
        && left.feed_degraded == right.feed_degraded
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tv_bot_config::{AppConfig, ConfigError, MapEnvironment};

    use super::*;

    fn load_test_config(values: &[(&str, &str)]) -> Result<AppConfig, ConfigError> {
        let env = values
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>();

        AppConfig::load(None, &MapEnvironment::new(env))
    }

    #[test]
    fn health_supervisor_persists_first_snapshot() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");
        let persistence = RuntimePersistence::open(&config);
        let supervisor = RuntimeHealthSupervisor::from_persistence(&persistence)
            .expect("health supervisor should initialize");
        let updated_at = Utc::now();

        let snapshot = supervisor
            .capture(
                RuntimeHealthInputs {
                    cpu_percent: Some(12.5),
                    memory_bytes: Some(2_048),
                    reconnect_count: 1,
                    feed_degraded: false,
                },
                updated_at,
            )
            .expect("health capture should succeed")
            .expect("first snapshot should persist");

        assert_eq!(snapshot.reconnect_count, 1);
        assert_eq!(snapshot.cpu_percent, Some(12.5));
        assert_eq!(
            persistence
                .system_health_store()
                .list_system_health()
                .expect("health snapshots should list"),
            vec![snapshot]
        );
    }

    #[test]
    fn health_supervisor_skips_unchanged_snapshots() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");
        let persistence = RuntimePersistence::open(&config);
        let supervisor = RuntimeHealthSupervisor::from_persistence(&persistence)
            .expect("health supervisor should initialize");
        let updated_at = Utc::now();

        supervisor
            .capture(
                RuntimeHealthInputs {
                    cpu_percent: None,
                    memory_bytes: None,
                    reconnect_count: 0,
                    feed_degraded: false,
                },
                updated_at,
            )
            .expect("health capture should succeed");

        let repeated = supervisor
            .capture(
                RuntimeHealthInputs {
                    cpu_percent: None,
                    memory_bytes: None,
                    reconnect_count: 0,
                    feed_degraded: false,
                },
                updated_at + chrono::Duration::seconds(1),
            )
            .expect("health capture should succeed");

        assert!(repeated.is_none());
        assert_eq!(
            persistence
                .system_health_store()
                .list_system_health()
                .expect("health snapshots should list")
                .len(),
            1
        );
    }

    #[test]
    fn health_supervisor_includes_runtime_error_and_db_latency_counters() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");
        let persistence = RuntimePersistence::open(&config);
        let supervisor = RuntimeHealthSupervisor::from_persistence(&persistence)
            .expect("health supervisor should initialize");

        supervisor
            .note_error()
            .expect("error count should increment");
        supervisor
            .record_db_write_latency(17)
            .expect("db latency should record");
        supervisor
            .record_queue_lag(4)
            .expect("queue lag should record");

        let snapshot = supervisor
            .capture(
                RuntimeHealthInputs {
                    cpu_percent: None,
                    memory_bytes: None,
                    reconnect_count: 2,
                    feed_degraded: true,
                },
                Utc::now(),
            )
            .expect("health capture should succeed")
            .expect("health snapshot should persist");

        assert_eq!(snapshot.error_count, 1);
        assert_eq!(snapshot.db_write_latency_ms, Some(17));
        assert_eq!(snapshot.queue_lag_ms, Some(4));
        assert!(snapshot.feed_degraded);
    }

    #[test]
    fn health_supervisor_hydrates_existing_snapshot_state() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");
        let persistence = RuntimePersistence::open(&config);
        let existing = SystemHealthSnapshot {
            cpu_percent: Some(9.0),
            memory_bytes: Some(4_096),
            reconnect_count: 3,
            db_write_latency_ms: Some(5),
            queue_lag_ms: Some(1),
            error_count: 2,
            feed_degraded: true,
            updated_at: Utc::now(),
        };
        persistence
            .system_health_store()
            .append_system_health(existing.clone())
            .expect("existing health should append");

        let supervisor = RuntimeHealthSupervisor::from_persistence(&persistence)
            .expect("health supervisor should initialize");
        assert_eq!(
            supervisor.snapshot().expect("snapshot should load"),
            Some(existing)
        );
    }
}
