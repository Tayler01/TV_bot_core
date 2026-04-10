use std::{collections::BTreeMap, time::Duration as StdDuration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tv_bot_core_types::{
    BrokerAccountSnapshot, BrokerFillUpdate, BrokerOrderStatus, BrokerOrderUpdate,
    BrokerPositionSnapshot, TradeSide,
};

use crate::{
    TradovateAccessToken, TradovateAccount, TradovateAccountApi, TradovateAccountListRequest,
    TradovateAuthApi, TradovateAuthRequest, TradovateError, TradovateRenewAccessTokenRequest,
    TradovateSyncApi, TradovateSyncConnectRequest, TradovateSyncEvent, TradovateSyncSnapshot,
    TradovateUserSyncRequest,
};

const AUTH_REQUEST_ID: u64 = 0;

type SocketStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovateLiveClientConfig {
    pub user_sync_entity_types: Vec<String>,
    pub split_responses: bool,
    pub request_timeout: StdDuration,
}

impl Default for TradovateLiveClientConfig {
    fn default() -> Self {
        Self {
            user_sync_entity_types: vec![
                "user".to_owned(),
                "account".to_owned(),
                "position".to_owned(),
                "order".to_owned(),
                "fill".to_owned(),
                "fillPair".to_owned(),
                "executionReport".to_owned(),
                "accountRiskStatus".to_owned(),
                "cashBalance".to_owned(),
            ],
            split_responses: false,
            request_timeout: StdDuration::from_secs(10),
        }
    }
}

pub struct TradovateLiveClient {
    pub(crate) http_client: Client,
    config: TradovateLiveClientConfig,
    socket_state: Mutex<LiveSocketState>,
}

impl TradovateLiveClient {
    pub fn new(config: TradovateLiveClientConfig) -> Self {
        Self::with_http_client(Client::new(), config)
    }

    pub fn with_http_client(http_client: Client, config: TradovateLiveClientConfig) -> Self {
        Self {
            http_client,
            config,
            socket_state: Mutex::new(LiveSocketState::default()),
        }
    }
}

#[async_trait]
impl TradovateAuthApi for TradovateLiveClient {
    async fn request_access_token(
        &self,
        request: TradovateAuthRequest,
    ) -> Result<TradovateAccessToken, TradovateError> {
        let cid = parse_json_string_or_number(&request.credentials.cid);
        let body = json!({
            "name": request.credentials.username,
            "password": request.credentials.password.expose_secret(),
            "appId": request.credentials.app_id,
            "appVersion": request.credentials.app_version,
            "deviceId": request.credentials.device_id,
            "cid": cid,
            "sec": request.credentials.sec.expose_secret(),
        });

        let response = self
            .http_client
            .post(build_http_url(
                &request.http_base_url,
                "auth/accesstokenrequest",
            ))
            .json(&body)
            .send()
            .await
            .map_err(|error| TradovateError::AuthTransport {
                message: error.to_string(),
            })?;

        let response =
            response
                .error_for_status()
                .map_err(|error| TradovateError::AuthTransport {
                    message: error.to_string(),
                })?;

        let payload = response
            .json::<AccessTokenResponse>()
            .await
            .map_err(|error| TradovateError::AuthTransport {
                message: error.to_string(),
            })?;

        payload.into_access_token()
    }

    async fn renew_access_token(
        &self,
        request: TradovateRenewAccessTokenRequest,
    ) -> Result<TradovateAccessToken, TradovateError> {
        let response = self
            .http_client
            .get(build_http_url(
                &request.http_base_url,
                "auth/renewaccesstoken",
            ))
            .bearer_auth(request.current_token.access_token.expose_secret())
            .send()
            .await
            .map_err(|error| TradovateError::AuthTransport {
                message: error.to_string(),
            })?;

        let response =
            response
                .error_for_status()
                .map_err(|error| TradovateError::AuthTransport {
                    message: error.to_string(),
                })?;

        let payload = response
            .json::<AccessTokenResponse>()
            .await
            .map_err(|error| TradovateError::AuthTransport {
                message: error.to_string(),
            })?;

        payload.into_access_token()
    }
}

#[async_trait]
impl TradovateAccountApi for TradovateLiveClient {
    async fn list_accounts(
        &self,
        request: TradovateAccountListRequest,
    ) -> Result<Vec<TradovateAccount>, TradovateError> {
        let response = self
            .http_client
            .get(build_http_url(&request.http_base_url, "account/list"))
            .bearer_auth(request.access_token.access_token.expose_secret())
            .send()
            .await
            .map_err(|error| TradovateError::AccountTransport {
                message: error.to_string(),
            })?;

        let response =
            response
                .error_for_status()
                .map_err(|error| TradovateError::AccountTransport {
                    message: error.to_string(),
                })?;

        let payload = response
            .json::<Vec<AccountResponse>>()
            .await
            .map_err(|error| TradovateError::AccountTransport {
                message: error.to_string(),
            })?;

        Ok(payload
            .into_iter()
            .map(|account| TradovateAccount {
                account_id: account.id,
                account_name: account.name,
                nickname: account.nickname,
                active: account.active.unwrap_or(true),
            })
            .collect())
    }
}

#[async_trait]
impl TradovateSyncApi for TradovateLiveClient {
    async fn connect(&self, request: TradovateSyncConnectRequest) -> Result<(), TradovateError> {
        let mut state = self.socket_state.lock().await;

        if let Some(stream) = state.stream.as_mut() {
            let _ = stream.close(None).await;
        }

        let connection = timeout(
            self.config.request_timeout,
            connect_async(&request.websocket_url),
        )
        .await
        .map_err(|_| TradovateError::SyncTransport {
            message: "timed out while opening Tradovate websocket".to_owned(),
        })?
        .map_err(|error| TradovateError::SyncTransport {
            message: error.to_string(),
        })?;

        let mut stream = connection.0;
        wait_for_open_frame(&mut stream, self.config.request_timeout).await?;

        send_text_message(
            &mut stream,
            format!(
                "authorize\n{AUTH_REQUEST_ID}\n\n{}",
                request.access_token.access_token.expose_secret()
            ),
        )
        .await?;

        let auth_response =
            wait_for_response(&mut stream, AUTH_REQUEST_ID, self.config.request_timeout).await?;
        ensure_success_response(&auth_response, "authorize")?;

        state.stream = Some(stream);
        state.next_request_id = AUTH_REQUEST_ID + 1;
        state.cache = SyncCache::default();

        Ok(())
    }

    async fn request_user_sync(
        &self,
        request: TradovateUserSyncRequest,
    ) -> Result<TradovateSyncSnapshot, TradovateError> {
        let mut state = self.socket_state.lock().await;
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.saturating_add(1);

        let stream = state
            .stream
            .as_mut()
            .ok_or_else(|| TradovateError::SyncTransport {
                message: "Tradovate websocket is not connected".to_owned(),
            })?;

        let body = json!({
            "splitResponses": self.config.split_responses,
            "accounts": [request.account_id],
            "entityTypes": self.config.user_sync_entity_types,
        });

        send_text_message(
            stream,
            format!("user/syncrequest\n{request_id}\n\n{}", body),
        )
        .await?;

        let response = wait_for_response(stream, request_id, self.config.request_timeout).await?;
        ensure_success_response(&response, "user/syncrequest")?;

        let snapshot = state.cache.replace_from_sync_payload(
            response.payload.unwrap_or(Value::Null),
            request.account_id,
        )?;
        Ok(snapshot)
    }

    async fn next_event(&self) -> Result<Option<TradovateSyncEvent>, TradovateError> {
        let mut state = self.socket_state.lock().await;

        loop {
            let message = {
                let stream = match state.stream.as_mut() {
                    Some(stream) => stream,
                    None => return Ok(None),
                };

                read_socket_message(stream, self.config.request_timeout).await?
            };

            match message {
                SocketMessage::Heartbeat => {
                    let stream = match state.stream.as_mut() {
                        Some(stream) => stream,
                        None => return Ok(None),
                    };
                    send_text_message(stream, "[]").await?;
                    return Ok(Some(TradovateSyncEvent::Heartbeat {
                        occurred_at: Utc::now(),
                    }));
                }
                SocketMessage::ResponseBatch(batch) => {
                    if let Some(error_response) =
                        batch.iter().find(|response| response.status >= 400)
                    {
                        return Err(TradovateError::SyncTransport {
                            message: response_error_message(error_response, "websocket request"),
                        });
                    }
                }
                SocketMessage::Props(props) => {
                    if let Some(snapshot) = state.cache.apply_props(props) {
                        return Ok(Some(TradovateSyncEvent::SyncSnapshot { snapshot }));
                    }
                }
                SocketMessage::Disconnected(reason) => {
                    state.stream = None;
                    return Ok(Some(TradovateSyncEvent::Disconnected {
                        occurred_at: Utc::now(),
                        reason,
                    }));
                }
                SocketMessage::Ignore => {}
            }
        }
    }

    async fn disconnect(&self) -> Result<(), TradovateError> {
        let mut state = self.socket_state.lock().await;

        if let Some(stream) = state.stream.as_mut() {
            stream
                .close(None)
                .await
                .map_err(|error| TradovateError::SyncTransport {
                    message: error.to_string(),
                })?;
        }

        state.stream = None;
        state.cache = SyncCache::default();

        Ok(())
    }
}

#[derive(Default)]
struct LiveSocketState {
    stream: Option<SocketStream>,
    next_request_id: u64,
    cache: SyncCache,
}

#[derive(Default)]
struct SyncCache {
    positions: BTreeMap<String, BrokerPositionSnapshot>,
    working_orders: BTreeMap<String, BrokerOrderUpdate>,
    fills: BTreeMap<String, BrokerFillUpdate>,
    account_snapshot: Option<BrokerAccountSnapshot>,
}

impl SyncCache {
    fn replace_from_sync_payload(
        &mut self,
        payload: Value,
        account_id: i64,
    ) -> Result<TradovateSyncSnapshot, TradovateError> {
        let occurred_at = Utc::now();
        self.positions.clear();
        self.working_orders.clear();
        self.fills.clear();
        self.account_snapshot = None;

        for position in extract_entities(&payload, &["positions", "position"]) {
            if let Some(position) = parse_position_snapshot(position, occurred_at) {
                if position.quantity != 0 {
                    self.positions.insert(position.symbol.clone(), position);
                }
            }
        }

        for order in extract_entities(&payload, &["orders", "order"]) {
            if let Some(order) = parse_order_update(order, occurred_at) {
                if is_working_order(&order.status) {
                    self.working_orders
                        .insert(order.broker_order_id.clone(), order);
                }
            }
        }

        for fill in extract_entities(&payload, &["fills", "fill"]) {
            if let Some(fill) = parse_fill_update(fill, occurred_at) {
                self.fills.insert(fill.fill_id.clone(), fill);
            }
        }

        for entity in extract_entities(
            &payload,
            &[
                "accounts",
                "account",
                "cashBalances",
                "cashBalance",
                "accountRiskStatuses",
                "accountRiskStatus",
            ],
        ) {
            self.account_snapshot = merge_account_snapshot(
                self.account_snapshot.take(),
                entity,
                occurred_at,
                Some(account_id),
            );
        }

        Ok(self.snapshot(occurred_at, "Tradovate user sync snapshot loaded"))
    }

    fn apply_props(&mut self, props: PropsPayload) -> Option<TradovateSyncSnapshot> {
        let occurred_at = Utc::now();
        let entity_type = props.entity_type.to_ascii_lowercase();
        let event_type = props.event_type.to_ascii_lowercase();

        match entity_type.as_str() {
            "position" => {
                if event_type == "deleted" {
                    if let Some(symbol) = entity_symbol(&props.entity) {
                        self.positions.remove(&symbol);
                    }
                } else if let Some(position) = parse_position_snapshot(&props.entity, occurred_at) {
                    if position.quantity == 0 {
                        self.positions.remove(&position.symbol);
                    } else {
                        self.positions.insert(position.symbol.clone(), position);
                    }
                }

                Some(self.snapshot(occurred_at, "Tradovate position update applied"))
            }
            "order" | "executionreport" => {
                if event_type == "deleted" {
                    if let Some(order_id) = entity_order_id(&props.entity) {
                        self.working_orders.remove(&order_id);
                    }
                } else if let Some(order) = parse_order_update(&props.entity, occurred_at) {
                    if is_working_order(&order.status) {
                        self.working_orders
                            .insert(order.broker_order_id.clone(), order);
                    } else {
                        self.working_orders.remove(&order.broker_order_id);
                    }
                }

                Some(self.snapshot(occurred_at, "Tradovate order update applied"))
            }
            "fill" => {
                if event_type == "deleted" {
                    if let Some(fill_id) = entity_fill_id(&props.entity) {
                        self.fills.remove(&fill_id);
                    }
                } else if let Some(fill) = parse_fill_update(&props.entity, occurred_at) {
                    self.fills.insert(fill.fill_id.clone(), fill);
                }

                Some(self.snapshot(occurred_at, "Tradovate fill update applied"))
            }
            "account" | "cashbalance" | "accountriskstatus" => {
                if event_type == "deleted" {
                    self.account_snapshot = None;
                } else {
                    self.account_snapshot = merge_account_snapshot(
                        self.account_snapshot.take(),
                        &props.entity,
                        occurred_at,
                        None,
                    );
                }

                Some(self.snapshot(occurred_at, "Tradovate account update applied"))
            }
            _ => None,
        }
    }

    fn snapshot(
        &self,
        occurred_at: DateTime<Utc>,
        detail: impl Into<String>,
    ) -> TradovateSyncSnapshot {
        TradovateSyncSnapshot {
            occurred_at,
            positions: self.positions.values().cloned().collect(),
            working_orders: self.working_orders.values().cloned().collect(),
            fills: self.fills.values().cloned().collect(),
            account_snapshot: self.account_snapshot.clone(),
            mismatch_reason: None,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct AccessTokenResponse {
    #[serde(default, rename = "errorText")]
    error_text: Option<String>,
    #[serde(default, rename = "accessToken")]
    access_token: Option<String>,
    #[serde(default, rename = "expirationTime")]
    expiration_time: Option<DateTime<Utc>>,
    #[serde(default, rename = "userId")]
    user_id: Option<i64>,
    #[serde(default, rename = "personId")]
    person_id: Option<i64>,
    #[serde(default, rename = "mdAccessToken")]
    market_data_access: Option<String>,
}

impl AccessTokenResponse {
    fn into_access_token(self) -> Result<TradovateAccessToken, TradovateError> {
        if let Some(error_text) = self.error_text.filter(|value| !value.trim().is_empty()) {
            return Err(TradovateError::AuthTransport {
                message: error_text,
            });
        }

        let access_token = self
            .access_token
            .ok_or_else(|| TradovateError::AuthTransport {
                message: "Tradovate response did not include an access token".to_owned(),
            })?;
        let expiration_time =
            self.expiration_time
                .ok_or_else(|| TradovateError::AuthTransport {
                    message: "Tradovate response did not include an expiration time".to_owned(),
                })?;

        Ok(TradovateAccessToken {
            access_token: SecretString::new(access_token.into()),
            expiration_time,
            issued_at: Utc::now(),
            user_id: self.user_id,
            person_id: self.person_id,
            market_data_access: self.market_data_access,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct AccountResponse {
    id: i64,
    name: String,
    #[serde(default)]
    nickname: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
struct SocketResponseEnvelope {
    #[serde(rename = "i")]
    request_id: u64,
    #[serde(rename = "s")]
    status: u16,
    #[serde(default, rename = "d")]
    payload: Option<Value>,
    #[serde(default, rename = "errorText")]
    error_text: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct PropsEnvelope {
    e: String,
    d: PropsPayload,
}

#[derive(Clone, Debug, Deserialize)]
struct PropsPayload {
    #[serde(rename = "entityType")]
    entity_type: String,
    #[serde(rename = "eventType")]
    event_type: String,
    entity: Value,
}

enum SocketMessage {
    Heartbeat,
    ResponseBatch(Vec<SocketResponseEnvelope>),
    Props(PropsPayload),
    Disconnected(String),
    Ignore,
}

async fn wait_for_open_frame(
    stream: &mut SocketStream,
    timeout_window: StdDuration,
) -> Result<(), TradovateError> {
    loop {
        match read_socket_message(stream, timeout_window).await? {
            SocketMessage::Heartbeat => return Ok(()),
            SocketMessage::Disconnected(reason) => {
                return Err(TradovateError::SyncTransport { message: reason })
            }
            SocketMessage::Ignore | SocketMessage::ResponseBatch(_) | SocketMessage::Props(_) => {}
        }
    }
}

async fn wait_for_response(
    stream: &mut SocketStream,
    request_id: u64,
    timeout_window: StdDuration,
) -> Result<SocketResponseEnvelope, TradovateError> {
    loop {
        match read_socket_message(stream, timeout_window).await? {
            SocketMessage::Heartbeat => {
                send_text_message(stream, "[]").await?;
            }
            SocketMessage::ResponseBatch(batch) => {
                if let Some(response) = batch
                    .into_iter()
                    .find(|response| response.request_id == request_id)
                {
                    return Ok(response);
                }
            }
            SocketMessage::Props(_) | SocketMessage::Ignore => {}
            SocketMessage::Disconnected(reason) => {
                return Err(TradovateError::SyncTransport { message: reason })
            }
        }
    }
}

async fn read_socket_message(
    stream: &mut SocketStream,
    timeout_window: StdDuration,
) -> Result<SocketMessage, TradovateError> {
    let message = timeout(timeout_window, stream.next())
        .await
        .map_err(|_| TradovateError::SyncTransport {
            message: "timed out waiting for Tradovate websocket message".to_owned(),
        })?
        .ok_or_else(|| TradovateError::SyncTransport {
            message: "Tradovate websocket closed unexpectedly".to_owned(),
        })?
        .map_err(|error| TradovateError::SyncTransport {
            message: error.to_string(),
        })?;

    match message {
        Message::Text(text) => parse_socket_text(text.as_ref()),
        Message::Ping(payload) => {
            stream.send(Message::Pong(payload)).await.map_err(|error| {
                TradovateError::SyncTransport {
                    message: error.to_string(),
                }
            })?;
            Ok(SocketMessage::Ignore)
        }
        Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => Ok(SocketMessage::Ignore),
        Message::Close(frame) => Ok(SocketMessage::Disconnected(
            frame
                .map(|close| close.reason.to_string())
                .unwrap_or_else(|| "Tradovate websocket closed".to_owned()),
        )),
    }
}

fn parse_socket_text(text: &str) -> Result<SocketMessage, TradovateError> {
    if text == "o" {
        return Ok(SocketMessage::Heartbeat);
    }

    if let Some(batch) = text.strip_prefix('a') {
        let responses =
            serde_json::from_str::<Vec<SocketResponseEnvelope>>(batch).map_err(|error| {
                TradovateError::SyncTransport {
                    message: format!("failed to parse Tradovate websocket response batch: {error}"),
                }
            })?;

        return Ok(SocketMessage::ResponseBatch(responses));
    }

    let envelope = match serde_json::from_str::<PropsEnvelope>(text) {
        Ok(envelope) => envelope,
        Err(_) => return Ok(SocketMessage::Ignore),
    };

    if envelope.e.eq_ignore_ascii_case("props") {
        return Ok(SocketMessage::Props(envelope.d));
    }

    Ok(SocketMessage::Ignore)
}

async fn send_text_message(
    stream: &mut SocketStream,
    message: impl Into<String>,
) -> Result<(), TradovateError> {
    stream
        .send(Message::Text(message.into().into()))
        .await
        .map_err(|error| TradovateError::SyncTransport {
            message: error.to_string(),
        })
}

fn ensure_success_response(
    response: &SocketResponseEnvelope,
    operation: &str,
) -> Result<(), TradovateError> {
    if response.status >= 400 {
        return Err(TradovateError::SyncTransport {
            message: response_error_message(response, operation),
        });
    }

    Ok(())
}

fn response_error_message(response: &SocketResponseEnvelope, operation: &str) -> String {
    response
        .error_text
        .clone()
        .or_else(|| {
            response
                .payload
                .as_ref()
                .and_then(|payload| payload.get("errorText"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| {
            format!(
                "Tradovate {operation} failed with status {}",
                response.status
            )
        })
}

pub(crate) fn build_http_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn parse_json_string_or_number(value: &str) -> Value {
    value
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::from(value))
}

fn extract_entities<'a>(payload: &'a Value, keys: &[&str]) -> Vec<&'a Value> {
    match payload {
        Value::Object(object) => keys
            .iter()
            .filter_map(|key| object.get(*key))
            .flat_map(|value| match value {
                Value::Array(items) => items.iter().collect::<Vec<_>>(),
                Value::Object(_) => vec![value],
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_position_snapshot(
    value: &Value,
    occurred_at: DateTime<Utc>,
) -> Option<BrokerPositionSnapshot> {
    let quantity = value
        .get("netPos")
        .or_else(|| value.get("netQty"))
        .or_else(|| value.get("qty"))
        .and_then(value_to_i32)?;

    let symbol = entity_symbol(value)?;

    Some(BrokerPositionSnapshot {
        symbol,
        quantity,
        average_price: value
            .get("netPrice")
            .or_else(|| value.get("averagePrice"))
            .or_else(|| value.get("avgPrice"))
            .and_then(value_to_decimal),
        realized_pnl: value
            .get("realizedPnL")
            .or_else(|| value.get("realizedPnl"))
            .and_then(value_to_decimal),
        unrealized_pnl: value
            .get("unrealizedPnL")
            .or_else(|| value.get("unrealizedPnl"))
            .or_else(|| value.get("openPnL"))
            .and_then(value_to_decimal),
        protective_orders_present: false,
        captured_at: value
            .get("timestamp")
            .and_then(value_to_datetime)
            .unwrap_or(occurred_at),
    })
}

fn parse_order_update(value: &Value, occurred_at: DateTime<Utc>) -> Option<BrokerOrderUpdate> {
    let broker_order_id = entity_order_id(value)?;
    let status = order_status_from_value(value);

    Some(BrokerOrderUpdate {
        broker_order_id,
        symbol: entity_symbol(value).unwrap_or_else(|| "unknown".to_owned()),
        status,
        filled_quantity: value
            .get("fillQty")
            .or_else(|| value.get("filledQty"))
            .or_else(|| value.get("cumQty"))
            .and_then(value_to_u32)
            .unwrap_or(0),
        average_fill_price: value
            .get("avgFillPrice")
            .or_else(|| value.get("fillPrice"))
            .or_else(|| value.get("price"))
            .and_then(value_to_decimal),
        updated_at: value
            .get("timestamp")
            .or_else(|| value.get("transactTime"))
            .and_then(value_to_datetime)
            .unwrap_or(occurred_at),
    })
}

fn parse_fill_update(value: &Value, occurred_at: DateTime<Utc>) -> Option<BrokerFillUpdate> {
    Some(BrokerFillUpdate {
        fill_id: entity_fill_id(value)?,
        broker_order_id: value
            .get("orderId")
            .and_then(value_to_string)
            .or_else(|| value.get("ordId").and_then(value_to_string)),
        symbol: entity_symbol(value)?,
        side: value
            .get("action")
            .or_else(|| value.get("side"))
            .or_else(|| value.get("buySell"))
            .and_then(value_to_trade_side)?,
        quantity: value
            .get("qty")
            .or_else(|| value.get("fillQty"))
            .or_else(|| value.get("quantity"))
            .and_then(value_to_u32)?,
        price: value
            .get("price")
            .or_else(|| value.get("fillPrice"))
            .and_then(value_to_decimal)?,
        fee: value
            .get("fee")
            .or_else(|| value.get("fillFee"))
            .and_then(value_to_decimal),
        commission: value.get("commission").and_then(value_to_decimal),
        occurred_at: value
            .get("timestamp")
            .or_else(|| value.get("fillTime"))
            .and_then(value_to_datetime)
            .unwrap_or(occurred_at),
    })
}

fn merge_account_snapshot(
    snapshot: Option<BrokerAccountSnapshot>,
    value: &Value,
    occurred_at: DateTime<Utc>,
    fallback_account_id: Option<i64>,
) -> Option<BrokerAccountSnapshot> {
    let account_id = value
        .get("accountId")
        .or_else(|| value.get("id"))
        .and_then(value_to_string)
        .or_else(|| fallback_account_id.map(|account_id| account_id.to_string()))
        .or_else(|| {
            snapshot
                .as_ref()
                .map(|snapshot| snapshot.account_id.clone())
        })?;

    let mut snapshot = snapshot.unwrap_or(BrokerAccountSnapshot {
        account_id,
        account_name: None,
        cash_balance: None,
        available_funds: None,
        excess_liquidity: None,
        margin_used: None,
        net_liquidation_value: None,
        realized_pnl: None,
        unrealized_pnl: None,
        risk_state: None,
        captured_at: occurred_at,
    });

    if let Some(account_name) = value
        .get("name")
        .and_then(value_to_string)
        .or_else(|| value.get("accountSpec").and_then(value_to_string))
    {
        snapshot.account_name = Some(account_name);
    }

    assign_decimal(
        &mut snapshot.cash_balance,
        extract_decimal_field(value, &["cashBalance", "cash", "balance"]),
    );
    assign_decimal(
        &mut snapshot.available_funds,
        extract_decimal_field(value, &["availableFunds", "availableBalance"]),
    );
    assign_decimal(
        &mut snapshot.excess_liquidity,
        extract_decimal_field(value, &["excessLiquidity", "excessMargin", "excessEquity"]),
    );
    assign_decimal(
        &mut snapshot.margin_used,
        extract_decimal_field(value, &["marginUsed", "initialMarginReq", "maintMarginReq"]),
    );
    assign_decimal(
        &mut snapshot.net_liquidation_value,
        extract_decimal_field(
            value,
            &[
                "netLiq",
                "netLiqValue",
                "netLiquidatingValue",
                "netLiquidationValue",
            ],
        ),
    );
    assign_decimal(
        &mut snapshot.realized_pnl,
        extract_decimal_field(value, &["realizedPnL", "realizedPnl"]),
    );
    assign_decimal(
        &mut snapshot.unrealized_pnl,
        extract_decimal_field(value, &["unrealizedPnL", "unrealizedPnl", "openPnL"]),
    );

    if let Some(risk_state) = value
        .get("riskStatus")
        .and_then(value_to_string)
        .or_else(|| value.get("status").and_then(value_to_string))
    {
        snapshot.risk_state = Some(risk_state);
    }

    snapshot.captured_at = value
        .get("timestamp")
        .or_else(|| value.get("updatedAt"))
        .and_then(value_to_datetime)
        .unwrap_or(occurred_at);

    Some(snapshot)
}

fn entity_order_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(value_to_string)
        .or_else(|| value.get("orderId").and_then(value_to_string))
}

fn entity_fill_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(value_to_string)
        .or_else(|| value.get("fillId").and_then(value_to_string))
}

fn entity_symbol(value: &Value) -> Option<String> {
    value
        .get("symbol")
        .and_then(value_to_string)
        .or_else(|| value.get("contractSymbol").and_then(value_to_string))
        .or_else(|| {
            value
                .get("contractId")
                .and_then(value_to_string)
                .map(|id| format!("contract:{id}"))
        })
}

fn order_status_from_value(value: &Value) -> BrokerOrderStatus {
    let status = value
        .get("ordStatus")
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("working")
        .to_ascii_lowercase();

    match status.as_str() {
        "pending" | "pendingnew" | "submitted" => BrokerOrderStatus::Pending,
        "working" | "open" | "accepted" | "new" | "partiallyfilled" => BrokerOrderStatus::Working,
        "filled" => BrokerOrderStatus::Filled,
        "cancelled" | "canceled" | "expired" => BrokerOrderStatus::Cancelled,
        "rejected" => BrokerOrderStatus::Rejected,
        _ => BrokerOrderStatus::Working,
    }
}

fn is_working_order(status: &BrokerOrderStatus) -> bool {
    matches!(
        status,
        BrokerOrderStatus::Pending | BrokerOrderStatus::Working
    )
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn value_to_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Number(number) => number.as_i64().and_then(|value| i32::try_from(value).ok()),
        Value::String(text) => text.parse::<i32>().ok(),
        _ => None,
    }
}

fn value_to_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(text) => text.parse::<u32>().ok(),
        _ => None,
    }
}

fn value_to_decimal(value: &Value) -> Option<rust_decimal::Decimal> {
    match value {
        Value::Number(number) => rust_decimal::Decimal::from_str_exact(&number.to_string()).ok(),
        Value::String(text) => rust_decimal::Decimal::from_str_exact(text).ok(),
        _ => None,
    }
}

fn value_to_datetime(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|value| value.with_timezone(&Utc)),
        _ => None,
    }
}

fn value_to_trade_side(value: &Value) -> Option<TradeSide> {
    match value_to_string(value)?.to_ascii_lowercase().as_str() {
        "buy" => Some(TradeSide::Buy),
        "sell" => Some(TradeSide::Sell),
        _ => None,
    }
}

fn extract_decimal_field(value: &Value, keys: &[&str]) -> Option<rust_decimal::Decimal> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(value_to_decimal))
}

fn assign_decimal(
    target: &mut Option<rust_decimal::Decimal>,
    value: Option<rust_decimal::Decimal>,
) {
    if let Some(value) = value {
        *target = Some(value);
    }
}
