//! Tradovate auth, account-selection, and user-sync session foundations.

mod execution;
mod live;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tracing::{error, info, warn};
use tv_bot_core_types::{
    BrokerAccountRouting, BrokerAccountSelection, BrokerAccountSnapshot, BrokerConnectionState,
    BrokerEnvironment, BrokerFillUpdate, BrokerHealth, BrokerOrderUpdate, BrokerPositionSnapshot,
    BrokerStatusSnapshot, BrokerSyncState, RuntimeMode,
};

pub use execution::{
    TradovateBracketOrder, TradovateCancelOrderRequest, TradovateCancelOrderResult,
    TradovateExecutionApi, TradovateExecutionContext, TradovateLiquidatePositionRequest,
    TradovateLiquidatePositionResult, TradovateOrderPlacement, TradovateOrderType,
    TradovateOsoOrderPlacement, TradovatePlaceOrderRequest, TradovatePlaceOrderResult,
    TradovatePlaceOsoRequest, TradovatePlaceOsoResult, TradovateTimeInForce,
};
pub use live::{TradovateLiveClient, TradovateLiveClientConfig};

const PROVIDER_NAME: &str = "tradovate";
const DEFAULT_RENEW_BEFORE_EXPIRY: i64 = 15;
const DEFAULT_HEARTBEAT_STALE_AFTER_SECONDS: i64 = 30;
const DEFAULT_SYNC_STALE_AFTER_SECONDS: i64 = 15;

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TradovateRoutingPreferences {
    pub paper_account_name: Option<String>,
    pub live_account_name: Option<String>,
}

