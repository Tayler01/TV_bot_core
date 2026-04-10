//! Journal interfaces and testable storage adapters.

use std::sync::{Arc, Mutex};

use thiserror::Error;
use tv_bot_core_types::EventJournalRecord;

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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JournalError {
    #[error("journal storage lock is poisoned")]
    Poisoned,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use tv_bot_core_types::{ActionSource, EventSeverity};

    use super::*;

    #[test]
    fn in_memory_journal_appends_and_lists_records() {
        let journal = InMemoryJournal::new();
        let record = EventJournalRecord {
            event_id: "evt-1".to_owned(),
            category: "strategy".to_owned(),
            action: "loaded".to_owned(),
            source: ActionSource::System,
            severity: EventSeverity::Info,
            occurred_at: Utc::now(),
            payload: json!({
                "strategy_id": "gc_momentum_fade_v1",
            }),
        };

        journal.append(record.clone()).expect("append should work");

        let records = journal.list().expect("list should work");
        assert_eq!(records, vec![record]);
    }

    #[test]
    fn clear_removes_records() {
        let journal = InMemoryJournal::new();
        journal
            .append(EventJournalRecord {
                event_id: "evt-2".to_owned(),
                category: "runtime".to_owned(),
                action: "armed".to_owned(),
                source: ActionSource::Cli,
                severity: EventSeverity::Info,
                occurred_at: Utc::now(),
                payload: json!({ "mode": "paper" }),
            })
            .expect("append should work");

        journal.clear().expect("clear should work");
        assert!(journal.list().expect("list should work").is_empty());
    }
}
