use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use secrecy::SecretString;
use tv_bot_broker_tradovate::{
    Clock, TradovateAccessToken, TradovateAccount, TradovateAccountApi,
    TradovateAccountListRequest, TradovateAuthApi, TradovateAuthRequest, TradovateBracketOrder,
    TradovateCancelOrderRequest, TradovateCancelOrderResult, TradovateCredentials, TradovateError,
    TradovateExecutionApi, TradovateLiquidatePositionRequest, TradovateLiquidatePositionResult,
    TradovateOrderPlacement, TradovateOrderType, TradovateOsoOrderPlacement,
    TradovatePlaceOrderRequest, TradovatePlaceOrderResult, TradovatePlaceOsoRequest,
    TradovatePlaceOsoResult, TradovateReconnectDecision, TradovateRenewAccessTokenRequest,
    TradovateRoutingPreferences, TradovateSessionConfig, TradovateSessionManager, TradovateSyncApi,
    TradovateSyncConnectRequest, TradovateSyncEvent, TradovateSyncSnapshot, TradovateTimeInForce,
    TradovateUserSyncRequest,
};
use tv_bot_core_types::{
    BrokerAccountRouting, BrokerAccountSnapshot, BrokerConnectionState, BrokerEnvironment,
    BrokerFillUpdate, BrokerHealth, BrokerOrderStatus, BrokerOrderUpdate, BrokerPositionSnapshot,
    BrokerSyncState, RuntimeMode, TradeSide,
};

#[derive(Clone)]
struct MutableClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl MutableClock {
    fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
        }
    }

    fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().expect("clock mutex should not poison") = now;
    }
}

impl Clock for MutableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("clock mutex should not poison")
    }
}

#[derive(Clone, Default)]
struct FakeAuthApi {
    state: Arc<Mutex<FakeAuthState>>,
}

#[derive(Default)]
struct FakeAuthState {
    request_tokens: VecDeque<Result<TradovateAccessToken, TradovateError>>,
    renewal_tokens: VecDeque<Result<TradovateAccessToken, TradovateError>>,
    request_count: usize,
    renewal_count: usize,
}

impl FakeAuthApi {
    fn with_request_token(token: TradovateAccessToken) -> Self {
        let api = Self::default();
        api.state
            .lock()
            .expect("auth mutex should not poison")
            .request_tokens
            .push_back(Ok(token));
        api
    }

    fn push_renewed_token(&self, token: TradovateAccessToken) {
        self.state
            .lock()
            .expect("auth mutex should not poison")
            .renewal_tokens
            .push_back(Ok(token));
    }

    fn request_count(&self) -> usize {
        self.state
            .lock()
            .expect("auth mutex should not poison")
            .request_count
    }

    fn renewal_count(&self) -> usize {
        self.state
            .lock()
            .expect("auth mutex should not poison")
            .renewal_count
    }
}

#[async_trait]
impl TradovateAuthApi for FakeAuthApi {
    async fn request_access_token(
        &self,
        _request: TradovateAuthRequest,
    ) -> Result<TradovateAccessToken, TradovateError> {
        let mut guard = self.state.lock().expect("auth mutex should not poison");
        guard.request_count += 1;
        guard.request_tokens.pop_front().unwrap_or_else(|| {
            Err(TradovateError::AuthTransport {
                message: "missing fake access token".to_owned(),
            })
        })
    }

    async fn renew_access_token(
        &self,
        _request: TradovateRenewAccessTokenRequest,
    ) -> Result<TradovateAccessToken, TradovateError> {
        let mut guard = self.state.lock().expect("auth mutex should not poison");
        guard.renewal_count += 1;
        guard.renewal_tokens.pop_front().unwrap_or_else(|| {
            Err(TradovateError::AuthTransport {
                message: "missing fake renewed token".to_owned(),
            })
        })
    }
}

#[derive(Clone, Default)]
struct FakeAccountApi {
    state: Arc<Mutex<FakeAccountState>>,
}

#[derive(Default)]
struct FakeAccountState {
    responses: VecDeque<Result<Vec<TradovateAccount>, TradovateError>>,
    request_count: usize,
}

