//! Strategy-agnostic Databento market-data contracts, rolling buffers, and warmup tracking.

mod databento_chart_backfill;
mod databento_live;
mod service;

use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tv_bot_core_types::{
    CompiledStrategy, DatabentoInstrument, FeedType, InstrumentMapping, MarketEvent, Timeframe,
    WarmupStatus,
};

pub use databento_chart_backfill::fetch_recent_chart_backfill;
pub use databento_live::{
    DatabentoLiveTransport, DatabentoLiveTransportConfig, DatabentoSlowReaderPolicy,
};
pub use service::{DatabentoWarmupMode, MarketDataService, MarketDataServiceSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketDataConnectionState {
    Disconnected,
    Connecting,
    Subscribed,
    Reconnecting,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketDataHealth {
    Healthy,
    Initializing,
    Degraded,
    Disconnected,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedReadinessState {
    Pending,
    Ready,
    Degraded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionRequest {
    pub provider: &'static str,
    pub dataset: String,
    pub instruments: Vec<DatabentoInstrument>,
    pub feeds: Vec<FeedType>,
    pub timeframes: Vec<Timeframe>,
    pub replay_from: Option<DateTime<Utc>>,
}

impl SubscriptionRequest {
    pub fn from_strategy(
        strategy: &CompiledStrategy,
        mapping: &InstrumentMapping,
    ) -> Result<Self, MarketDataError> {
        if mapping.databento_symbols.is_empty() {
            return Err(MarketDataError::NoDatabentoSymbols);
        }

        let dataset = mapping
            .databento_symbols
            .first()
            .map(|symbol| symbol.dataset.clone())
            .expect("databento symbols checked above");

        Ok(Self {
            provider: "databento",
            dataset,
            instruments: mapping.databento_symbols.clone(),
            feeds: provider_subscription_feeds(strategy)?,
            timeframes: ordered_timeframes(strategy),
            replay_from: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedStatus {
    pub instrument_symbol: String,
    pub feed: FeedType,
    pub state: FeedReadinessState,
    pub last_event_at: Option<DateTime<Utc>>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferStatus {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub available_bars: usize,
    pub required_bars: u32,
    pub capacity: usize,
    pub ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmupProgress {
    pub status: WarmupStatus,
    pub ready_requires_all: bool,
    pub buffers: Vec<BufferStatus>,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub failure_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketDataStatusSnapshot {
    pub provider: String,
    pub dataset: String,
    pub connection_state: MarketDataConnectionState,
    pub health: MarketDataHealth,
    pub feed_statuses: Vec<FeedStatus>,
    pub warmup: WarmupProgress,
    pub reconnect_count: u64,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub last_disconnect_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabentoSessionStatus {
    pub market_data: MarketDataStatusSnapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DatabentoTransportUpdate {
    Event(MarketEvent),
    Disconnected {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
    SubscriptionAck {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
    ReplayCompleted {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
    SlowReaderWarning {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
    EndOfInterval {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
}

#[derive(Clone, Debug)]
pub struct RollingBuffer<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> RollingBuffer<T> {
    pub fn new(capacity: usize) -> Result<Self, MarketDataError> {
        if capacity == 0 {
            return Err(MarketDataError::InvalidBufferCapacity);
        }

        Ok(Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        })
    }

    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn latest(&self) -> Option<&T> {
        self.items.back()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }
}

fn retained_buffer_capacity(timeframe: Timeframe, required: u32) -> usize {
    let required = required as usize;
    let chart_floor = usize::try_from(
        (Duration::hours(2).num_seconds() / timeframe_duration(timeframe).num_seconds()).max(1),
    )
    .unwrap_or(required);

    required.max(chart_floor)
}

#[derive(Clone, Debug)]
struct AggregationWindow {
    symbol: String,
    timeframe: Timeframe,
    window_start: DateTime<Utc>,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: u64,
}

impl AggregationWindow {
    fn new(
        symbol: String,
        timeframe: Timeframe,
        window_start: DateTime<Utc>,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
        volume: u64,
    ) -> Self {
        Self {
            symbol,
            timeframe,
            window_start,
            open,
            high,
            low,
            close,
            volume,
        }
    }

    fn update(&mut self, high: Decimal, low: Decimal, close: Decimal, volume: u64) {
        if high > self.high {
            self.high = high;
        }
        if low < self.low {
            self.low = low;
        }
        self.close = close;
        self.volume = self.volume.saturating_add(volume);
    }

    fn into_event(self) -> MarketEvent {
        let closed_at = self.window_start + timeframe_duration(self.timeframe);
        MarketEvent::Bar {
            symbol: self.symbol,
            timeframe: self.timeframe,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            closed_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MultiTimeframeAggregator {
    source_timeframe: Timeframe,
    target_timeframes: Vec<Timeframe>,
    windows: HashMap<(String, Timeframe), AggregationWindow>,
}

impl MultiTimeframeAggregator {
    pub fn new(
        source_timeframe: Timeframe,
        target_timeframes: Vec<Timeframe>,
    ) -> Result<Self, MarketDataError> {
        if target_timeframes.is_empty() {
            return Err(MarketDataError::AggregatorRequiresTargets);
        }

        for timeframe in &target_timeframes {
            if timeframe_duration(*timeframe) <= timeframe_duration(source_timeframe) {
                return Err(MarketDataError::InvalidAggregationTarget {
                    source_timeframe,
                    target_timeframe: *timeframe,
                });
            }
        }

        let mut deduped = target_timeframes;
        deduped.sort();
        deduped.dedup();

        Ok(Self {
            source_timeframe,
            target_timeframes: deduped,
            windows: HashMap::new(),
        })
    }

    pub fn source_timeframe(&self) -> Timeframe {
        self.source_timeframe
    }

    pub fn target_timeframes(&self) -> &[Timeframe] {
        &self.target_timeframes
    }

    pub fn ingest(&mut self, event: &MarketEvent) -> Vec<MarketEvent> {
        let MarketEvent::Bar {
            symbol,
            timeframe,
            open,
            high,
            low,
            close,
            volume,
            closed_at,
        } = event
        else {
            return Vec::new();
        };

        if *timeframe != self.source_timeframe {
            return Vec::new();
        }

        let mut completed = Vec::new();

        for target_timeframe in &self.target_timeframes {
            let source_started_at = *closed_at - timeframe_duration(self.source_timeframe);
            let window_start = align_window_start(source_started_at, *target_timeframe);
            let key = (symbol.clone(), *target_timeframe);
            let window_end = window_start + timeframe_duration(*target_timeframe);

            match self.windows.get_mut(&key) {
                Some(window) if window.window_start == window_start => {
                    window.update(*high, *low, *close, *volume);
                }
                Some(_) => {
                    let finished = self.windows.remove(&key).expect("window exists");
                    completed.push(finished.into_event());
                    self.windows.insert(
                        key.clone(),
                        AggregationWindow::new(
                            symbol.clone(),
                            *target_timeframe,
                            window_start,
                            *open,
                            *high,
                            *low,
                            *close,
                            *volume,
                        ),
                    );
                }
                None => {
                    self.windows.insert(
                        key.clone(),
                        AggregationWindow::new(
                            symbol.clone(),
                            *target_timeframe,
                            window_start,
                            *open,
                            *high,
                            *low,
                            *close,
                            *volume,
                        ),
                    );
                }
            }

            if *closed_at >= window_end {
                let finished = self
                    .windows
                    .remove(&key)
                    .expect("completed aggregation window should exist");
                completed.push(finished.into_event());
            }
        }

        completed
    }

    pub fn flush(&mut self) -> Vec<MarketEvent> {
        let mut events: Vec<_> = self
            .windows
            .drain()
            .map(|(_, window)| window.into_event())
            .collect();
        events.sort_by_key(|event| match event {
            MarketEvent::Bar {
                symbol,
                timeframe,
                closed_at,
                ..
            } => (symbol.clone(), *timeframe, *closed_at),
            _ => unreachable!("aggregator only emits bars"),
        });
        events
    }
}

#[derive(Clone, Debug)]
pub struct WarmupTracker {
    symbol: String,
    ready_requires_all: bool,
    buffers: HashMap<Timeframe, RollingBuffer<MarketEvent>>,
    required_bars: HashMap<Timeframe, u32>,
    started_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
    failure_reason: Option<String>,
}

impl WarmupTracker {
    pub fn from_strategy(
        strategy: &CompiledStrategy,
        primary_symbol: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, MarketDataError> {
        let mut buffers = HashMap::new();
        let mut required_bars = HashMap::new();

        for (timeframe, required) in &strategy.warmup.bars_required {
            if *required == 0 {
                return Err(MarketDataError::InvalidWarmupRequirement {
                    timeframe: *timeframe,
                });
            }

            buffers.insert(
                *timeframe,
                RollingBuffer::new(retained_buffer_capacity(*timeframe, *required))?,
            );
            required_bars.insert(*timeframe, *required);
        }

        Ok(Self {
            symbol: primary_symbol.into(),
            ready_requires_all: strategy.warmup.ready_requires_all,
            buffers,
            required_bars,
            started_at: None,
            updated_at: now,
            failure_reason: None,
        })
    }

    pub fn start(&mut self, now: DateTime<Utc>) {
        self.started_at = Some(now);
        self.updated_at = now;
        self.failure_reason = None;
    }

    pub fn reset(&mut self, now: DateTime<Utc>) {
        for (timeframe, required) in &self.required_bars {
            self.buffers.insert(
                *timeframe,
                RollingBuffer::new(retained_buffer_capacity(*timeframe, *required))
                    .expect("validated"),
            );
        }

        self.started_at = None;
        self.updated_at = now;
        self.failure_reason = None;
    }

    pub fn mark_failed(&mut self, message: impl Into<String>, now: DateTime<Utc>) {
        self.failure_reason = Some(message.into());
        self.updated_at = now;
    }

    pub fn ingest(&mut self, event: &MarketEvent) {
        match event {
            MarketEvent::Bar {
                symbol, timeframe, ..
            } if symbol == &self.symbol => {
                if let Some(buffer) = self.buffers.get_mut(timeframe) {
                    buffer.push(event.clone());
                    self.updated_at = event_timestamp(event);
                }
            }
            _ => {}
        }
    }

    pub fn ingest_history<'a>(&mut self, events: impl IntoIterator<Item = &'a MarketEvent>) {
        for event in events {
            self.ingest(event);
        }
    }

    pub fn progress(&self, now: DateTime<Utc>) -> WarmupProgress {
        let status = if self.failure_reason.is_some() {
            WarmupStatus::Failed
        } else if self.started_at.is_none() {
            WarmupStatus::Loaded
        } else if self.is_ready() {
            WarmupStatus::Ready
        } else {
            WarmupStatus::Warming
        };

        let mut buffers: Vec<_> = self
            .required_bars
            .iter()
            .map(|(timeframe, required_bars)| {
                let buffer = self
                    .buffers
                    .get(timeframe)
                    .expect("warmup buffer should exist for each requirement");
                BufferStatus {
                    symbol: self.symbol.clone(),
                    timeframe: *timeframe,
                    available_bars: buffer.len(),
                    required_bars: *required_bars,
                    capacity: buffer.capacity(),
                    ready: buffer.len() >= *required_bars as usize,
                }
            })
            .collect();
        buffers.sort_by_key(|buffer| buffer.timeframe);

        WarmupProgress {
            status,
            ready_requires_all: self.ready_requires_all,
            buffers,
            started_at: self.started_at,
            updated_at: self.updated_at.max(now),
            failure_reason: self.failure_reason.clone(),
        }
    }

    pub fn is_ready(&self) -> bool {
        let mut readiness = self.required_bars.iter().map(|(timeframe, required_bars)| {
            self.buffers
                .get(timeframe)
                .map(|buffer| buffer.len() >= *required_bars as usize)
                .unwrap_or(false)
        });

        if self.ready_requires_all {
            readiness.all(|ready| ready)
        } else {
            readiness.any(|ready| ready)
        }
    }

    pub fn buffer(&self, timeframe: Timeframe) -> Option<&RollingBuffer<MarketEvent>> {
        self.buffers.get(&timeframe)
    }
}

pub struct DatabentoMarketDataCoordinator {
    subscription: SubscriptionRequest,
    feed_statuses: HashMap<(String, FeedType), FeedStatus>,
    connection_state: MarketDataConnectionState,
    degradation_reason: Option<String>,
    aggregator: Option<MultiTimeframeAggregator>,
    warmup: WarmupTracker,
    reconnect_count: u64,
    last_heartbeat_at: Option<DateTime<Utc>>,
    last_disconnect_reason: Option<String>,
    updated_at: DateTime<Utc>,
}

impl DatabentoMarketDataCoordinator {
    pub fn from_strategy(
        strategy: &CompiledStrategy,
        mapping: &InstrumentMapping,
        now: DateTime<Utc>,
    ) -> Result<Self, MarketDataError> {
        let subscription = SubscriptionRequest::from_strategy(strategy, mapping)?;
        let primary_symbol = subscription
            .instruments
            .first()
            .map(|instrument| instrument.symbol.clone())
            .expect("subscription always has symbols");
        let warmup = WarmupTracker::from_strategy(strategy, primary_symbol, now)?;
        let aggregator = build_aggregator(strategy)?;

        let mut feed_statuses = HashMap::new();
        for instrument in &subscription.instruments {
            for feed in required_feed_statuses(strategy) {
                feed_statuses.insert(
                    (instrument.symbol.clone(), feed),
                    FeedStatus {
                        instrument_symbol: instrument.symbol.clone(),
                        feed,
                        state: FeedReadinessState::Pending,
                        last_event_at: None,
                        detail: "awaiting feed data".to_owned(),
                    },
                );
            }
        }

        Ok(Self {
            subscription,
            feed_statuses,
            connection_state: MarketDataConnectionState::Disconnected,
            degradation_reason: None,
            aggregator,
            warmup,
            reconnect_count: 0,
            last_heartbeat_at: None,
            last_disconnect_reason: None,
            updated_at: now,
        })
    }

    pub fn subscription(&self) -> &SubscriptionRequest {
        &self.subscription
    }

    pub fn warmup(&self) -> &WarmupTracker {
        &self.warmup
    }

    pub fn warmup_mut(&mut self) -> &mut WarmupTracker {
        &mut self.warmup
    }

    pub fn buffer(&self, timeframe: Timeframe) -> Option<&RollingBuffer<MarketEvent>> {
        self.warmup.buffer(timeframe)
    }

    pub fn reconnect_count(&self) -> u64 {
        self.reconnect_count
    }

    pub fn last_heartbeat_at(&self) -> Option<DateTime<Utc>> {
        self.last_heartbeat_at
    }

    pub fn last_disconnect_reason(&self) -> Option<&str> {
        self.last_disconnect_reason.as_deref()
    }

    pub fn set_connection_state(
        &mut self,
        connection_state: MarketDataConnectionState,
        now: DateTime<Utc>,
    ) {
        self.connection_state = connection_state;
        self.updated_at = now;
    }

    pub fn mark_degraded(&mut self, message: impl Into<String>, now: DateTime<Utc>) {
        self.degradation_reason = Some(message.into());
        self.updated_at = now;
    }

    pub fn clear_degraded(&mut self, now: DateTime<Utc>) {
        self.degradation_reason = None;
        self.updated_at = now;
    }

    pub fn touch(&mut self, now: DateTime<Utc>) {
        self.updated_at = self.updated_at.max(now);
    }

    pub fn mark_feed_degraded(
        &mut self,
        instrument_symbol: &str,
        feed: FeedType,
        message: impl Into<String>,
        now: DateTime<Utc>,
    ) {
        if let Some(status) = self
            .feed_statuses
            .get_mut(&(instrument_symbol.to_owned(), feed))
        {
            status.state = FeedReadinessState::Degraded;
            status.last_event_at = Some(now);
            status.detail = message.into();
            self.updated_at = now;
        }
    }

    pub fn note_reconnect_attempt(&mut self, now: DateTime<Utc>) {
        self.reconnect_count = self.reconnect_count.saturating_add(1);
        self.connection_state = MarketDataConnectionState::Reconnecting;
        self.updated_at = now;
    }

    pub fn note_disconnect(&mut self, reason: impl Into<String>, now: DateTime<Utc>) {
        self.connection_state = MarketDataConnectionState::Disconnected;
        self.last_disconnect_reason = Some(reason.into());
        self.updated_at = now;
    }

    pub fn record_event(&mut self, event: MarketEvent) {
        let mut queued = vec![event];

        while let Some(next_event) = queued.pop() {
            let event_time = event_timestamp(&next_event);

            if matches!(next_event, MarketEvent::Heartbeat { .. }) {
                self.last_heartbeat_at = Some(event_time);
            }

            for feed in feeds_for_event(&next_event) {
                let symbol = event_symbol(&next_event).to_owned();
                if let Some(status) = self.feed_statuses.get_mut(&(symbol, feed)) {
                    status.state = FeedReadinessState::Ready;
                    status.last_event_at = Some(event_time);
                    status.detail = "feed is producing data".to_owned();
                }
            }

            if let Some(aggregator) = self.aggregator.as_mut() {
                let aggregated_events = aggregator.ingest(&next_event);
                queued.extend(aggregated_events);
            }

            self.warmup.ingest(&next_event);
            self.updated_at = event_time;
        }
    }

    pub fn can_open_new_positions(&self, now: DateTime<Utc>) -> bool {
        let snapshot = self.snapshot(now);
        snapshot.health == MarketDataHealth::Healthy
            && snapshot.warmup.status == WarmupStatus::Ready
    }

    pub fn snapshot(&self, now: DateTime<Utc>) -> MarketDataStatusSnapshot {
        let mut feed_statuses: Vec<_> = self.feed_statuses.values().cloned().collect();
        feed_statuses.sort_by_key(|status| (status.instrument_symbol.clone(), status.feed));

        let health = derive_market_data_health(
            self.connection_state,
            &feed_statuses,
            self.degradation_reason.as_deref(),
        );

        MarketDataStatusSnapshot {
            provider: self.subscription.provider.to_owned(),
            dataset: self.subscription.dataset.clone(),
            connection_state: self.connection_state,
            health,
            feed_statuses,
            warmup: self.warmup.progress(now),
            reconnect_count: self.reconnect_count,
            last_heartbeat_at: self.last_heartbeat_at,
            last_disconnect_reason: self.last_disconnect_reason.clone(),
            updated_at: self.updated_at.max(now),
        }
    }
}

#[async_trait]
pub trait DatabentoTransport: Send {
    async fn connect(&mut self, dataset: &str) -> Result<(), MarketDataError>;
    async fn subscribe(&mut self, request: &SubscriptionRequest) -> Result<(), MarketDataError>;
    async fn start(&mut self) -> Result<(), MarketDataError>;
    async fn next_update(&mut self) -> Result<Option<DatabentoTransportUpdate>, MarketDataError>;
    async fn disconnect(&mut self) -> Result<(), MarketDataError>;
}

pub struct DatabentoSessionManager<T>
where
    T: DatabentoTransport,
{
    transport: T,
    coordinator: DatabentoMarketDataCoordinator,
}

impl<T> DatabentoSessionManager<T>
where
    T: DatabentoTransport,
{
    pub fn new(
        transport: T,
        strategy: &CompiledStrategy,
        mapping: &InstrumentMapping,
        now: DateTime<Utc>,
    ) -> Result<Self, MarketDataError> {
        Ok(Self {
            transport,
            coordinator: DatabentoMarketDataCoordinator::from_strategy(strategy, mapping, now)?,
        })
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn coordinator(&self) -> &DatabentoMarketDataCoordinator {
        &self.coordinator
    }

    pub fn coordinator_mut(&mut self) -> &mut DatabentoMarketDataCoordinator {
        &mut self.coordinator
    }

    pub fn configure_replay_from(&mut self, replay_from: Option<DateTime<Utc>>) {
        self.coordinator.subscription.replay_from = replay_from;
    }

    pub async fn connect(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<DatabentoSessionStatus, MarketDataError> {
        let request = self.coordinator.subscription().clone();
        self.coordinator
            .set_connection_state(MarketDataConnectionState::Connecting, now);

        if let Err(error) = self.transport.connect(&request.dataset).await {
            self.handle_transport_failure("connect", &error, now);
            return Err(error);
        }

        if let Err(error) = self.transport.subscribe(&request).await {
            self.handle_transport_failure("subscribe", &error, now);
            return Err(error);
        }

        if let Err(error) = self.transport.start().await {
            self.handle_transport_failure("start", &error, now);
            return Err(error);
        }

        self.coordinator
            .set_connection_state(MarketDataConnectionState::Subscribed, now);
        self.coordinator.clear_degraded(now);

        Ok(self.snapshot(now))
    }

    pub async fn reconnect(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<DatabentoSessionStatus, MarketDataError> {
        let request = self.coordinator.subscription().clone();
        self.coordinator.note_reconnect_attempt(now);

        if let Err(error) = self.transport.connect(&request.dataset).await {
            self.handle_transport_failure("reconnect_connect", &error, now);
            return Err(error);
        }

        if let Err(error) = self.transport.subscribe(&request).await {
            self.handle_transport_failure("reconnect_subscribe", &error, now);
            return Err(error);
        }

        if let Err(error) = self.transport.start().await {
            self.handle_transport_failure("reconnect_start", &error, now);
            return Err(error);
        }

        self.coordinator
            .set_connection_state(MarketDataConnectionState::Subscribed, now);
        self.coordinator.clear_degraded(now);

        Ok(self.snapshot(now))
    }

    pub async fn disconnect(
        &mut self,
        reason: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<DatabentoSessionStatus, MarketDataError> {
        if let Err(error) = self.transport.disconnect().await {
            self.handle_transport_failure("disconnect", &error, now);
            return Err(error);
        }

        self.coordinator.note_disconnect(reason, now);
        Ok(self.snapshot(now))
    }

    pub fn record_event(&mut self, event: MarketEvent) {
        self.coordinator.record_event(event);
    }

    pub async fn poll_next_update(
        &mut self,
    ) -> Result<Option<DatabentoTransportUpdate>, MarketDataError> {
        let Some(update) = self.transport.next_update().await? else {
            return Ok(None);
        };

        self.apply_transport_update(&update);
        Ok(Some(update))
    }

    pub fn snapshot(&self, now: DateTime<Utc>) -> DatabentoSessionStatus {
        DatabentoSessionStatus {
            market_data: self.coordinator.snapshot(now),
        }
    }

    fn handle_transport_failure(
        &mut self,
        operation: &'static str,
        error: &MarketDataError,
        now: DateTime<Utc>,
    ) {
        self.coordinator
            .set_connection_state(MarketDataConnectionState::Failed, now);
        self.coordinator.mark_degraded(
            format!("Databento transport operation `{operation}` failed: {error}"),
            now,
        );
    }

    fn apply_transport_update(&mut self, update: &DatabentoTransportUpdate) {
        match update {
            DatabentoTransportUpdate::Event(event) => self.coordinator.record_event(event.clone()),
            DatabentoTransportUpdate::Disconnected {
                occurred_at,
                detail,
            } => {
                self.coordinator
                    .note_disconnect(detail.clone(), *occurred_at);
            }
            DatabentoTransportUpdate::SubscriptionAck { occurred_at, .. }
            | DatabentoTransportUpdate::ReplayCompleted { occurred_at, .. }
            | DatabentoTransportUpdate::EndOfInterval { occurred_at, .. } => {
                self.coordinator.touch(*occurred_at);
            }
            DatabentoTransportUpdate::SlowReaderWarning {
                occurred_at,
                detail,
            } => {
                self.coordinator.mark_degraded(detail.clone(), *occurred_at);
            }
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MarketDataError {
    #[error("market-data subscription requires at least one Databento symbol")]
    NoDatabentoSymbols,
    #[error("rolling buffer capacity must be greater than zero")]
    InvalidBufferCapacity,
    #[error("warmup requirement for timeframe `{timeframe:?}` must be greater than zero")]
    InvalidWarmupRequirement { timeframe: Timeframe },
    #[error("multi-timeframe aggregation requires at least one target timeframe")]
    AggregatorRequiresTargets,
    #[error("cannot aggregate from `{source_timeframe:?}` into `{target_timeframe:?}` because the target timeframe must be larger")]
    InvalidAggregationTarget {
        source_timeframe: Timeframe,
        target_timeframe: Timeframe,
    },
    #[error("Databento live transport does not support feed `{feed:?}` in this runtime")]
    UnsupportedLiveFeed { feed: FeedType },
    #[error("Databento dataset `{dataset}` is not recognized by the client library")]
    UnsupportedDataset { dataset: String },
    #[error("Databento gateway address `{address}` is invalid: {message}")]
    InvalidGatewayAddress { address: String, message: String },
    #[error("Databento record is missing a symbol mapping for instrument id `{instrument_id}`")]
    MissingInstrumentSymbol { instrument_id: u32 },
    #[error("Databento record did not provide a usable event timestamp")]
    MissingRecordTimestamp,
    #[error("Databento transport operation `{operation}` failed: {message}")]
    TransportOperationFailed {
        operation: &'static str,
        message: String,
    },
}

fn derive_market_data_health(
    connection_state: MarketDataConnectionState,
    feed_statuses: &[FeedStatus],
    degradation_reason: Option<&str>,
) -> MarketDataHealth {
    match connection_state {
        MarketDataConnectionState::Disconnected => MarketDataHealth::Disconnected,
        MarketDataConnectionState::Failed => MarketDataHealth::Failed,
        MarketDataConnectionState::Connecting | MarketDataConnectionState::Reconnecting => {
            MarketDataHealth::Initializing
        }
        MarketDataConnectionState::Subscribed => {
            if degradation_reason.is_some() {
                MarketDataHealth::Degraded
            } else if feed_statuses
                .iter()
                .any(|status| status.state == FeedReadinessState::Degraded)
            {
                MarketDataHealth::Degraded
            } else if feed_statuses
                .iter()
                .any(|status| status.state == FeedReadinessState::Pending)
            {
                MarketDataHealth::Initializing
            } else {
                MarketDataHealth::Healthy
            }
        }
    }
}

fn event_symbol(event: &MarketEvent) -> &str {
    match event {
        MarketEvent::Trade { symbol, .. } => symbol,
        MarketEvent::Bar { symbol, .. } => symbol,
        MarketEvent::Heartbeat { dataset, .. } => dataset,
    }
}

fn event_timestamp(event: &MarketEvent) -> DateTime<Utc> {
    match event {
        MarketEvent::Trade { occurred_at, .. } => *occurred_at,
        MarketEvent::Bar { closed_at, .. } => *closed_at,
        MarketEvent::Heartbeat { occurred_at, .. } => *occurred_at,
    }
}

fn feeds_for_event(event: &MarketEvent) -> Vec<FeedType> {
    match event {
        MarketEvent::Trade { .. } => vec![FeedType::Trades],
        MarketEvent::Bar { timeframe, .. } => match timeframe {
            Timeframe::OneSecond => vec![FeedType::Ohlcv1s],
            Timeframe::OneMinute => vec![FeedType::Ohlcv1m],
            Timeframe::FiveMinute => vec![FeedType::Ohlcv5m],
        },
        MarketEvent::Heartbeat { .. } => Vec::new(),
    }
}

fn provider_subscription_feeds(
    strategy: &CompiledStrategy,
) -> Result<Vec<FeedType>, MarketDataError> {
    let mut feeds: Vec<_> = strategy
        .data_requirements
        .feeds
        .iter()
        .map(|feed| feed.kind)
        .filter(|feed| {
            !matches!(
                feed,
                FeedType::Ohlcv1s | FeedType::Ohlcv1m | FeedType::Ohlcv5m
            )
        })
        .collect();

    feeds.push(provider_source_feed(strategy));
    feeds.sort();
    feeds.dedup();
    Ok(feeds)
}

fn ordered_timeframes(strategy: &CompiledStrategy) -> Vec<Timeframe> {
    let mut timeframes = strategy.data_requirements.timeframes.clone();
    timeframes.sort();
    timeframes.dedup();
    timeframes
}

fn required_feed_statuses(strategy: &CompiledStrategy) -> Vec<FeedType> {
    let mut feeds: Vec<_> = strategy
        .data_requirements
        .feeds
        .iter()
        .map(|feed| feed.kind)
        .filter(|feed| {
            !matches!(
                feed,
                FeedType::Ohlcv1s | FeedType::Ohlcv1m | FeedType::Ohlcv5m
            )
        })
        .collect();

    feeds.push(provider_source_feed(strategy));

    feeds.sort();
    feeds.dedup();
    feeds
}

fn build_aggregator(
    strategy: &CompiledStrategy,
) -> Result<Option<MultiTimeframeAggregator>, MarketDataError> {
    let requested_timeframes = ordered_timeframes(strategy);
    if !strategy.data_requirements.multi_timeframe || requested_timeframes.len() <= 1 {
        return Ok(None);
    }

    let source_timeframe = provider_source_timeframe(strategy);
    let target_timeframes: Vec<_> = requested_timeframes
        .into_iter()
        .filter(|timeframe| *timeframe != source_timeframe)
        .collect();

    if target_timeframes.is_empty() {
        return Ok(None);
    }

    Ok(Some(MultiTimeframeAggregator::new(
        source_timeframe,
        target_timeframes,
    )?))
}

fn feed_for_timeframe(timeframe: Timeframe) -> FeedType {
    match timeframe {
        Timeframe::OneSecond => FeedType::Ohlcv1s,
        Timeframe::OneMinute => FeedType::Ohlcv1m,
        Timeframe::FiveMinute => FeedType::Ohlcv5m,
    }
}

fn provider_source_timeframe(strategy: &CompiledStrategy) -> Timeframe {
    let timeframes = ordered_timeframes(strategy);
    if timeframes.contains(&Timeframe::OneSecond) {
        Timeframe::OneSecond
    } else {
        Timeframe::OneMinute
    }
}

fn provider_source_feed(strategy: &CompiledStrategy) -> FeedType {
    feed_for_timeframe(provider_source_timeframe(strategy))
}

fn timeframe_duration(timeframe: Timeframe) -> Duration {
    match timeframe {
        Timeframe::OneSecond => Duration::seconds(1),
        Timeframe::OneMinute => Duration::minutes(1),
        Timeframe::FiveMinute => Duration::minutes(5),
    }
}

fn align_window_start(timestamp: DateTime<Utc>, timeframe: Timeframe) -> DateTime<Utc> {
    let seconds = timeframe_duration(timeframe).num_seconds();
    let aligned_seconds = timestamp.timestamp() - timestamp.timestamp().rem_euclid(seconds);

    DateTime::<Utc>::from_timestamp(aligned_seconds, 0).expect("aligned timestamp should be valid")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use tv_bot_core_types::{
        BrokerPreference, BrokerPreferences, ContractMode, DailyLossLimit, DashboardDisplay,
        DataFeedRequirement, DataRequirements, EntryOrderType, EntryRules, ExecutionSpec,
        ExitRules, FailsafeRules, InstrumentMapping, MarketConfig, MarketSelection, PositionSizing,
        PositionSizingMode, ReversalMode, RiskLimits, ScalingConfig, SessionMode, SessionRules,
        SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata,
        TradeManagement, WarmupRequirements,
    };

    use super::*;

    fn sample_strategy() -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "sample_strategy".to_owned(),
                name: "Sample Strategy".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "test".to_owned(),
                tags: Vec::new(),
                source: None,
                notes: None,
            },
            market: MarketConfig {
                market: "gold".to_owned(),
                selection: MarketSelection {
                    contract_mode: ContractMode::FrontMonthAuto,
                },
            },
            session: SessionRules {
                mode: SessionMode::Always,
                timezone: "America/New_York".to_owned(),
                trade_window: None,
                no_new_entries_after: None,
                flatten_rule: None,
                allowed_days: Vec::new(),
            },
            data_requirements: DataRequirements {
                feeds: vec![
                    DataFeedRequirement {
                        kind: FeedType::Trades,
                    },
                    DataFeedRequirement {
                        kind: FeedType::Ohlcv1m,
                    },
                ],
                timeframes: vec![Timeframe::OneMinute],
                multi_timeframe: false,
                requires: None,
            },
            warmup: WarmupRequirements {
                bars_required: [(Timeframe::OneMinute, 3)].into_iter().collect(),
                ready_requires_all: true,
            },
            signal_confirmation: SignalConfirmation {
                mode: SignalCombinationMode::All,
                primary_conditions: vec!["condition".to_owned()],
                n_required: None,
                secondary_conditions: Vec::new(),
                score_threshold: None,
                regime_filter: None,
                sequence: Vec::new(),
            },
            entry_rules: EntryRules {
                long_enabled: true,
                short_enabled: false,
                entry_order_type: EntryOrderType::Market,
                entry_conditions: None,
                max_entry_distance_ticks: None,
                entry_timeout_seconds: None,
                allow_reentry_same_bar: None,
                entry_filters: None,
            },
            exit_rules: ExitRules {
                exit_on_opposite_signal: false,
                flatten_on_session_end: true,
                exit_conditions: Vec::new(),
                timeout_exit: None,
                max_hold_seconds: None,
                emergency_exit_rules: None,
            },
            position_sizing: PositionSizing {
                mode: PositionSizingMode::Fixed,
                contracts: Some(1),
                max_risk_usd: None,
                min_contracts: None,
                max_contracts: None,
                fallback_fixed_contracts: None,
                rounding_mode: None,
            },
            execution: ExecutionSpec {
                reversal_mode: ReversalMode::FlattenFirst,
                scaling: ScalingConfig {
                    allow_scale_in: false,
                    allow_scale_out: false,
                    max_legs: 1,
                },
                broker_preferences: BrokerPreferences {
                    stop_loss: BrokerPreference::BrokerRequired,
                    take_profit: BrokerPreference::BrokerRequired,
                    trailing_stop: BrokerPreference::BotAllowed,
                },
            },
            trade_management: TradeManagement {
                initial_stop_ticks: 10,
                take_profit_ticks: 20,
                break_even: None,
                trailing: None,
                partial_take_profit: None,
                post_entry_rules: None,
                time_based_adjustments: None,
            },
            risk: RiskLimits {
                daily_loss: DailyLossLimit {
                    broker_side_required: true,
                    local_backup_enabled: true,
                },
                max_trades_per_day: 2,
                max_consecutive_losses: 2,
                max_open_positions: Some(1),
                max_unrealized_drawdown_usd: None,
                cooldown_after_daily_stop: None,
                max_notional_exposure: None,
            },
            failsafes: FailsafeRules {
                no_new_entries_on_data_degrade: true,
                pause_on_broker_sync_mismatch: true,
                pause_on_reconnect_until_reviewed: Some(true),
                kill_on_repeated_order_rejects: None,
                abnormal_spread_guard: None,
                clock_sanity_required: Some(true),
                storage_health_required: Some(true),
            },
            state_behavior: StateBehavior {
                cooldown_after_loss_s: 60,
                max_reentries_per_side: 1,
                regime_mode: None,
                memory_reset_rules: None,
                post_win_cooldown_s: None,
                failed_setup_decay: None,
                reentry_logic: None,
            },
            dashboard_display: DashboardDisplay {
                show: vec!["pnl".to_owned()],
                default_overlay: "entries_exits".to_owned(),
                debug_panels: Vec::new(),
                custom_labels: None,
                preferred_chart_timeframe: None,
            },
        }
    }

    fn sample_mapping() -> InstrumentMapping {
        InstrumentMapping {
            market_family: "gold".to_owned(),
            market_display_name: "COMEX Gold".to_owned(),
            contract_mode: ContractMode::FrontMonthAuto,
            resolved_contract: tv_bot_core_types::FuturesContract {
                market_family: "gold".to_owned(),
                display_name: "COMEX Gold".to_owned(),
                venue: "COMEX".to_owned(),
                symbol_root: "GC".to_owned(),
                month: tv_bot_core_types::ContractMonth {
                    year: 2026,
                    month: 6,
                },
                canonical_symbol: "GCM2026".to_owned(),
            },
            databento_symbols: vec![DatabentoInstrument {
                dataset: "GLBX.MDP3".to_owned(),
                symbol: "GCM2026".to_owned(),
                symbology: tv_bot_core_types::DatabentoSymbology::RawSymbol,
            }],
            tradovate_symbol: "GCM2026".to_owned(),
            resolution_basis: tv_bot_core_types::FrontMonthSelectionBasis::ChainOrder,
            resolved_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 30, 0).unwrap(),
            summary: "test mapping".to_owned(),
        }
    }

    fn bar(closed_at_second: u32) -> MarketEvent {
        MarketEvent::Bar {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: 1.into(),
            high: 2.into(),
            low: 1.into(),
            close: 2.into(),
            volume: 10,
            closed_at: Utc
                .with_ymd_and_hms(
                    2026,
                    4,
                    10,
                    13,
                    closed_at_second / 60,
                    closed_at_second % 60,
                )
                .unwrap(),
        }
    }

    fn trade(second: u32) -> MarketEvent {
        MarketEvent::Trade {
            symbol: "GCM2026".to_owned(),
            price: 2.into(),
            quantity: 1,
            occurred_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, second).unwrap(),
        }
    }

    fn minute_bar(
        minute: u32,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        volume: u64,
    ) -> MarketEvent {
        MarketEvent::Bar {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: open.into(),
            high: high.into(),
            low: low.into(),
            close: close.into(),
            volume,
            closed_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, minute, 0).unwrap(),
        }
    }

    fn second_bar(
        minute: u32,
        second: u32,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        volume: u64,
    ) -> MarketEvent {
        MarketEvent::Bar {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneSecond,
            open: open.into(),
            high: high.into(),
            low: low.into(),
            close: close.into(),
            volume,
            closed_at: Utc
                .with_ymd_and_hms(2026, 4, 10, 13, minute, second)
                .unwrap(),
        }
    }

    fn multi_timeframe_strategy() -> CompiledStrategy {
        let mut strategy = sample_strategy();
        strategy.data_requirements.feeds = vec![
            DataFeedRequirement {
                kind: FeedType::Trades,
            },
            DataFeedRequirement {
                kind: FeedType::Ohlcv1m,
            },
            DataFeedRequirement {
                kind: FeedType::Ohlcv5m,
            },
        ];
        strategy.data_requirements.timeframes = vec![Timeframe::OneMinute, Timeframe::FiveMinute];
        strategy.data_requirements.multi_timeframe = true;
        strategy.warmup.bars_required = [(Timeframe::OneMinute, 3), (Timeframe::FiveMinute, 1)]
            .into_iter()
            .collect();
        strategy
    }

    #[derive(Default)]
    struct FakeDatabentoTransport {
        operations: Vec<String>,
        fail_operation: Option<&'static str>,
        queued_updates: VecDeque<DatabentoTransportUpdate>,
    }

    impl FakeDatabentoTransport {
        fn failing(operation: &'static str) -> Self {
            Self {
                operations: Vec::new(),
                fail_operation: Some(operation),
                queued_updates: VecDeque::new(),
            }
        }

        fn with_updates(updates: Vec<DatabentoTransportUpdate>) -> Self {
            Self {
                operations: Vec::new(),
                fail_operation: None,
                queued_updates: updates.into(),
            }
        }
    }

    #[async_trait]
    impl DatabentoTransport for FakeDatabentoTransport {
        async fn connect(&mut self, dataset: &str) -> Result<(), MarketDataError> {
            self.operations.push(format!("connect:{dataset}"));

            if self.fail_operation == Some("connect") {
                return Err(MarketDataError::TransportOperationFailed {
                    operation: "connect",
                    message: "simulated failure".to_owned(),
                });
            }

            Ok(())
        }

        async fn subscribe(
            &mut self,
            request: &SubscriptionRequest,
        ) -> Result<(), MarketDataError> {
            self.operations.push(format!(
                "subscribe:{}:{}",
                request.dataset,
                request
                    .feeds
                    .iter()
                    .map(|feed| format!("{feed:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ));

            if self.fail_operation == Some("subscribe") {
                return Err(MarketDataError::TransportOperationFailed {
                    operation: "subscribe",
                    message: "simulated failure".to_owned(),
                });
            }

            Ok(())
        }

        async fn start(&mut self) -> Result<(), MarketDataError> {
            self.operations.push("start".to_owned());

            if self.fail_operation == Some("start") {
                return Err(MarketDataError::TransportOperationFailed {
                    operation: "start",
                    message: "simulated failure".to_owned(),
                });
            }

            Ok(())
        }

        async fn next_update(
            &mut self,
        ) -> Result<Option<DatabentoTransportUpdate>, MarketDataError> {
            if self.fail_operation == Some("next_update") {
                return Err(MarketDataError::TransportOperationFailed {
                    operation: "next_update",
                    message: "simulated failure".to_owned(),
                });
            }

            Ok(self.queued_updates.pop_front())
        }

        async fn disconnect(&mut self) -> Result<(), MarketDataError> {
            self.operations.push("disconnect".to_owned());

            if self.fail_operation == Some("disconnect") {
                return Err(MarketDataError::TransportOperationFailed {
                    operation: "disconnect",
                    message: "simulated failure".to_owned(),
                });
            }

            Ok(())
        }
    }

    #[test]
    fn rolling_buffer_keeps_only_recent_items() {
        let mut buffer = RollingBuffer::new(2).expect("buffer should be created");
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);

        let items: Vec<_> = buffer.iter().copied().collect();
        assert_eq!(items, vec![2, 3]);
    }

    #[test]
    fn warmup_progress_stays_loaded_until_started_then_becomes_ready() {
        let strategy = sample_strategy();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut tracker =
            WarmupTracker::from_strategy(&strategy, "GCM2026", now).expect("tracker should build");

        tracker.ingest(&bar(1));
        assert_eq!(tracker.progress(now).status, WarmupStatus::Loaded);

        tracker.start(now);
        tracker.ingest(&bar(2));
        tracker.ingest(&bar(3));
        let progress = tracker.progress(now);

        assert_eq!(progress.status, WarmupStatus::Ready);
        assert!(progress.buffers[0].ready);
    }

    #[test]
    fn warmup_tracker_retains_chart_sized_buffers_beyond_minimum_requirements() {
        let strategy = sample_strategy();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let tracker =
            WarmupTracker::from_strategy(&strategy, "GCM2026", now).expect("tracker should build");

        let progress = tracker.progress(now);
        assert_eq!(progress.buffers[0].timeframe, Timeframe::OneMinute);
        assert_eq!(progress.buffers[0].required_bars, 3);
        assert_eq!(progress.buffers[0].capacity, 120);
    }

    #[test]
    fn coordinator_reports_initializing_until_required_feeds_are_ready() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut coordinator =
            DatabentoMarketDataCoordinator::from_strategy(&strategy, &mapping, now)
                .expect("coordinator should build");

        coordinator.set_connection_state(MarketDataConnectionState::Subscribed, now);
        coordinator.warmup_mut().start(now);
        coordinator.record_event(bar(1));
        let snapshot = coordinator.snapshot(now);

        assert_eq!(snapshot.health, MarketDataHealth::Initializing);
        assert_eq!(snapshot.warmup.status, WarmupStatus::Warming);
        assert!(!coordinator.can_open_new_positions(now));
    }

    #[test]
    fn coordinator_becomes_healthy_and_trade_ready_when_feeds_and_warmup_complete() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut coordinator =
            DatabentoMarketDataCoordinator::from_strategy(&strategy, &mapping, now)
                .expect("coordinator should build");

        coordinator.set_connection_state(MarketDataConnectionState::Subscribed, now);
        coordinator.warmup_mut().start(now);
        coordinator.record_event(trade(1));
        coordinator.record_event(bar(1));
        coordinator.record_event(bar(2));
        coordinator.record_event(bar(3));

        let snapshot = coordinator.snapshot(now);
        assert_eq!(snapshot.health, MarketDataHealth::Healthy);
        assert_eq!(snapshot.warmup.status, WarmupStatus::Ready);
        assert!(coordinator.can_open_new_positions(now));
    }

    #[test]
    fn historical_warmup_seed_can_ready_buffers_without_marking_feed_healthy() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut coordinator =
            DatabentoMarketDataCoordinator::from_strategy(&strategy, &mapping, now)
                .expect("coordinator should build");

        coordinator.set_connection_state(MarketDataConnectionState::Subscribed, now);
        coordinator.warmup_mut().start(now);
        coordinator
            .warmup_mut()
            .ingest_history([bar(1), bar(2), bar(3)].iter());

        let snapshot = coordinator.snapshot(now);
        assert_eq!(snapshot.warmup.status, WarmupStatus::Ready);
        assert_eq!(snapshot.health, MarketDataHealth::Initializing);
        assert!(!coordinator.can_open_new_positions(now));
    }

    #[test]
    fn degraded_market_data_blocks_new_entries() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut coordinator =
            DatabentoMarketDataCoordinator::from_strategy(&strategy, &mapping, now)
                .expect("coordinator should build");

        coordinator.set_connection_state(MarketDataConnectionState::Subscribed, now);
        coordinator.mark_degraded("heartbeat gap exceeded", now);

        let snapshot = coordinator.snapshot(now);
        assert_eq!(snapshot.health, MarketDataHealth::Degraded);
        assert!(!coordinator.can_open_new_positions(now));
    }

    #[test]
    fn subscription_request_uses_strategy_requirements_and_resolved_mapping() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();

        let request =
            SubscriptionRequest::from_strategy(&strategy, &mapping).expect("request should build");

        assert_eq!(request.provider, "databento");
        assert_eq!(request.dataset, "GLBX.MDP3");
        assert_eq!(request.instruments[0].symbol, "GCM2026");
        assert_eq!(request.feeds, vec![FeedType::Trades, FeedType::Ohlcv1m]);
        assert_eq!(request.timeframes, vec![Timeframe::OneMinute]);
    }

    #[test]
    fn multi_timeframe_subscription_uses_smallest_provider_bar_feed() {
        let strategy = multi_timeframe_strategy();
        let mapping = sample_mapping();

        let request =
            SubscriptionRequest::from_strategy(&strategy, &mapping).expect("request should build");

        assert_eq!(request.feeds, vec![FeedType::Trades, FeedType::Ohlcv1m]);
        assert_eq!(
            request.timeframes,
            vec![Timeframe::OneMinute, Timeframe::FiveMinute]
        );
    }

    #[test]
    fn five_minute_only_strategy_uses_one_minute_provider_feed() {
        let mut strategy = sample_strategy();
        strategy.data_requirements.feeds = vec![DataFeedRequirement {
            kind: FeedType::Ohlcv5m,
        }];
        strategy.data_requirements.timeframes = vec![Timeframe::FiveMinute];
        strategy.data_requirements.multi_timeframe = false;
        strategy.warmup.bars_required = [(Timeframe::FiveMinute, 1)].into_iter().collect();
        let mapping = sample_mapping();

        let request =
            SubscriptionRequest::from_strategy(&strategy, &mapping).expect("request should build");

        assert_eq!(request.feeds, vec![FeedType::Ohlcv1m]);
        assert_eq!(request.timeframes, vec![Timeframe::FiveMinute]);
    }

    #[test]
    fn coordinator_aggregates_completed_five_minute_bars_from_one_minute_source() {
        let strategy = multi_timeframe_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut coordinator =
            DatabentoMarketDataCoordinator::from_strategy(&strategy, &mapping, now)
                .expect("coordinator should build");

        coordinator.set_connection_state(MarketDataConnectionState::Subscribed, now);
        coordinator.warmup_mut().start(now);

        for event in [
            minute_bar(1, 100, 101, 99, 100, 10),
            minute_bar(2, 100, 104, 98, 103, 12),
            minute_bar(3, 103, 105, 102, 104, 11),
            minute_bar(4, 104, 106, 101, 102, 9),
            minute_bar(5, 102, 107, 100, 106, 13),
        ] {
            coordinator.record_event(event);
        }

        let aggregated = coordinator
            .buffer(Timeframe::FiveMinute)
            .expect("aggregated buffer should exist");
        let bar = aggregated
            .latest()
            .expect("aggregated bar should be present");

        assert_eq!(aggregated.len(), 1);
        assert_eq!(
            bar,
            &MarketEvent::Bar {
                symbol: "GCM2026".to_owned(),
                timeframe: Timeframe::FiveMinute,
                open: 100.into(),
                high: 107.into(),
                low: 98.into(),
                close: 106.into(),
                volume: 55,
                closed_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 5, 0).unwrap(),
            }
        );
    }

    #[test]
    fn aggregator_aligns_completed_bar_close_to_target_window_boundary() {
        let mut aggregator =
            MultiTimeframeAggregator::new(Timeframe::OneSecond, vec![Timeframe::OneMinute])
                .expect("aggregator should build");

        for event in [
            second_bar(0, 57, 100, 101, 99, 100, 5),
            second_bar(0, 58, 100, 102, 100, 101, 7),
            second_bar(0, 59, 101, 103, 100, 102, 6),
            second_bar(1, 0, 102, 104, 101, 103, 8),
        ] {
            let completed = aggregator.ingest(&event);
            if !completed.is_empty() {
                assert_eq!(
                    completed,
                    vec![MarketEvent::Bar {
                        symbol: "GCM2026".to_owned(),
                        timeframe: Timeframe::OneMinute,
                        open: 100.into(),
                        high: 104.into(),
                        low: 99.into(),
                        close: 103.into(),
                        volume: 26,
                        closed_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 1, 0).unwrap(),
                    }]
                );
            }
        }
    }

    #[test]
    fn heartbeat_updates_last_heartbeat_timestamp() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut coordinator =
            DatabentoMarketDataCoordinator::from_strategy(&strategy, &mapping, now)
                .expect("coordinator should build");

        coordinator.record_event(MarketEvent::Heartbeat {
            dataset: "GLBX.MDP3".to_owned(),
            occurred_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 5).unwrap(),
        });

        assert_eq!(
            coordinator.last_heartbeat_at(),
            Some(Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 5).unwrap())
        );
    }

    #[tokio::test]
    async fn session_manager_connects_disconnects_and_reconnects_observably() {
        let strategy = multi_timeframe_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut manager = DatabentoSessionManager::new(
            FakeDatabentoTransport::default(),
            &strategy,
            &mapping,
            now,
        )
        .expect("manager should build");

        let connected = manager.connect(now).await.expect("connect should succeed");
        assert_eq!(
            connected.market_data.connection_state,
            MarketDataConnectionState::Subscribed
        );

        let disconnected = manager
            .disconnect("network maintenance", now + Duration::seconds(5))
            .await
            .expect("disconnect should succeed");
        assert_eq!(
            disconnected.market_data.connection_state,
            MarketDataConnectionState::Disconnected
        );
        assert_eq!(
            disconnected.market_data.last_disconnect_reason.as_deref(),
            Some("network maintenance")
        );

        let reconnected = manager
            .reconnect(now + Duration::seconds(10))
            .await
            .expect("reconnect should succeed");
        assert_eq!(reconnected.market_data.reconnect_count, 1);
        assert_eq!(
            reconnected.market_data.connection_state,
            MarketDataConnectionState::Subscribed
        );

        assert_eq!(
            manager.transport().operations,
            vec![
                "connect:GLBX.MDP3".to_owned(),
                "subscribe:GLBX.MDP3:Trades,Ohlcv1m".to_owned(),
                "start".to_owned(),
                "disconnect".to_owned(),
                "connect:GLBX.MDP3".to_owned(),
                "subscribe:GLBX.MDP3:Trades,Ohlcv1m".to_owned(),
                "start".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn session_manager_surfaces_transport_failures_in_snapshot_health() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut manager = DatabentoSessionManager::new(
            FakeDatabentoTransport::failing("subscribe"),
            &strategy,
            &mapping,
            now,
        )
        .expect("manager should build");

        let error = manager
            .connect(now)
            .await
            .expect_err("subscribe failure should bubble up");
        assert_eq!(
            error,
            MarketDataError::TransportOperationFailed {
                operation: "subscribe",
                message: "simulated failure".to_owned(),
            }
        );

        let snapshot = manager.snapshot(now).market_data;
        assert_eq!(snapshot.connection_state, MarketDataConnectionState::Failed);
        assert_eq!(snapshot.health, MarketDataHealth::Failed);
    }

    #[tokio::test]
    async fn service_start_warmup_failure_does_not_leave_warmup_marked_active() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut service = MarketDataService::from_strategy(
            FakeDatabentoTransport::failing("connect"),
            &strategy,
            &mapping,
            now,
        )
        .expect("service should build");

        let error = service
            .start_warmup(
                DatabentoWarmupMode::ReplayFrom(now - Duration::minutes(20)),
                now,
            )
            .await
            .expect_err("connect failure should bubble up");
        assert_eq!(
            error,
            MarketDataError::TransportOperationFailed {
                operation: "connect",
                message: "simulated failure".to_owned(),
            }
        );

        let snapshot = service.snapshot(now);
        assert!(!snapshot.warmup_requested);
        assert_eq!(snapshot.warmup_mode, DatabentoWarmupMode::LiveOnly);
        assert!(!snapshot.replay_caught_up);
        assert_ne!(
            snapshot.session.market_data.warmup.status,
            WarmupStatus::Warming
        );
        assert_eq!(
            snapshot.session.market_data.connection_state,
            MarketDataConnectionState::Failed
        );
        assert_eq!(
            service.session().transport().operations,
            vec!["connect:GLBX.MDP3".to_owned()]
        );
    }

    #[tokio::test]
    async fn session_manager_applies_transport_updates_to_runtime_state() {
        let strategy = sample_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let mut manager = DatabentoSessionManager::new(
            FakeDatabentoTransport::with_updates(vec![
                DatabentoTransportUpdate::Event(MarketEvent::Heartbeat {
                    dataset: "GLBX.MDP3".to_owned(),
                    occurred_at: now + Duration::seconds(1),
                }),
                DatabentoTransportUpdate::SlowReaderWarning {
                    occurred_at: now + Duration::seconds(2),
                    detail: "gateway slow-reader warning".to_owned(),
                },
            ]),
            &strategy,
            &mapping,
            now,
        )
        .expect("manager should build");

        manager.connect(now).await.expect("connect should succeed");

        let first = manager
            .poll_next_update()
            .await
            .expect("poll should succeed")
            .expect("heartbeat update should be present");
        assert!(matches!(
            first,
            DatabentoTransportUpdate::Event(MarketEvent::Heartbeat { .. })
        ));

        let second = manager
            .poll_next_update()
            .await
            .expect("poll should succeed")
            .expect("warning update should be present");
        assert!(matches!(
            second,
            DatabentoTransportUpdate::SlowReaderWarning { .. }
        ));

        let snapshot = manager.snapshot(now + Duration::seconds(2)).market_data;
        assert_eq!(snapshot.last_heartbeat_at, Some(now + Duration::seconds(1)));
        assert_eq!(snapshot.health, MarketDataHealth::Degraded);
    }

    #[tokio::test]
    async fn session_manager_marks_disconnect_when_transport_reports_closed_stream() {
        let strategy = multi_timeframe_strategy();
        let mapping = sample_mapping();
        let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
        let disconnect_at = now + Duration::seconds(3);
        let mut manager = DatabentoSessionManager::new(
            FakeDatabentoTransport::with_updates(vec![DatabentoTransportUpdate::Disconnected {
                occurred_at: disconnect_at,
                detail: "Databento live gateway closed connection".to_owned(),
            }]),
            &strategy,
            &mapping,
            now,
        )
        .expect("manager should build");

        manager.connect(now).await.expect("connect should succeed");

        let update = manager
            .poll_next_update()
            .await
            .expect("poll should succeed")
            .expect("disconnect update should be present");
        assert!(matches!(
            update,
            DatabentoTransportUpdate::Disconnected { .. }
        ));

        let snapshot = manager.snapshot(disconnect_at).market_data;
        assert_eq!(
            snapshot.connection_state,
            MarketDataConnectionState::Disconnected
        );
        assert_eq!(snapshot.health, MarketDataHealth::Disconnected);
        assert_eq!(
            snapshot.last_disconnect_reason.as_deref(),
            Some("Databento live gateway closed connection")
        );
    }
}
