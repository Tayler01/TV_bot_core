use std::collections::VecDeque;

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use tv_bot_core_types::{
    BrokerPreference, BrokerPreferences, CompiledStrategy, ContractMode, DailyLossLimit,
    DashboardDisplay, DataFeedRequirement, DataRequirements, DatabentoInstrument, EntryOrderType,
    EntryRules, ExecutionSpec, ExitRules, FailsafeRules, InstrumentMapping, MarketConfig,
    MarketSelection, PositionSizing, PositionSizingMode, ReversalMode, RiskLimits, ScalingConfig,
    SessionMode, SessionRules, SignalCombinationMode, SignalConfirmation, StateBehavior,
    StrategyMetadata, TradeManagement, WarmupRequirements, WarmupStatus,
};
use tv_bot_market_data::{
    DatabentoTransport, DatabentoTransportUpdate, DatabentoWarmupMode, MarketDataConnectionState,
    MarketDataError, MarketDataHealth, MarketDataService, SubscriptionRequest,
};

#[derive(Default)]
struct FakeDatabentoTransport {
    subscribed_requests: Vec<SubscriptionRequest>,
    updates: VecDeque<DatabentoTransportUpdate>,
    started: bool,
}

impl FakeDatabentoTransport {
    fn with_updates(updates: Vec<DatabentoTransportUpdate>) -> Self {
        Self {
            subscribed_requests: Vec::new(),
            updates: updates.into(),
            started: false,
        }
    }
}

#[async_trait]
impl DatabentoTransport for FakeDatabentoTransport {
    async fn connect(&mut self, _dataset: &str) -> Result<(), MarketDataError> {
        Ok(())
    }

    async fn subscribe(&mut self, request: &SubscriptionRequest) -> Result<(), MarketDataError> {
        self.subscribed_requests.push(request.clone());
        Ok(())
    }

    async fn start(&mut self) -> Result<(), MarketDataError> {
        self.started = true;
        Ok(())
    }

    async fn next_update(&mut self) -> Result<Option<DatabentoTransportUpdate>, MarketDataError> {
        Ok(self.updates.pop_front())
    }

    async fn disconnect(&mut self) -> Result<(), MarketDataError> {
        self.started = false;
        Ok(())
    }
}

fn sample_strategy() -> CompiledStrategy {
    CompiledStrategy {
        metadata: StrategyMetadata {
            schema_version: 1,
            strategy_id: "integration_test".to_owned(),
            name: "Integration Test".to_owned(),
            version: "1.0.0".to_owned(),
            author: "tests".to_owned(),
            description: "integration".to_owned(),
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
                    kind: tv_bot_core_types::FeedType::Trades,
                },
                DataFeedRequirement {
                    kind: tv_bot_core_types::FeedType::Ohlcv1m,
                },
            ],
            timeframes: vec![tv_bot_core_types::Timeframe::OneMinute],
            multi_timeframe: false,
            requires: None,
        },
        warmup: WarmupRequirements {
            bars_required: [(tv_bot_core_types::Timeframe::OneMinute, 3)]
                .into_iter()
                .collect(),
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
        summary: "integration mapping".to_owned(),
    }
}

fn bar(minute: u32) -> DatabentoTransportUpdate {
    DatabentoTransportUpdate::Event(tv_bot_core_types::MarketEvent::Bar {
        symbol: "GCM2026".to_owned(),
        timeframe: tv_bot_core_types::Timeframe::OneMinute,
        open: 100.into(),
        high: 102.into(),
        low: 99.into(),
        close: 101.into(),
        volume: 10,
        closed_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, minute, 0).unwrap(),
    })
}

fn trade(second: u32) -> DatabentoTransportUpdate {
    DatabentoTransportUpdate::Event(tv_bot_core_types::MarketEvent::Trade {
        symbol: "GCM2026".to_owned(),
        price: 101.into(),
        quantity: 1,
        occurred_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, second).unwrap(),
    })
}

#[test]
fn load_does_not_auto_start_warmup() {
    let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
    let service = MarketDataService::from_strategy(
        FakeDatabentoTransport::default(),
        &sample_strategy(),
        &sample_mapping(),
        now,
    )
    .expect("service should build");

    let snapshot = service.snapshot(now);
    assert!(!snapshot.warmup_requested);
    assert!(!snapshot.trade_ready);
    assert_eq!(
        snapshot.session.market_data.connection_state,
        MarketDataConnectionState::Disconnected
    );
    assert_eq!(
        snapshot.session.market_data.warmup.status,
        WarmupStatus::Loaded
    );
}