impl FakeAccountApi {
    fn with_accounts(accounts: Vec<TradovateAccount>) -> Self {
        let api = Self::default();
        api.state
            .lock()
            .expect("account mutex should not poison")
            .responses
            .push_back(Ok(accounts));
        api
    }

    fn request_count(&self) -> usize {
        self.state
            .lock()
            .expect("account mutex should not poison")
            .request_count
    }
}

#[async_trait]
impl TradovateAccountApi for FakeAccountApi {
    async fn list_accounts(
        &self,
        _request: TradovateAccountListRequest,
    ) -> Result<Vec<TradovateAccount>, TradovateError> {
        let mut guard = self.state.lock().expect("account mutex should not poison");
        guard.request_count += 1;
        guard.responses.pop_front().unwrap_or_else(|| {
            Err(TradovateError::AccountTransport {
                message: "missing fake account response".to_owned(),
            })
        })
    }
}

#[derive(Clone, Default)]
struct FakeSyncApi {
    state: Arc<Mutex<FakeSyncState>>,
}

#[derive(Default)]
struct FakeSyncState {
    initial_syncs: VecDeque<Result<TradovateSyncSnapshot, TradovateError>>,
    events: VecDeque<Result<Option<TradovateSyncEvent>, TradovateError>>,
    connect_count: usize,
    sync_request_count: usize,
    disconnect_count: usize,
}

#[derive(Clone, Default)]
struct FakeExecutionApi {
    state: Arc<Mutex<FakeExecutionState>>,
}

#[derive(Default)]
struct FakeExecutionState {
    place_orders: Vec<TradovatePlaceOrderRequest>,
    place_osos: Vec<TradovatePlaceOsoRequest>,
    liquidations: Vec<TradovateLiquidatePositionRequest>,
    cancel_orders: Vec<TradovateCancelOrderRequest>,
}

impl FakeSyncApi {
    fn with_initial_sync(snapshot: TradovateSyncSnapshot) -> Self {
        let api = Self::default();
        api.state
            .lock()
            .expect("sync mutex should not poison")
            .initial_syncs
            .push_back(Ok(snapshot));
        api
    }

    fn push_initial_sync(&self, snapshot: TradovateSyncSnapshot) {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .initial_syncs
            .push_back(Ok(snapshot));
    }

    fn push_event(&self, event: TradovateSyncEvent) {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .events
            .push_back(Ok(Some(event)));
    }

    fn connect_count(&self) -> usize {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .connect_count
    }

    fn sync_request_count(&self) -> usize {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .sync_request_count
    }
}

#[async_trait]
impl TradovateExecutionApi for FakeExecutionApi {
    async fn place_order(
        &self,
        request: TradovatePlaceOrderRequest,
    ) -> Result<TradovatePlaceOrderResult, TradovateError> {
        self.state
            .lock()
            .expect("execution mutex should not poison")
            .place_orders
            .push(request);
        Ok(TradovatePlaceOrderResult { order_id: 9101 })
    }

    async fn place_oso(
        &self,
        request: TradovatePlaceOsoRequest,
    ) -> Result<TradovatePlaceOsoResult, TradovateError> {
        self.state
            .lock()
            .expect("execution mutex should not poison")
            .place_osos
            .push(request);
        Ok(TradovatePlaceOsoResult {
            order_id: 9201,
            oso1_id: Some(9202),
            oso2_id: None,
        })
    }

    async fn liquidate_position(
        &self,
        request: TradovateLiquidatePositionRequest,
    ) -> Result<TradovateLiquidatePositionResult, TradovateError> {
        self.state
            .lock()
            .expect("execution mutex should not poison")
            .liquidations
            .push(request);
        Ok(TradovateLiquidatePositionResult { order_id: 9301 })
    }

    async fn cancel_order(
        &self,
        request: TradovateCancelOrderRequest,
    ) -> Result<TradovateCancelOrderResult, TradovateError> {
        self.state
            .lock()
            .expect("execution mutex should not poison")
            .cancel_orders
            .push(request.clone());
        Ok(TradovateCancelOrderResult {
            order_id: request.order_id,
        })
    }
}

