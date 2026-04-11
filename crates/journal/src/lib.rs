//! Journal interfaces, durable adapters, and live projection helpers.

use std::sync::{Arc, Mutex};

use thiserror::Error;
use tv_bot_core_types::EventJournalRecord;
use tv_bot_persistence::{EventJournalStore, PersistenceError};
use tv_bot_state_store::{EventProjectionStore, StateStoreError};

pub trait EventJournal: Send + Sync {
    fn append(&self, record: EventJournalRecord) -> Result<(), JournalError>;
    fn list(&self) -> Result<Vec<EventJournalRecord>, JournalError>;
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryJournal {
    inner: Arc<Mutex<Vec<EventJournalRecord>>>,
}

impl InMemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) -> Result<(), JournalError> {
        let mut guard = self.lock()?;
        guard.clear();
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Vec<EventJournalRecord>>, JournalError> {
        self.inner.lock().map_err(|_| JournalError::Poisoned)
    }
}

impl EventJournal for InMemoryJournal {
    fn append(&self, record: EventJournalRecord) -> Result<(), JournalError> {
        let mut guard = self.lock()?;
        guard.push(record);
        Ok(())
    }

    fn list(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
        Ok(self.lock()?.clone())
    }
}

#[derive(Clone)]
pub struct PersistentJournal {
    store: Arc<dyn EventJournalStore>,
}

impl PersistentJournal {
    pub fn new(store: Arc<dyn EventJournalStore>) -> Self {
        Self { store }
    }
}

impl EventJournal for PersistentJournal {
    fn append(&self, record: EventJournalRecord) -> Result<(), JournalError> {
        self.store
            .append_event(record)
            .map_err(|source| JournalError::Persistence { source })
    }

    fn list(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
        self.store
            .list_events()
            .map_err(|source| JournalError::Persistence { source })
    }
}

#[derive(Clone)]
pub struct ProjectingJournal<J, S> {
    inner: J,
    projection_store: S,
}

impl<J, S> ProjectingJournal<J, S>
where
    J: EventJournal,
    S: EventProjectionStore,
{
    pub fn new(inner: J, projection_store: S) -> Self {
        Self {
            inner,
            projection_store,
        }
    }

    pub fn with_hydrated_projection(inner: J, projection_store: S) -> Result<Self, JournalError> {
        let journal = Self::new(inner, projection_store);
        let records = journal.inner.list()?;
        journal
            .projection_store
            .rebuild_from_events(&records)
            .map_err(|source| JournalError::StateStore { source })?;
        Ok(journal)
    }

    pub fn projection_store(&self) -> &S {
        &self.projection_store
    }

    pub fn inner(&self) -> &J {
        &self.inner
    }
}

impl<J, S> EventJournal for ProjectingJournal<J, S>
where
    J: EventJournal,
    S: EventProjectionStore,
{
    fn append(&self, record: EventJournalRecord) -> Result<(), JournalError> {
        self.inner.append(record.clone())?;
        self.projection_store
            .apply_event(record)
            .map_err(|source| JournalError::StateStore { source })?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
        self.inner.list()
    }
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal storage lock is poisoned")]
    Poisoned,
    #[error("persistent journal backend failed: {source}")]
    Persistence {
        #[source]
        source: PersistenceError,
    },
    #[error("state projection update failed: {source}")]
    StateStore {
        #[source]
        source: StateStoreError,
    },
}

impl PartialEq for JournalError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Poisoned, Self::Poisoned) => true,
            (Self::Persistence { source: left }, Self::Persistence { source: right }) => {
                left.to_string() == right.to_string()
            }
            (Self::StateStore { source: left }, Self::StateStore { source: right }) => {
                left == right
            }
            _ => false,
        }
    }
}

impl Eq for JournalError {}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use tv_bot_core_types::{ActionSource, EventSeverity};
    use tv_bot_persistence::{EventJournalStore, InMemoryPersistence};
    use tv_bot_state_store::{EventProjectionStore, InMemoryStateStore};

    use super::*;

    fn sample_record(id: &str, action: &str) -> EventJournalRecord {
        EventJournalRecord {
            event_id: id.to_owned(),
            category: "strategy".to_owned(),
            action: action.to_owned(),
            source: ActionSource::System,
            severity: EventSeverity::Info,
            occurred_at: Utc::now(),
            payload: json!({
                "mode": "paper",
                "strategy_id": "gc_momentum_fade_v1",
                "intent": "enter"
            }),
        }
    }

    #[test]
    fn in_memory_journal_appends_and_lists_records() {
        let journal = InMemoryJournal::new();
        let record = sample_record("evt-1", "loaded");

        journal.append(record.clone()).expect("append should work");

        let records = journal.list().expect("list should work");
        assert_eq!(records, vec![record]);
    }

    #[test]
    fn clear_removes_records() {
        let journal = InMemoryJournal::new();
        journal
            .append(sample_record("evt-2", "armed"))
            .expect("append should work");

        journal.clear().expect("clear should work");
        assert!(journal.list().expect("list should work").is_empty());
    }

    #[test]
    fn persistent_journal_delegates_to_event_store() {
        let store = InMemoryPersistence::new();
        let store_for_assert = store.clone();
        let journal = PersistentJournal::new(Arc::new(store));
        let record = sample_record("evt-3", "loaded");

        journal.append(record.clone()).expect("append should work");

        assert_eq!(
            store_for_assert.list_events().expect("events should list"),
            vec![record]
        );
    }

    #[test]
    fn projecting_journal_hydrates_and_updates_state_store() {
        let store = InMemoryPersistence::new();
        store
            .append_event(sample_record("evt-4", "intent_received"))
            .expect("seed record should append");
        let projection = InMemoryStateStore::new();
        let journal = ProjectingJournal::with_hydrated_projection(
            PersistentJournal::new(Arc::new(store.clone())),
            projection.clone(),
        )
        .expect("journal should hydrate");

        let initial = projection.snapshot().expect("snapshot should work");
        assert_eq!(initial.total_events, 1);
        assert_eq!(
            initial.last_strategy_id.as_deref(),
            Some("gc_momentum_fade_v1")
        );

        journal
            .append(sample_record("evt-5", "dispatch_succeeded"))
            .expect("append should project");

        let updated = projection.snapshot().expect("snapshot should work");
        assert_eq!(updated.total_events, 2);
        assert_eq!(updated.dispatch_succeeded_count, 1);
    }
}