#[tokio::test]
async fn manual_live_warmup_reaches_ready() {
    let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
    let mut service = MarketDataService::from_strategy(
        FakeDatabentoTransport::with_updates(vec![trade(1), bar(1), bar(2), bar(3)]),
        &sample_strategy(),
        &sample_mapping(),
        now,
    )
    .expect("service should build");

    let started = service
        .start_warmup(DatabentoWarmupMode::LiveOnly, now)
        .await
        .expect("warmup should start");
    assert!(started.warmup_requested);
    assert!(!started.trade_ready);

    while service
        .poll_next_update()
        .await
        .expect("poll should succeed")
        .is_some()
    {}

    let snapshot = service.snapshot(now + Duration::minutes(3));
    assert_eq!(
        snapshot.session.market_data.health,
        MarketDataHealth::Healthy
    );
    assert_eq!(
        snapshot.session.market_data.warmup.status,
        WarmupStatus::Ready
    );
    assert!(snapshot.replay_caught_up);
    assert!(snapshot.trade_ready);
}

#[tokio::test]
async fn replay_warmup_waits_for_replay_completion_before_trade_ready() {
    let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
    let replay_from = now - Duration::minutes(15);
    let mut service = MarketDataService::from_strategy(
        FakeDatabentoTransport::with_updates(vec![
            trade(1),
            bar(1),
            bar(2),
            bar(3),
            DatabentoTransportUpdate::ReplayCompleted {
                occurred_at: now + Duration::minutes(3),
                detail: "replay caught up".to_owned(),
            },
        ]),
        &sample_strategy(),
        &sample_mapping(),
        now,
    )
    .expect("service should build");

    service
        .start_warmup(DatabentoWarmupMode::ReplayFrom(replay_from), now)
        .await
        .expect("replay warmup should start");

    for _ in 0..4 {
        service
            .poll_next_update()
            .await
            .expect("poll should succeed")
            .expect("update should be present");
    }

    let pre_catchup = service.snapshot(now + Duration::minutes(2));
    assert_eq!(
        pre_catchup.session.market_data.warmup.status,
        WarmupStatus::Ready
    );
    assert!(!pre_catchup.replay_caught_up);
    assert!(!pre_catchup.trade_ready);

    service
        .poll_next_update()
        .await
        .expect("poll should succeed")
        .expect("replay completed should be present");

    let snapshot = service.snapshot(now + Duration::minutes(3));
    assert!(snapshot.replay_caught_up);
    assert!(snapshot.trade_ready);

    let request = service
        .session()
        .transport()
        .subscribed_requests
        .first()
        .expect("subscription request should be recorded");
    assert_eq!(request.replay_from, Some(replay_from));
}

#[tokio::test]
async fn replay_reconnect_requires_fresh_catchup_before_trade_ready_resumes() {
    let now = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
    let replay_from = now - Duration::minutes(15);
    let mut service = MarketDataService::from_strategy(
        FakeDatabentoTransport::with_updates(vec![
            trade(1),
            bar(1),
            bar(2),
            bar(3),
            DatabentoTransportUpdate::ReplayCompleted {
                occurred_at: now + Duration::minutes(3),
                detail: "initial replay caught up".to_owned(),
            },
            trade(5),
            DatabentoTransportUpdate::ReplayCompleted {
                occurred_at: now + Duration::minutes(4),
                detail: "reconnect replay caught up".to_owned(),
            },
        ]),
        &sample_strategy(),
        &sample_mapping(),
        now,
    )
    .expect("service should build");

    service
        .start_warmup(DatabentoWarmupMode::ReplayFrom(replay_from), now)
        .await
        .expect("replay warmup should start");

    for _ in 0..5 {
        service
            .poll_next_update()
            .await
            .expect("poll should succeed")
            .expect("initial update should be present");
    }

    let pre_disconnect_ready = service.snapshot(now + Duration::minutes(3));
    assert!(pre_disconnect_ready.replay_caught_up);
    assert!(pre_disconnect_ready.trade_ready);

    service
        .session_mut()
        .disconnect(
            "network interruption",
            now + Duration::minutes(3) + Duration::seconds(5),
        )
        .await
        .expect("disconnect should succeed");

    let reconnected = service
        .reconnect(now + Duration::minutes(3) + Duration::seconds(10))
        .await
        .expect("reconnect should succeed");

    assert_eq!(reconnected.session.market_data.reconnect_count, 1);
    assert_eq!(
        reconnected.session.market_data.connection_state,
        MarketDataConnectionState::Subscribed
    );
    assert_eq!(
        reconnected.session.market_data.warmup.status,
        WarmupStatus::Ready
    );
    assert!(!reconnected.replay_caught_up);
    assert!(!reconnected.trade_ready);

    let requests = &service.session().transport().subscribed_requests;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].replay_from, Some(replay_from));
    assert_eq!(requests[1].replay_from, Some(replay_from));

    service
        .poll_next_update()
        .await
        .expect("poll should succeed")
        .expect("reconnect update should be present");
    let still_catching_up = service.snapshot(now + Duration::minutes(3) + Duration::seconds(30));
    assert!(!still_catching_up.replay_caught_up);
    assert!(!still_catching_up.trade_ready);

    service
        .poll_next_update()
        .await
        .expect("poll should succeed")
        .expect("replay completed should be present");
    let resumed = service.snapshot(now + Duration::minutes(4));
    assert!(resumed.replay_caught_up);
    assert!(resumed.trade_ready);
}