#[async_trait]
impl TradovateSyncApi for FakeSyncApi {
    async fn connect(&self, _request: TradovateSyncConnectRequest) -> Result<(), TradovateError> {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .connect_count += 1;
        Ok(())
    }

    async fn request_user_sync(
        &self,
        _request: TradovateUserSyncRequest,
    ) -> Result<TradovateSyncSnapshot, TradovateError> {
        let mut guard = self.state.lock().expect("sync mutex should not poison");
        guard.sync_request_count += 1;
        guard.initial_syncs.pop_front().unwrap_or_else(|| {
            Err(TradovateError::SyncTransport {
                message: "missing fake initial sync snapshot".to_owned(),
            })
        })
    }

    async fn next_event(&self) -> Result<Option<TradovateSyncEvent>, TradovateError> {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .events
            .pop_front()
            .unwrap_or(Ok(None))
    }

    async fn disconnect(&self) -> Result<(), TradovateError> {
        self.state
            .lock()
            .expect("sync mutex should not poison")
            .disconnect_count += 1;
        Ok(())
    }
}

fn sample_credentials() -> TradovateCredentials {
    TradovateCredentials {
        username: "bot-user".to_owned(),
        password: SecretString::new("password".to_owned().into()),
        cid: "cid-123".to_owned(),
        sec: SecretString::new("sec-456".to_owned().into()),
        app_id: "tv-bot-core".to_owned(),
        app_version: "0.1.0".to_owned(),
        device_id: Some("desktop".to_owned()),
    }
}

fn sample_token(now: DateTime<Utc>, expires_in_minutes: i64) -> TradovateAccessToken {
    TradovateAccessToken {
        access_token: SecretString::new("access-token".to_owned().into()),
        expiration_time: now + Duration::minutes(expires_in_minutes),
        issued_at: now,
        user_id: Some(7),
        person_id: Some(11),
        market_data_access: Some("realtime".to_owned()),
    }
}

fn sample_account(id: i64, name: &str) -> TradovateAccount {
    TradovateAccount {
        account_id: id,
        account_name: name.to_owned(),
        nickname: None,
        active: true,
    }
}

fn empty_sync_snapshot(now: DateTime<Utc>) -> TradovateSyncSnapshot {
    TradovateSyncSnapshot {
        occurred_at: now,
        positions: Vec::new(),
        working_orders: Vec::new(),
        fills: Vec::new(),
        account_snapshot: None,
        mismatch_reason: None,
        detail: "synced".to_owned(),
    }
}

fn snapshot_with_position(now: DateTime<Utc>) -> TradovateSyncSnapshot {
    TradovateSyncSnapshot {
        occurred_at: now,
        positions: vec![BrokerPositionSnapshot {
            account_id: Some("101".to_owned()),
            symbol: "GCM6".to_owned(),
            quantity: 1,
            average_price: None,
            realized_pnl: None,
            unrealized_pnl: None,
            protective_orders_present: true,
            captured_at: now,
        }],
        working_orders: vec![BrokerOrderUpdate {
            broker_order_id: "order-1".to_owned(),
            account_id: Some("101".to_owned()),
            symbol: "GCM6".to_owned(),
            side: Some(TradeSide::Buy),
            quantity: Some(1),
            order_type: Some(tv_bot_core_types::EntryOrderType::Limit),
            status: BrokerOrderStatus::Working,
            filled_quantity: 0,
            average_fill_price: None,
            updated_at: now,
        }],
        fills: vec![BrokerFillUpdate {
            fill_id: "fill-1".to_owned(),
            broker_order_id: Some("order-1".to_owned()),
            account_id: Some("101".to_owned()),
            symbol: "GCM6".to_owned(),
            side: TradeSide::Buy,
            quantity: 1,
            price: Decimal::new(2_385_10, 2),
            fee: Some(Decimal::new(125, 2)),
            commission: Some(Decimal::new(250, 2)),
            occurred_at: now,
        }],
        account_snapshot: Some(BrokerAccountSnapshot {
            account_id: "101".to_owned(),
            account_name: Some("paper-primary".to_owned()),
            cash_balance: Some(Decimal::new(100_000, 0)),
            available_funds: Some(Decimal::new(90_000, 0)),
            excess_liquidity: Some(Decimal::new(85_000, 0)),
            margin_used: Some(Decimal::new(5_000, 0)),
            net_liquidation_value: Some(Decimal::new(101_500, 0)),
            realized_pnl: Some(Decimal::new(250, 0)),
            unrealized_pnl: Some(Decimal::new(150, 0)),
            risk_state: Some("healthy".to_owned()),
            captured_at: now,
        }),
        mismatch_reason: None,
        detail: "existing position".to_owned(),
    }
}

