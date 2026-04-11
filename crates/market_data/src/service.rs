use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tv_bot_core_types::{CompiledStrategy, InstrumentMapping, MarketEvent, WarmupStatus};

use crate::{
    DatabentoSessionManager, DatabentoSessionStatus, DatabentoTransport, DatabentoTransportUpdate,
    MarketDataConnectionState, MarketDataError, MarketDataHealth,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatabentoWarmupMode {
    LiveOnly,
    ReplayFrom(DateTime<Utc>),
}

impl DatabentoWarmupMode {
    pub fn replay_from(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::LiveOnly => None,
            Self::ReplayFrom(timestamp) => Some(*timestamp),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketDataServiceSnapshot {
    pub session: DatabentoSessionStatus,
    pub warmup_requested: bool,
    pub warmup_mode: DatabentoWarmupMode,
    pub replay_caught_up: bool,
    pub trade_ready: bool,
    pub updated_at: DateTime<Utc>,
}

pub struct MarketDataService<T>
where
    T: DatabentoTransport,
{
    session: DatabentoSessionManager<T>,
    warmup_requested: bool,
    warmup_mode: DatabentoWarmupMode,
    replay_caught_up: bool,
    updated_at: DateTime<Utc>,
}

impl<T> MarketDataService<T>
where
    T: DatabentoTransport,
{
    pub fn from_strategy(
        transport: T,
        strategy: &CompiledStrategy,
        mapping: &InstrumentMapping,
        now: DateTime<Utc>,
    ) -> Result<Self, MarketDataError> {
        Ok(Self {
            session: DatabentoSessionManager::new(transport, strategy, mapping, now)?,
            warmup_requested: false,
            warmup_mode: DatabentoWarmupMode::LiveOnly,
            replay_caught_up: false,
            updated_at: now,
        })
    }

    pub fn session(&self) -> &DatabentoSessionManager<T> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut DatabentoSessionManager<T> {
        &mut self.session
    }

    pub fn snapshot(&self, now: DateTime<Utc>) -> MarketDataServiceSnapshot {
        let session = self.session.snapshot(now);
        let trade_ready = self.warmup_requested
            && self.replay_caught_up
            && session.market_data.health == MarketDataHealth::Healthy
            && session.market_data.warmup.status == WarmupStatus::Ready;

        MarketDataServiceSnapshot {
            session,
            warmup_requested: self.warmup_requested,
            warmup_mode: self.warmup_mode.clone(),
            replay_caught_up: self.replay_caught_up,
            trade_ready,
            updated_at: self.updated_at.max(now),
        }
    }

    pub async fn start_warmup(
        &mut self,
        warmup_mode: DatabentoWarmupMode,
        now: DateTime<Utc>,
    ) -> Result<MarketDataServiceSnapshot, MarketDataError> {
        let current_state = self.session.snapshot(now).market_data.connection_state;
        if current_state != MarketDataConnectionState::Disconnected {
            self.session.disconnect("warmup restart", now).await?;
        }

        self.warmup_requested = true;
        self.warmup_mode = warmup_mode.clone();
        self.replay_caught_up = matches!(warmup_mode, DatabentoWarmupMode::LiveOnly);
        self.session
            .configure_replay_from(warmup_mode.replay_from());
        self.session.coordinator_mut().warmup_mut().reset(now);
        self.session.coordinator_mut().warmup_mut().start(now);
        self.session.connect(now).await?;
        self.updated_at = now;

        Ok(self.snapshot(now))
    }

    pub async fn reconnect(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<MarketDataServiceSnapshot, MarketDataError> {
        self.session.reconnect(now).await?;
        self.updated_at = now;
        Ok(self.snapshot(now))
    }

    pub async fn poll_next_update(
        &mut self,
    ) -> Result<Option<DatabentoTransportUpdate>, MarketDataError> {
        let update = self.session.poll_next_update().await?;
        if let Some(update) = &update {
            self.apply_update(update);
        }
        Ok(update)
    }

    pub fn ingest_manual_event(&mut self, event: MarketEvent) {
        self.session.record_event(event);
    }

    fn apply_update(&mut self, update: &DatabentoTransportUpdate) {
        self.updated_at = update_timestamp(update);

        if matches!(update, DatabentoTransportUpdate::ReplayCompleted { .. }) {
            self.replay_caught_up = true;
        }
    }
}

fn update_timestamp(update: &DatabentoTransportUpdate) -> DateTime<Utc> {
    match update {
        DatabentoTransportUpdate::Event(MarketEvent::Trade { occurred_at, .. })
        | DatabentoTransportUpdate::Event(MarketEvent::Heartbeat { occurred_at, .. }) => {
            *occurred_at
        }
        DatabentoTransportUpdate::Event(MarketEvent::Bar { closed_at, .. }) => *closed_at,
        DatabentoTransportUpdate::SubscriptionAck { occurred_at, .. }
        | DatabentoTransportUpdate::ReplayCompleted { occurred_at, .. }
        | DatabentoTransportUpdate::SlowReaderWarning { occurred_at, .. }
        | DatabentoTransportUpdate::EndOfInterval { occurred_at, .. } => *occurred_at,
    }
}
