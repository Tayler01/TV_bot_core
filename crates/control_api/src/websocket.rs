use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::sync::broadcast;
use tv_bot_core_types::{
    ActionSource, ArmReadinessReport, BrokerStatusSnapshot, EventJournalRecord,
    SystemHealthSnapshot,
};

use crate::ControlApiCommandResult;

#[derive(Clone, Debug, PartialEq)]
pub enum ControlApiEvent {
    CommandResult {
        source: ActionSource,
        result: ControlApiCommandResult,
        occurred_at: DateTime<Utc>,
    },
    ReadinessReport {
        report: ArmReadinessReport,
        occurred_at: DateTime<Utc>,
    },
    BrokerStatus {
        snapshot: BrokerStatusSnapshot,
        occurred_at: DateTime<Utc>,
    },
    SystemHealth {
        snapshot: SystemHealthSnapshot,
        occurred_at: DateTime<Utc>,
    },
    JournalRecord {
        record: EventJournalRecord,
    },
}

pub trait ControlApiEventPublisher {
    fn publish(&self, event: ControlApiEvent) -> Result<(), WebSocketEventHubError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopEventPublisher;

impl ControlApiEventPublisher for NoopEventPublisher {
    fn publish(&self, _event: ControlApiEvent) -> Result<(), WebSocketEventHubError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct WebSocketEventHub {
    sender: broadcast::Sender<ControlApiEvent>,
}

impl WebSocketEventHub {
    pub fn new(capacity: usize) -> Result<Self, WebSocketEventHubError> {
        if capacity == 0 {
            return Err(WebSocketEventHubError::InvalidCapacity);
        }

        let (sender, _) = broadcast::channel(capacity);
        Ok(Self { sender })
    }

    pub fn subscribe(&self) -> WebSocketEventStream {
        WebSocketEventStream {
            receiver: self.sender.subscribe(),
        }
    }
}

impl ControlApiEventPublisher for WebSocketEventHub {
    fn publish(&self, event: ControlApiEvent) -> Result<(), WebSocketEventHubError> {
        self.sender
            .send(event)
            .map(|_| ())
            .map_err(|_| WebSocketEventHubError::NoSubscribers)
    }
}

pub struct WebSocketEventStream {
    receiver: broadcast::Receiver<ControlApiEvent>,
}

impl WebSocketEventStream {
    pub async fn recv(&mut self) -> Result<ControlApiEvent, WebSocketEventStreamError> {
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(skipped) => {
                WebSocketEventStreamError::Lagged { skipped }
            }
            broadcast::error::RecvError::Closed => WebSocketEventStreamError::Closed,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebSocketEventHubError {
    #[error("websocket event hub capacity must be greater than zero")]
    InvalidCapacity,
    #[error("websocket event has no active subscribers")]
    NoSubscribers,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebSocketEventStreamError {
    #[error("websocket event stream closed")]
    Closed,
    #[error("websocket event stream lagged and skipped {skipped} messages")]
    Lagged { skipped: u64 },
}