fn sample_config(environment: BrokerEnvironment) -> TradovateSessionConfig {
    let host = match environment {
        BrokerEnvironment::Live => "live",
        BrokerEnvironment::Demo | BrokerEnvironment::Custom => "demo",
    };

    TradovateSessionConfig::new(
        environment,
        format!("https://{host}.tradovateapi.com/v1"),
        format!("wss://{host}.tradovateapi.com/v1/websocket"),
    )
    .expect("config should be valid")
}

#[tokio::test]
async fn authenticate_select_account_and_connect_sync() {
    let now = Utc::now();
    let clock = MutableClock::new(now);
    let auth_api = FakeAuthApi::with_request_token(sample_token(now, 90));
    let account_api = FakeAccountApi::with_accounts(vec![sample_account(101, "paper-primary")]);
    let sync_api = FakeSyncApi::with_initial_sync(empty_sync_snapshot(now));

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences {
            paper_account_name: Some("paper-primary".to_owned()),
            live_account_name: None,
        },
        auth_api.clone(),
        account_api.clone(),
        sync_api.clone(),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    let selected = manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect("paper account should select");
    manager
        .connect_user_sync()
        .await
        .expect("sync should connect");

    let snapshot = manager.snapshot();

    assert_eq!(selected.account_name, "paper-primary");
    assert_eq!(
        snapshot.broker.connection_state,
        BrokerConnectionState::Connected
    );
    assert_eq!(snapshot.broker.health, BrokerHealth::Healthy);
    assert_eq!(snapshot.broker.sync_state, BrokerSyncState::Synchronized);
    assert_eq!(
        snapshot
            .broker
            .selected_account
            .expect("selected account")
            .routing,
        BrokerAccountRouting::Paper
    );
    assert_eq!(auth_api.request_count(), 1);
    assert_eq!(account_api.request_count(), 1);
    assert_eq!(sync_api.connect_count(), 1);
    assert_eq!(sync_api.sync_request_count(), 1);
}

#[tokio::test]
async fn renews_access_token_before_expiry() {
    let now = Utc::now();
    let clock = MutableClock::new(now);
    let auth_api = FakeAuthApi::with_request_token(sample_token(now, 2));
    auth_api.push_renewed_token(sample_token(now + Duration::minutes(1), 90));

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences::default(),
        auth_api.clone(),
        FakeAccountApi::with_accounts(vec![sample_account(101, "paper-primary")]),
        FakeSyncApi::with_initial_sync(empty_sync_snapshot(now)),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    let renewed = manager
        .renew_access_token_if_needed()
        .await
        .expect("renewal should work");

    assert!(renewed);
    assert_eq!(auth_api.renewal_count(), 1);
    assert!(
        manager
            .snapshot()
            .token_expires_at
            .expect("token expiry should exist")
            > now + Duration::minutes(30)
    );
}

#[tokio::test]
async fn multiple_accounts_require_explicit_selection_without_preference() {
    let now = Utc::now();
    let clock = MutableClock::new(now);
    let auth_api = FakeAuthApi::with_request_token(sample_token(now, 90));
    let account_api = FakeAccountApi::with_accounts(vec![
        sample_account(101, "paper-one"),
        sample_account(102, "paper-two"),
    ]);

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences::default(),
        auth_api,
        account_api,
        FakeSyncApi::with_initial_sync(empty_sync_snapshot(now)),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    let error = manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect_err("selection should require an explicit account");

    assert_eq!(
        error,
        TradovateError::AccountSelectionRequired {
            routing: BrokerAccountRouting::Paper,
        }
    );
}