impl TradovateRoutingPreferences {
    fn selector_for(&self, routing: BrokerAccountRouting) -> TradovateAccountSelector {
        match routing {
            BrokerAccountRouting::Paper => self
                .paper_account_name
                .as_ref()
                .map(|name| TradovateAccountSelector::AccountName(name.clone()))
                .unwrap_or(TradovateAccountSelector::Auto),
            BrokerAccountRouting::Live => self
                .live_account_name
                .as_ref()
                .map(|name| TradovateAccountSelector::AccountName(name.clone()))
                .unwrap_or(TradovateAccountSelector::Auto),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TradovateCredentials {
    pub username: String,
    pub password: SecretString,
    pub cid: String,
    pub sec: SecretString,
    pub app_id: String,
    pub app_version: String,
    pub device_id: Option<String>,
}

impl TradovateCredentials {
    fn validate(&self) -> Result<(), TradovateError> {
        validate_non_empty_credential("username", &self.username)?;
        validate_secret("password", &self.password)?;
        validate_non_empty_credential("cid", &self.cid)?;
        validate_secret("sec", &self.sec)?;
        validate_non_empty_credential("app_id", &self.app_id)?;
        validate_non_empty_credential("app_version", &self.app_version)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovateSessionConfig {
    pub environment: BrokerEnvironment,
    pub http_base_url: String,
    pub websocket_url: String,
    pub renew_before_expiry: Duration,
    pub heartbeat_stale_after: Duration,
    pub sync_stale_after: Duration,
}

impl TradovateSessionConfig {
    pub fn new(
        environment: BrokerEnvironment,
        http_base_url: impl Into<String>,
        websocket_url: impl Into<String>,
    ) -> Result<Self, TradovateError> {
        let config = Self {
            environment,
            http_base_url: http_base_url.into(),
            websocket_url: websocket_url.into(),
            renew_before_expiry: Duration::minutes(DEFAULT_RENEW_BEFORE_EXPIRY),
            heartbeat_stale_after: Duration::seconds(DEFAULT_HEARTBEAT_STALE_AFTER_SECONDS),
            sync_stale_after: Duration::seconds(DEFAULT_SYNC_STALE_AFTER_SECONDS),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_timing(
        mut self,
        renew_before_expiry: Duration,
        heartbeat_stale_after: Duration,
        sync_stale_after: Duration,
    ) -> Result<Self, TradovateError> {
        self.renew_before_expiry = renew_before_expiry;
        self.heartbeat_stale_after = heartbeat_stale_after;
        self.sync_stale_after = sync_stale_after;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), TradovateError> {
        validate_non_empty("http_base_url", &self.http_base_url)?;
        validate_non_empty("websocket_url", &self.websocket_url)?;
        validate_positive_duration("renew_before_expiry", self.renew_before_expiry)?;
        validate_positive_duration("heartbeat_stale_after", self.heartbeat_stale_after)?;
        validate_positive_duration("sync_stale_after", self.sync_stale_after)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovateAccount {
    pub account_id: i64,
    pub account_name: String,
    pub nickname: Option<String>,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct TradovateAuthRequest {
    pub http_base_url: String,
    pub environment: BrokerEnvironment,
    pub credentials: TradovateCredentials,
}

#[derive(Clone, Debug)]
pub struct TradovateRenewAccessTokenRequest {
    pub http_base_url: String,
    pub environment: BrokerEnvironment,
    pub current_token: TradovateAccessToken,
}

#[derive(Clone, Debug)]
pub struct TradovateAccountListRequest {
    pub http_base_url: String,
    pub environment: BrokerEnvironment,
    pub access_token: TradovateAccessToken,
}

#[derive(Clone, Debug)]
pub struct TradovateSyncConnectRequest {
    pub websocket_url: String,
    pub environment: BrokerEnvironment,
    pub access_token: TradovateAccessToken,
}

#[derive(Clone, Debug)]
pub struct TradovateUserSyncRequest {
    pub account_id: i64,
    pub access_token: TradovateAccessToken,
}

#[derive(Clone, Debug)]
pub struct TradovateAccessToken {
    pub access_token: SecretString,
    pub expiration_time: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub user_id: Option<i64>,
    pub person_id: Option<i64>,
    pub market_data_access: Option<String>,
}

impl TradovateAccessToken {
    fn expires_within(&self, now: DateTime<Utc>, margin: Duration) -> bool {
        self.expiration_time - now <= margin
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradovateSyncSnapshot {
    pub occurred_at: DateTime<Utc>,
    pub positions: Vec<BrokerPositionSnapshot>,
    pub working_orders: Vec<BrokerOrderUpdate>,
    pub fills: Vec<BrokerFillUpdate>,
    pub account_snapshot: Option<BrokerAccountSnapshot>,
    pub mismatch_reason: Option<String>,
    pub detail: String,
}

impl TradovateSyncSnapshot {
    fn has_open_exposure(&self) -> bool {
        self.positions.iter().any(|position| position.quantity != 0)
            || !self.working_orders.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TradovateSyncEvent {
    Heartbeat {
        occurred_at: DateTime<Utc>,
    },
    SyncSnapshot {
        snapshot: TradovateSyncSnapshot,
    },
    Mismatch {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
    Disconnected {
        occurred_at: DateTime<Utc>,
        reason: String,
    },
    Reconnected {
        occurred_at: DateTime<Utc>,
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TradovateAccountSelector {
    Auto,
    AccountId(i64),
    AccountName(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradovateReconnectDecision {
    ClosePosition,
    LeaveBrokerProtected,
    ReattachBotManagement,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradovateSessionSnapshot {
    pub broker: BrokerStatusSnapshot,
    pub available_accounts: Vec<TradovateAccount>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub current_user_id: Option<i64>,
    pub last_review_decision: Option<TradovateReconnectDecision>,
    pub open_positions: Vec<BrokerPositionSnapshot>,
    pub working_orders: Vec<BrokerOrderUpdate>,
    pub fills: Vec<BrokerFillUpdate>,
    pub account_snapshot: Option<BrokerAccountSnapshot>,
}

#[async_trait]
pub trait TradovateAuthApi: Send + Sync {
    async fn request_access_token(
        &self,
        request: TradovateAuthRequest,
    ) -> Result<TradovateAccessToken, TradovateError>;

    async fn renew_access_token(
        &self,
        request: TradovateRenewAccessTokenRequest,
    ) -> Result<TradovateAccessToken, TradovateError>;
}

#[async_trait]
pub trait TradovateAccountApi: Send + Sync {
    async fn list_accounts(
        &self,
        request: TradovateAccountListRequest,
    ) -> Result<Vec<TradovateAccount>, TradovateError>;
}

#[async_trait]
pub trait TradovateSyncApi: Send + Sync {
    async fn connect(&self, request: TradovateSyncConnectRequest) -> Result<(), TradovateError>;

    async fn request_user_sync(
        &self,
        request: TradovateUserSyncRequest,
    ) -> Result<TradovateSyncSnapshot, TradovateError>;

    async fn next_event(&self) -> Result<Option<TradovateSyncEvent>, TradovateError>;

    async fn disconnect(&self) -> Result<(), TradovateError>;
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TradovateError {
    #[error("tradovate configuration field `{field}` must not be empty")]
    MissingConfigField { field: &'static str },
    #[error("tradovate credential `{field}` must not be empty")]
    MissingCredential { field: &'static str },
    #[error("tradovate duration `{field}` must be greater than zero")]
    InvalidDuration { field: &'static str },
    #[error("no access token is available; authenticate first")]
    NoAccessToken,
    #[error("no Tradovate account is selected")]
    NoSelectedAccount,
    #[error("no active Tradovate accounts are available")]
    NoAccountsAvailable,
    #[error(
        "multiple active Tradovate accounts are available for `{routing:?}` routing; explicit selection is required"
    )]
    AccountSelectionRequired { routing: BrokerAccountRouting },
    #[error("no active Tradovate account matched selector `{selector}`")]
    AccountNotFound { selector: String },
    #[error("runtime mode `{mode:?}` cannot select a trading account")]
    UnsupportedRuntimeMode { mode: RuntimeMode },
    #[error("Tradovate environment `{environment:?}` cannot be used for `{routing:?}` routing")]
    EnvironmentRouteMismatch {
        environment: BrokerEnvironment,
        routing: BrokerAccountRouting,
    },
    #[error("auth transport failed: {message}")]
    AuthTransport { message: String },
    #[error("account transport failed: {message}")]
    AccountTransport { message: String },
    #[error("sync transport failed: {message}")]
    SyncTransport { message: String },
    #[error("execution request is invalid: {message}")]
    InvalidExecutionRequest { message: String },
    #[error("execution transport failed: {message}")]
    ExecutionTransport { message: String },
    #[error("execution rejected by broker: {reason}: {message}")]
    ExecutionRejected { reason: String, message: String },
}

#[derive(Clone, Debug)]
struct SessionState {
    connection_state: BrokerConnectionState,
    sync_state: BrokerSyncState,
    access_token: Option<TradovateAccessToken>,
    available_accounts: Vec<TradovateAccount>,
    selected_account: Option<BrokerAccountSelection>,
    reconnect_count: u64,
    last_authenticated_at: Option<DateTime<Utc>>,
    last_heartbeat_at: Option<DateTime<Utc>>,
    last_sync_at: Option<DateTime<Utc>>,
    last_disconnect_reason: Option<String>,
    review_required_reason: Option<String>,
    last_review_decision: Option<TradovateReconnectDecision>,
    open_positions: Vec<BrokerPositionSnapshot>,
    working_orders: Vec<BrokerOrderUpdate>,
    fills: Vec<BrokerFillUpdate>,
    account_snapshot: Option<BrokerAccountSnapshot>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            connection_state: BrokerConnectionState::Disconnected,
            sync_state: BrokerSyncState::Disconnected,
            access_token: None,
            available_accounts: Vec::new(),
            selected_account: None,
            reconnect_count: 0,
            last_authenticated_at: None,
            last_heartbeat_at: None,
            last_sync_at: None,
            last_disconnect_reason: None,
            review_required_reason: None,
            last_review_decision: None,
            open_positions: Vec::new(),
            working_orders: Vec::new(),
            fills: Vec::new(),
            account_snapshot: None,
        }
    }
}

pub struct TradovateSessionManager<A, B, C, Clk = SystemClock> {
    auth_api: A,
    account_api: B,
    sync_api: C,
    clock: Clk,
    config: TradovateSessionConfig,
    credentials: TradovateCredentials,
    routing_preferences: TradovateRoutingPreferences,
    state: SessionState,
}

impl<A, B, C> TradovateSessionManager<A, B, C, SystemClock>
where
    A: TradovateAuthApi,
    B: TradovateAccountApi,
    C: TradovateSyncApi,
{
    pub fn with_system_clock(
        config: TradovateSessionConfig,
        credentials: TradovateCredentials,
        routing_preferences: TradovateRoutingPreferences,
        auth_api: A,
        account_api: B,
        sync_api: C,
    ) -> Result<Self, TradovateError> {
        Self::new(
            config,
            credentials,
            routing_preferences,
            auth_api,
            account_api,
            sync_api,
            SystemClock,
        )
    }
}

impl<A, B, C, Clk> TradovateSessionManager<A, B, C, Clk>
where
    A: TradovateAuthApi,
    B: TradovateAccountApi,
    C: TradovateSyncApi,
    Clk: Clock,
{
    pub fn new(
        config: TradovateSessionConfig,
        credentials: TradovateCredentials,
        routing_preferences: TradovateRoutingPreferences,
        auth_api: A,
        account_api: B,
        sync_api: C,
        clock: Clk,
    ) -> Result<Self, TradovateError> {
        config.validate()?;
        credentials.validate()?;

        Ok(Self {
            auth_api,
            account_api,
            sync_api,
            clock,
            config,
            credentials,
            routing_preferences,
            state: SessionState::default(),
        })
    }

    pub async fn authenticate(&mut self) -> Result<TradovateAccessToken, TradovateError> {
        self.state.connection_state = BrokerConnectionState::Authenticating;
        self.state.sync_state = BrokerSyncState::Pending;

        let request = TradovateAuthRequest {
            http_base_url: self.config.http_base_url.clone(),
            environment: self.config.environment,
            credentials: self.credentials.clone(),
        };

        match self.auth_api.request_access_token(request).await {
            Ok(token) => {
                let now = self.clock.now();
                self.state.access_token = Some(token.clone());
                self.state.last_authenticated_at = Some(now);
                self.state.connection_state = BrokerConnectionState::Authenticated;
                self.state.sync_state = BrokerSyncState::Pending;
                self.state.last_disconnect_reason = None;
                info!(
                    environment = ?self.config.environment,
                    expires_at = %token.expiration_time,
                    "Tradovate access token acquired"
                );
                Ok(token)
            }
            Err(error) => {
                self.mark_failed();
                error!(?error, "Tradovate authentication failed");
                Err(error)
            }
        }
    }

    pub async fn renew_access_token_if_needed(&mut self) -> Result<bool, TradovateError> {
        let token = self.current_access_token()?.clone();
        let now = self.clock.now();

        if !token.expires_within(now, self.config.renew_before_expiry) {
            return Ok(false);
        }

        let request = TradovateRenewAccessTokenRequest {
            http_base_url: self.config.http_base_url.clone(),
            environment: self.config.environment,
            current_token: token,
        };

        match self.auth_api.renew_access_token(request).await {
            Ok(token) => {
                self.state.access_token = Some(token.clone());
                self.state.last_authenticated_at = Some(now);
                info!(expires_at = %token.expiration_time, "Tradovate access token renewed");
                Ok(true)
            }
            Err(error) => {
                self.mark_failed();
                error!(?error, "Tradovate token renewal failed");
                Err(error)
            }
        }
    }

    pub async fn refresh_accounts(&mut self) -> Result<Vec<TradovateAccount>, TradovateError> {
        let request = TradovateAccountListRequest {
            http_base_url: self.config.http_base_url.clone(),
            environment: self.config.environment,
            access_token: self.current_access_token()?.clone(),
        };

        match self.account_api.list_accounts(request).await {
            Ok(accounts) => {
                self.state.available_accounts = accounts.clone();
                info!(
                    account_count = accounts.len(),
                    "Tradovate accounts refreshed"
                );
                Ok(accounts)
            }
            Err(error) => {
                self.state.sync_state = BrokerSyncState::Failed;
                error!(?error, "Tradovate account refresh failed");
                Err(error)
            }
        }
    }

    pub async fn select_account_for_mode(
        &mut self,
        mode: &RuntimeMode,
    ) -> Result<BrokerAccountSelection, TradovateError> {
        let routing = routing_for_mode(mode)?;
        let selector = self.routing_preferences.selector_for(routing);
        self.select_account(routing, selector).await
    }

    pub async fn select_account(
        &mut self,
        routing: BrokerAccountRouting,
        selector: TradovateAccountSelector,
    ) -> Result<BrokerAccountSelection, TradovateError> {
        validate_environment_for_routing(self.config.environment, routing)?;

        if self.state.available_accounts.is_empty() {
            self.refresh_accounts().await?;
        }

        let active_accounts = self
            .state
            .available_accounts
            .iter()
            .filter(|account| account.active)
            .cloned()
            .collect::<Vec<_>>();

        if active_accounts.is_empty() {
            return Err(TradovateError::NoAccountsAvailable);
        }

        let account = match selector {
            TradovateAccountSelector::Auto => {
                if active_accounts.len() == 1 {
                    active_accounts
                        .into_iter()
                        .next()
                        .expect("active accounts checked above")
                } else {
                    return Err(TradovateError::AccountSelectionRequired { routing });
                }
            }
            TradovateAccountSelector::AccountId(account_id) => active_accounts
                .into_iter()
                .find(|account| account.account_id == account_id)
                .ok_or_else(|| TradovateError::AccountNotFound {
                    selector: account_id.to_string(),
                })?,
            TradovateAccountSelector::AccountName(account_name) => active_accounts
                .into_iter()
                .find(|account| account_name_matches(&account.account_name, &account_name))
                .ok_or_else(|| TradovateError::AccountNotFound {
                    selector: account_name,
                })?,
        };

        let selection = BrokerAccountSelection {
            provider: PROVIDER_NAME.to_owned(),
            account_id: account.account_id.to_string(),
            account_name: account.account_name.clone(),
            routing,
            environment: self.config.environment,
            selected_at: self.clock.now(),
        };

        self.state.selected_account = Some(selection.clone());

        info!(
            account_id = account.account_id,
            account_name = %account.account_name,
            routing = ?routing,
            "Tradovate account selected"
        );

        Ok(selection)
    }

    pub async fn connect_user_sync(&mut self) -> Result<TradovateSyncSnapshot, TradovateError> {
        self.renew_access_token_if_needed().await?;

        let token = self.current_access_token()?.clone();
        let account = self
            .state
            .selected_account
            .as_ref()
            .ok_or(TradovateError::NoSelectedAccount)?
            .clone();
        let account_id = account
            .account_id
            .parse::<i64>()
            .map_err(|_| TradovateError::NoSelectedAccount)?;

        self.state.connection_state = BrokerConnectionState::Connecting;
        self.state.sync_state = BrokerSyncState::Pending;

        let connect_request = TradovateSyncConnectRequest {
            websocket_url: self.config.websocket_url.clone(),
            environment: self.config.environment,
            access_token: token.clone(),
        };

        if let Err(error) = self.sync_api.connect(connect_request).await {
            self.mark_failed();
            error!(?error, "Tradovate sync websocket connect failed");
            return Err(error);
        }

        let sync_request = TradovateUserSyncRequest {
            account_id,
            access_token: token,
        };

        match self.sync_api.request_user_sync(sync_request).await {
            Ok(snapshot) => {
                self.state.connection_state = BrokerConnectionState::Connected;
                self.state.last_disconnect_reason = None;
                let review_existing_state =
                    self.state.last_sync_at.is_none() || self.state.reconnect_count > 0;
                self.apply_sync_snapshot(snapshot.clone(), review_existing_state);
                info!(
                    account_id,
                    review_required = self.state.sync_state == BrokerSyncState::ReviewRequired,
                    "Tradovate user sync connected"
                );
                Ok(snapshot)
            }
            Err(error) => {
                self.mark_failed();
                error!(?error, "Tradovate initial user sync failed");
                Err(error)
            }
        }
    }

    pub async fn reconnect_user_sync(&mut self) -> Result<TradovateSyncSnapshot, TradovateError> {
        self.state.connection_state = BrokerConnectionState::Reconnecting;
        self.connect_user_sync().await
    }

    pub async fn poll_next_event(&mut self) -> Result<Option<TradovateSyncEvent>, TradovateError> {
        let event = match self.sync_api.next_event().await {
            Ok(event) => event,
            Err(error) => {
                self.mark_failed();
                error!(?error, "Tradovate sync event polling failed");
                return Err(error);
            }
        };

        if let Some(event_ref) = event.as_ref() {
            match event_ref {
                TradovateSyncEvent::Heartbeat { occurred_at } => {
                    self.state.last_heartbeat_at = Some(*occurred_at);
                }
                TradovateSyncEvent::SyncSnapshot { snapshot } => {
                    self.state.connection_state = BrokerConnectionState::Connected;
                    self.apply_sync_snapshot(snapshot.clone(), false);
                }
                TradovateSyncEvent::Mismatch { detail, .. } => {
                    self.state.connection_state = BrokerConnectionState::Connected;
                    self.state.sync_state = BrokerSyncState::Mismatch;
                    self.state.review_required_reason = Some(detail.clone());
                    warn!(detail = %detail, "Tradovate sync mismatch detected");
                }
                TradovateSyncEvent::Disconnected { reason, .. } => {
                    self.state.connection_state = BrokerConnectionState::Reconnecting;
                    self.state.sync_state = BrokerSyncState::Disconnected;
                    self.state.last_disconnect_reason = Some(reason.clone());
                    self.state.reconnect_count = self.state.reconnect_count.saturating_add(1);
                    warn!(
                        reason = %reason,
                        reconnect_count = self.state.reconnect_count,
                        "Tradovate sync disconnected"
                    );
                }
                TradovateSyncEvent::Reconnected { detail, .. } => {
                    self.state.connection_state = BrokerConnectionState::Connected;
                    self.state.sync_state = BrokerSyncState::Pending;
                    info!(detail = %detail, "Tradovate sync transport reconnected");
                }
            }
        }

        Ok(event)
    }

    pub async fn disconnect(&mut self) -> Result<(), TradovateError> {
        if let Err(error) = self.sync_api.disconnect().await {
            self.mark_failed();
            error!(?error, "Tradovate sync disconnect failed");
            return Err(error);
        }

        self.state.connection_state = BrokerConnectionState::Disconnected;
        self.state.sync_state = BrokerSyncState::Disconnected;
        info!("Tradovate sync disconnected");
        Ok(())
    }

    pub async fn place_order<E>(
        &mut self,
        execution_api: &E,
        order: TradovateOrderPlacement,
    ) -> Result<TradovatePlaceOrderResult, TradovateError>
    where
        E: TradovateExecutionApi,
    {
        self.renew_access_token_if_needed().await?;
        let symbol = order.symbol.clone();
        let result = execution_api
            .place_order(TradovatePlaceOrderRequest {
                context: self.execution_context()?,
                order,
            })
            .await?;

        info!(symbol = %symbol, order_id = result.order_id, "Tradovate order submitted");
        Ok(result)
    }

    pub async fn place_oso<E>(
        &mut self,
        execution_api: &E,
        order: TradovateOsoOrderPlacement,
    ) -> Result<TradovatePlaceOsoResult, TradovateError>
    where
        E: TradovateExecutionApi,
    {
        self.renew_access_token_if_needed().await?;
        let symbol = order.symbol.clone();
        let result = execution_api
            .place_oso(TradovatePlaceOsoRequest {
                context: self.execution_context()?,
                order,
            })
            .await?;

        info!(
            symbol = %symbol,
            order_id = result.order_id,
            oso1_id = ?result.oso1_id,
            oso2_id = ?result.oso2_id,
            "Tradovate OSO order submitted"
        );
        Ok(result)
    }

    pub async fn liquidate_position<E>(
        &mut self,
        execution_api: &E,
        contract_id: i64,
        custom_tag_50: Option<String>,
    ) -> Result<TradovateLiquidatePositionResult, TradovateError>
    where
        E: TradovateExecutionApi,
    {
        self.renew_access_token_if_needed().await?;
        let result = execution_api
            .liquidate_position(TradovateLiquidatePositionRequest {
                context: self.execution_context()?,
                contract_id,
                custom_tag_50,
                admin: false,
            })
            .await?;

        info!(
            contract_id,
            order_id = result.order_id,
            "Tradovate liquidation requested"
        );
        Ok(result)
    }

    pub async fn cancel_order<E>(
        &mut self,
        execution_api: &E,
        order_id: i64,
    ) -> Result<TradovateCancelOrderResult, TradovateError>
    where
        E: TradovateExecutionApi,
    {
        self.renew_access_token_if_needed().await?;
        let result = execution_api
            .cancel_order(TradovateCancelOrderRequest {
                context: self.execution_context()?,
                order_id,
                is_automated: true,
            })
            .await?;

        info!(
            order_id = result.order_id,
            "Tradovate order cancellation requested"
        );
        Ok(result)
    }

    pub fn acknowledge_reconnect_review(&mut self, decision: TradovateReconnectDecision) {
        self.state.last_review_decision = Some(decision);

        if self.state.sync_state == BrokerSyncState::ReviewRequired {
            self.state.sync_state = BrokerSyncState::Synchronized;
            self.state.review_required_reason = None;
            info!(decision = ?decision, "Tradovate reconnect review acknowledged");
        }
    }

    pub fn broker_status(&self) -> BrokerStatusSnapshot {
        let now = self.clock.now();
        let sync_state = self.effective_sync_state(now);
        let health = health_for(self.state.connection_state, sync_state);

        BrokerStatusSnapshot {
            provider: PROVIDER_NAME.to_owned(),
            environment: self.config.environment,
            connection_state: self.state.connection_state,
            health,
            sync_state,
            selected_account: self.state.selected_account.clone(),
            reconnect_count: self.state.reconnect_count,
            last_authenticated_at: self.state.last_authenticated_at,
            last_heartbeat_at: self.state.last_heartbeat_at,
            last_sync_at: self.state.last_sync_at,
            last_disconnect_reason: self.state.last_disconnect_reason.clone(),
            review_required_reason: self.state.review_required_reason.clone(),
            updated_at: now,
        }
    }

    pub fn snapshot(&self) -> TradovateSessionSnapshot {
        TradovateSessionSnapshot {
            broker: self.broker_status(),
            available_accounts: self.state.available_accounts.clone(),
            token_expires_at: self
                .state
                .access_token
                .as_ref()
                .map(|token| token.expiration_time),
            current_user_id: self
                .state
                .access_token
                .as_ref()
                .and_then(|token| token.user_id),
            last_review_decision: self.state.last_review_decision,
            open_positions: self.state.open_positions.clone(),
            working_orders: self.state.working_orders.clone(),
            fills: self.state.fills.clone(),
            account_snapshot: self.state.account_snapshot.clone(),
        }
    }

    fn apply_sync_snapshot(
        &mut self,
        snapshot: TradovateSyncSnapshot,
        review_existing_state: bool,
    ) {
        self.state.last_heartbeat_at = Some(snapshot.occurred_at);
        self.state.last_sync_at = Some(snapshot.occurred_at);
        self.state.open_positions = snapshot.positions.clone();
        self.state.working_orders = snapshot.working_orders.clone();
        self.state.fills = snapshot.fills.clone();
        self.state.account_snapshot = snapshot.account_snapshot.clone();

        if let Some(reason) = snapshot.mismatch_reason {
            self.state.sync_state = BrokerSyncState::Mismatch;
            self.state.review_required_reason = Some(reason);
            return;
        }

        if review_existing_state && snapshot.has_open_exposure() {
            self.state.sync_state = BrokerSyncState::ReviewRequired;
            self.state.review_required_reason =
                Some(existing_state_review_reason(self.state.reconnect_count > 0));
            return;
        }

        self.state.sync_state = BrokerSyncState::Synchronized;
        self.state.review_required_reason = None;
    }

    fn effective_sync_state(&self, now: DateTime<Utc>) -> BrokerSyncState {
        match self.state.sync_state {
            BrokerSyncState::Synchronized if self.is_stale(now) => BrokerSyncState::Stale,
            BrokerSyncState::Pending
                if self.state.connection_state == BrokerConnectionState::Connected
                    && self.is_stale(now) =>
            {
                BrokerSyncState::Stale
            }
            other => other,
        }
    }

    fn is_stale(&self, now: DateTime<Utc>) -> bool {
        let heartbeat_stale = self
            .state
            .last_heartbeat_at
            .map(|heartbeat| now - heartbeat > self.config.heartbeat_stale_after)
            .unwrap_or(false);

        let sync_stale = self
            .state
            .last_sync_at
            .map(|sync| now - sync > self.config.sync_stale_after)
            .unwrap_or(false);

        heartbeat_stale || sync_stale
    }

    fn current_access_token(&self) -> Result<&TradovateAccessToken, TradovateError> {
        self.state
            .access_token
            .as_ref()
            .ok_or(TradovateError::NoAccessToken)
    }

    fn execution_context(&self) -> Result<TradovateExecutionContext, TradovateError> {
        let selection = self
            .state
            .selected_account
            .as_ref()
            .ok_or(TradovateError::NoSelectedAccount)?;
        let account_id = selection
            .account_id
            .parse::<i64>()
            .map_err(|_| TradovateError::NoSelectedAccount)?;

        Ok(TradovateExecutionContext {
            http_base_url: self.config.http_base_url.clone(),
            access_token: self.current_access_token()?.clone(),
            account_id,
            account_spec: selection.account_name.clone(),
        })
    }

    fn mark_failed(&mut self) {
        self.state.connection_state = BrokerConnectionState::Failed;
        self.state.sync_state = BrokerSyncState::Failed;
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), TradovateError> {
    if value.trim().is_empty() {
        return Err(TradovateError::MissingConfigField { field });
    }

    Ok(())
}

fn validate_non_empty_credential(field: &'static str, value: &str) -> Result<(), TradovateError> {
    if value.trim().is_empty() {
        return Err(TradovateError::MissingCredential { field });
    }

    Ok(())
}

fn validate_secret(field: &'static str, value: &SecretString) -> Result<(), TradovateError> {
    if value.expose_secret().trim().is_empty() {
        return Err(TradovateError::MissingCredential { field });
    }

    Ok(())
}

fn validate_positive_duration(
    field: &'static str,
    duration: Duration,
) -> Result<(), TradovateError> {
    if duration <= Duration::zero() {
        return Err(TradovateError::InvalidDuration { field });
    }

    Ok(())
}

fn validate_environment_for_routing(
    environment: BrokerEnvironment,
    routing: BrokerAccountRouting,
) -> Result<(), TradovateError> {
    match (environment, routing) {
        (BrokerEnvironment::Demo, BrokerAccountRouting::Paper)
        | (BrokerEnvironment::Live, BrokerAccountRouting::Live)
        | (BrokerEnvironment::Custom, _) => Ok(()),
        _ => Err(TradovateError::EnvironmentRouteMismatch {
            environment,
            routing,
        }),
    }
}

fn routing_for_mode(mode: &RuntimeMode) -> Result<BrokerAccountRouting, TradovateError> {
    match mode {
        RuntimeMode::Paper => Ok(BrokerAccountRouting::Paper),
        RuntimeMode::Live => Ok(BrokerAccountRouting::Live),
        RuntimeMode::Observation | RuntimeMode::Paused => {
            Err(TradovateError::UnsupportedRuntimeMode { mode: mode.clone() })
        }
    }
}

fn health_for(
    connection_state: BrokerConnectionState,
    sync_state: BrokerSyncState,
) -> BrokerHealth {
    if matches!(sync_state, BrokerSyncState::Failed) {
        return BrokerHealth::Failed;
    }

    if matches!(sync_state, BrokerSyncState::Disconnected)
        && !matches!(connection_state, BrokerConnectionState::Reconnecting)
    {
        return BrokerHealth::Disconnected;
    }

    match connection_state {
        BrokerConnectionState::Failed => BrokerHealth::Failed,
        BrokerConnectionState::Disconnected => BrokerHealth::Disconnected,
        BrokerConnectionState::Authenticating
        | BrokerConnectionState::Authenticated
        | BrokerConnectionState::Connecting => BrokerHealth::Initializing,
        BrokerConnectionState::Reconnecting => BrokerHealth::Degraded,
        BrokerConnectionState::Connected => match sync_state {
            BrokerSyncState::Synchronized => BrokerHealth::Healthy,
            BrokerSyncState::Pending => BrokerHealth::Initializing,
            BrokerSyncState::Stale
            | BrokerSyncState::Mismatch
            | BrokerSyncState::ReviewRequired => BrokerHealth::Degraded,
            BrokerSyncState::Disconnected => BrokerHealth::Disconnected,
            BrokerSyncState::Failed => BrokerHealth::Failed,
        },
    }
}

fn account_name_matches(candidate: &str, selector: &str) -> bool {
    candidate.trim().eq_ignore_ascii_case(selector.trim())
}

fn existing_state_review_reason(is_reconnect: bool) -> String {
    if is_reconnect {
        "existing broker-side position or working orders detected after reconnect".to_owned()
    } else {
        "existing broker-side position or working orders detected at startup".to_owned()
    }
}