#[tokio::test]
async fn paper_routing_rejects_live_environment() {
    let now = Utc::now();
    let clock = MutableClock::new(now);

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Live),
        sample_credentials(),
        TradovateRoutingPreferences {
            paper_account_name: Some("live-primary".to_owned()),
            live_account_name: None,
        },
        FakeAuthApi::with_request_token(sample_token(now, 90)),
        FakeAccountApi::with_accounts(vec![sample_account(201, "live-primary")]),
        FakeSyncApi::with_initial_sync(empty_sync_snapshot(now)),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    let error = manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect_err("paper mode should reject live environment");

    assert_eq!(
        error,
        TradovateError::EnvironmentRouteMismatch {
            environment: BrokerEnvironment::Live,
            routing: BrokerAccountRouting::Paper,
        }
    );
}

#[tokio::test]
async fn reconnect_with_existing_position_requires_manual_review() {
    let now = Utc::now();
    let clock = MutableClock::new(now);
    let sync_api = FakeSyncApi::with_initial_sync(empty_sync_snapshot(now));

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences {
            paper_account_name: Some("paper-primary".to_owned()),
            live_account_name: None,
        },
        FakeAuthApi::with_request_token(sample_token(now, 90)),
        FakeAccountApi::with_accounts(vec![sample_account(101, "paper-primary")]),
        sync_api.clone(),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect("paper account should select");
    manager
        .connect_user_sync()
        .await
        .expect("initial sync should connect");

    sync_api.push_event(TradovateSyncEvent::Disconnected {
        occurred_at: now + Duration::seconds(5),
        reason: "socket reset".to_owned(),
    });
    manager
        .poll_next_event()
        .await
        .expect("disconnect event should apply");

    sync_api.push_initial_sync(snapshot_with_position(now + Duration::seconds(10)));
    manager
        .reconnect_user_sync()
        .await
        .expect("reconnect sync should succeed");

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.broker.sync_state, BrokerSyncState::ReviewRequired);
    assert_eq!(snapshot.broker.health, BrokerHealth::Degraded);
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(
        snapshot
            .account_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.account_name.as_deref()),
        Some("paper-primary")
    );
    assert_eq!(
        snapshot.broker.review_required_reason.as_deref(),
        Some("existing broker-side position or working orders detected after reconnect")
    );

    manager.acknowledge_reconnect_review(TradovateReconnectDecision::ReattachBotManagement);
    let reviewed = manager.snapshot();
    assert_eq!(reviewed.broker.sync_state, BrokerSyncState::Synchronized);
    assert_eq!(reviewed.broker.health, BrokerHealth::Healthy);
    assert_eq!(
        reviewed.last_review_decision,
        Some(TradovateReconnectDecision::ReattachBotManagement)
    );
}

#[tokio::test]
async fn heartbeat_gap_marks_sync_stale() {
    let now = Utc::now();
    let clock = MutableClock::new(now);

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences {
            paper_account_name: Some("paper-primary".to_owned()),
            live_account_name: None,
        },
        FakeAuthApi::with_request_token(sample_token(now, 90)),
        FakeAccountApi::with_accounts(vec![sample_account(101, "paper-primary")]),
        FakeSyncApi::with_initial_sync(empty_sync_snapshot(now)),
        clock.clone(),
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect("paper account should select");
    manager
        .connect_user_sync()
        .await
        .expect("sync should connect");

    clock.set(now + Duration::seconds(45));
    let snapshot = manager.snapshot();

    assert_eq!(snapshot.broker.sync_state, BrokerSyncState::Stale);
    assert_eq!(snapshot.broker.health, BrokerHealth::Degraded);
}

#[tokio::test]
async fn mismatch_event_degrades_broker_health() {
    let now = Utc::now();
    let clock = MutableClock::new(now);
    let sync_api = FakeSyncApi::with_initial_sync(empty_sync_snapshot(now));

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences {
            paper_account_name: Some("paper-primary".to_owned()),
            live_account_name: None,
        },
        FakeAuthApi::with_request_token(sample_token(now, 90)),
        FakeAccountApi::with_accounts(vec![sample_account(101, "paper-primary")]),
        sync_api.clone(),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect("paper account should select");
    manager
        .connect_user_sync()
        .await
        .expect("sync should connect");

    sync_api.push_event(TradovateSyncEvent::Mismatch {
        occurred_at: now + Duration::seconds(5),
        detail: "broker position mismatch detected".to_owned(),
    });
    manager
        .poll_next_event()
        .await
        .expect("mismatch event should apply");

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.broker.sync_state, BrokerSyncState::Mismatch);
    assert_eq!(snapshot.broker.health, BrokerHealth::Degraded);
    assert_eq!(
        snapshot.broker.review_required_reason.as_deref(),
        Some("broker position mismatch detected")
    );
}

#[tokio::test]
async fn session_manager_submits_order_requests_with_selected_account_context() {
    let now = Utc::now();
    let clock = MutableClock::new(now);
    let execution_api = FakeExecutionApi::default();

    let mut manager = TradovateSessionManager::new(
        sample_config(BrokerEnvironment::Demo),
        sample_credentials(),
        TradovateRoutingPreferences {
            paper_account_name: Some("paper-primary".to_owned()),
            live_account_name: None,
        },
        FakeAuthApi::with_request_token(sample_token(now, 90)),
        FakeAccountApi::with_accounts(vec![sample_account(101, "paper-primary")]),
        FakeSyncApi::with_initial_sync(empty_sync_snapshot(now)),
        clock,
    )
    .expect("manager should be created");

    manager.authenticate().await.expect("auth should succeed");
    manager
        .select_account_for_mode(&RuntimeMode::Paper)
        .await
        .expect("paper account should select");

    let order_result = manager
        .place_order(
            &execution_api,
            TradovateOrderPlacement {
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                quantity: 1,
                order_type: TradovateOrderType::Limit,
                limit_price: Some(Decimal::new(2_385_10, 2)),
                stop_price: None,
                time_in_force: Some(TradovateTimeInForce::Day),
                expire_time: None,
                text: Some("entry".to_owned()),
                activation_time: None,
                custom_tag_50: Some("bot".to_owned()),
                is_automated: true,
            },
        )
        .await
        .expect("place order should succeed");

    let oso_result = manager
        .place_oso(
            &execution_api,
            TradovateOsoOrderPlacement {
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                quantity: 1,
                order_type: TradovateOrderType::Limit,
                limit_price: Some(Decimal::new(2_385_10, 2)),
                stop_price: None,
                time_in_force: Some(TradovateTimeInForce::Day),
                expire_time: None,
                text: Some("entry-bracket".to_owned()),
                activation_time: None,
                custom_tag_50: Some("bot".to_owned()),
                is_automated: true,
                brackets: vec![TradovateBracketOrder {
                    side: TradeSide::Sell,
                    quantity: None,
                    order_type: TradovateOrderType::Limit,
                    limit_price: Some(Decimal::new(2_400_00, 2)),
                    stop_price: None,
                    time_in_force: Some(TradovateTimeInForce::Gtc),
                    expire_time: None,
                    text: Some("tp".to_owned()),
                    activation_time: None,
                    custom_tag_50: None,
                }],
            },
        )
        .await
        .expect("place oso should succeed");

    let liquidation_result = manager
        .liquidate_position(&execution_api, 555, Some("flatten".to_owned()))
        .await
        .expect("liquidate should succeed");

    let state = execution_api
        .state
        .lock()
        .expect("execution mutex should not poison");

    assert_eq!(order_result.order_id, 9101);
    assert_eq!(oso_result.order_id, 9201);
    assert_eq!(liquidation_result.order_id, 9301);
    assert_eq!(state.place_orders.len(), 1);
    assert_eq!(state.place_orders[0].context.account_id, 101);
    assert_eq!(state.place_orders[0].context.account_spec, "paper-primary");
    assert_eq!(state.place_osos.len(), 1);
    assert_eq!(state.liquidations.len(), 1);
    assert_eq!(state.liquidations[0].contract_id, 555);
}
