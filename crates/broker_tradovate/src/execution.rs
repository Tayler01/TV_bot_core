use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{Map, Number, Value};
use std::str::FromStr;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use tracing::info;
use tv_bot_core_types::TradeSide;

use crate::{TradovateAccessToken, TradovateError, TradovateLiveClient};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradovateOrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    Mit,
    Qts,
    TrailingStop,
    TrailingStopLimit,
}

impl TradovateOrderType {
    fn as_api_str(self) -> &'static str {
        match self {
            Self::Market => "Market",
            Self::Limit => "Limit",
            Self::Stop => "Stop",
            Self::StopLimit => "StopLimit",
            Self::Mit => "MIT",
            Self::Qts => "QTS",
            Self::TrailingStop => "TrailingStop",
            Self::TrailingStopLimit => "TrailingStopLimit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradovateTimeInForce {
    Day,
    Fok,
    Gtc,
    Gtd,
    Ioc,
}

impl TradovateTimeInForce {
    fn as_api_str(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::Fok => "FOK",
            Self::Gtc => "GTC",
            Self::Gtd => "GTD",
            Self::Ioc => "IOC",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TradovateExecutionContext {
    pub http_base_url: String,
    pub access_token: TradovateAccessToken,
    pub account_id: i64,
    pub account_spec: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradovateOrderPlacement {
    pub symbol: String,
    pub side: TradeSide,
    pub quantity: u32,
    pub order_type: TradovateOrderType,
    pub limit_price: Option<Decimal>,
    pub stop_price: Option<Decimal>,
    pub time_in_force: Option<TradovateTimeInForce>,
    pub expire_time: Option<DateTime<Utc>>,
    pub text: Option<String>,
    pub activation_time: Option<DateTime<Utc>>,
    pub custom_tag_50: Option<String>,
    pub is_automated: bool,
}

impl TradovateOrderPlacement {
    fn validate(&self) -> Result<(), TradovateError> {
        validate_symbol(&self.symbol)?;
        validate_quantity(self.quantity)?;
        validate_price_requirements(
            self.order_type,
            self.limit_price.as_ref(),
            self.stop_price.as_ref(),
        )?;
        Ok(())
    }

    fn to_payload(&self, context: &TradovateExecutionContext) -> Result<Value, TradovateError> {
        self.validate()?;

        let mut payload = base_order_payload(
            context,
            self.side,
            &self.symbol,
            self.quantity,
            self.order_type,
        )?;
        insert_order_optionals(
            &mut payload,
            self.limit_price.as_ref(),
            self.stop_price.as_ref(),
            self.time_in_force,
            self.expire_time,
            self.text.as_deref(),
            self.activation_time,
            self.custom_tag_50.as_deref(),
        );
        payload.insert("isAutomated".to_owned(), Value::Bool(self.is_automated));

        Ok(Value::Object(payload))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradovateBracketOrder {
    pub side: TradeSide,
    pub quantity: Option<u32>,
    pub order_type: TradovateOrderType,
    pub limit_price: Option<Decimal>,
    pub stop_price: Option<Decimal>,
    pub time_in_force: Option<TradovateTimeInForce>,
    pub expire_time: Option<DateTime<Utc>>,
    pub text: Option<String>,
    pub activation_time: Option<DateTime<Utc>>,
    pub custom_tag_50: Option<String>,
}

impl TradovateBracketOrder {
    fn validate(&self, parent_quantity: u32) -> Result<(), TradovateError> {
        validate_quantity(self.quantity.unwrap_or(parent_quantity))?;
        validate_price_requirements(
            self.order_type,
            self.limit_price.as_ref(),
            self.stop_price.as_ref(),
        )?;
        Ok(())
    }

    fn to_payload(&self, parent_quantity: u32) -> Result<Value, TradovateError> {
        self.validate(parent_quantity)?;
        let mut payload = Map::new();
        payload.insert(
            "action".to_owned(),
            Value::String(trade_side_to_api(self.side).to_owned()),
        );
        payload.insert(
            "orderType".to_owned(),
            Value::String(self.order_type.as_api_str().to_owned()),
        );

        if let Some(quantity) = self.quantity {
            payload.insert("orderQty".to_owned(), Value::Number(Number::from(quantity)));
        }

        insert_order_optionals(
            &mut payload,
            self.limit_price.as_ref(),
            self.stop_price.as_ref(),
            self.time_in_force,
            self.expire_time,
            self.text.as_deref(),
            self.activation_time,
            self.custom_tag_50.as_deref(),
        );

        Ok(Value::Object(payload))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradovateOsoOrderPlacement {
    pub symbol: String,
    pub side: TradeSide,
    pub quantity: u32,
    pub order_type: TradovateOrderType,
    pub limit_price: Option<Decimal>,
    pub stop_price: Option<Decimal>,
    pub time_in_force: Option<TradovateTimeInForce>,
    pub expire_time: Option<DateTime<Utc>>,
    pub text: Option<String>,
    pub activation_time: Option<DateTime<Utc>>,
    pub custom_tag_50: Option<String>,
    pub is_automated: bool,
    pub brackets: Vec<TradovateBracketOrder>,
}

impl TradovateOsoOrderPlacement {
    fn validate(&self) -> Result<(), TradovateError> {
        validate_symbol(&self.symbol)?;
        validate_quantity(self.quantity)?;
        validate_price_requirements(
            self.order_type,
            self.limit_price.as_ref(),
            self.stop_price.as_ref(),
        )?;

        if self.brackets.is_empty() || self.brackets.len() > 2 {
            return Err(TradovateError::InvalidExecutionRequest {
                message: "Tradovate OSO requests require one or two bracket orders".to_owned(),
            });
        }

        for bracket in &self.brackets {
            bracket.validate(self.quantity)?;
        }

        Ok(())
    }

    fn to_payload(&self, context: &TradovateExecutionContext) -> Result<Value, TradovateError> {
        self.validate()?;

        let mut payload = base_order_payload(
            context,
            self.side,
            &self.symbol,
            self.quantity,
            self.order_type,
        )?;
        insert_order_optionals(
            &mut payload,
            self.limit_price.as_ref(),
            self.stop_price.as_ref(),
            self.time_in_force,
            self.expire_time,
            self.text.as_deref(),
            self.activation_time,
            self.custom_tag_50.as_deref(),
        );
        payload.insert("isAutomated".to_owned(), Value::Bool(self.is_automated));
        payload.insert(
            "bracket1".to_owned(),
            self.brackets[0].to_payload(self.quantity)?,
        );

        if let Some(bracket) = self.brackets.get(1) {
            payload.insert("bracket2".to_owned(), bracket.to_payload(self.quantity)?);
        }

        Ok(Value::Object(payload))
    }
}

#[derive(Clone, Debug)]
pub struct TradovatePlaceOrderRequest {
    pub context: TradovateExecutionContext,
    pub order: TradovateOrderPlacement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovatePlaceOrderResult {
    pub order_id: i64,
}

#[derive(Clone, Debug)]
pub struct TradovatePlaceOsoRequest {
    pub context: TradovateExecutionContext,
    pub order: TradovateOsoOrderPlacement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovatePlaceOsoResult {
    pub order_id: i64,
    pub oso1_id: Option<i64>,
    pub oso2_id: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct TradovateLiquidatePositionRequest {
    pub context: TradovateExecutionContext,
    pub contract_id: i64,
    pub custom_tag_50: Option<String>,
    pub admin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovateLiquidatePositionResult {
    pub order_id: i64,
}

#[derive(Clone, Debug)]
pub struct TradovateCancelOrderRequest {
    pub context: TradovateExecutionContext,
    pub order_id: i64,
    pub is_automated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradovateCancelOrderResult {
    pub order_id: i64,
}

#[async_trait]
pub trait TradovateExecutionApi: Send + Sync {
    async fn place_order(
        &self,
        request: TradovatePlaceOrderRequest,
    ) -> Result<TradovatePlaceOrderResult, TradovateError>;

    async fn place_oso(
        &self,
        request: TradovatePlaceOsoRequest,
    ) -> Result<TradovatePlaceOsoResult, TradovateError>;

    async fn liquidate_position(
        &self,
        request: TradovateLiquidatePositionRequest,
    ) -> Result<TradovateLiquidatePositionResult, TradovateError>;

    async fn cancel_order(
        &self,
        request: TradovateCancelOrderRequest,
    ) -> Result<TradovateCancelOrderResult, TradovateError>;
}

#[async_trait]
impl TradovateExecutionApi for TradovateLiveClient {
    async fn place_order(
        &self,
        request: TradovatePlaceOrderRequest,
    ) -> Result<TradovatePlaceOrderResult, TradovateError> {
        let payload = request.order.to_payload(&request.context)?;
        let response = self
            .post_execution("order/placeorder", &request.context, payload)
            .await?;

        let result = response.into_place_order_result()?;
        info!(order_id = result.order_id, "Tradovate placeorder accepted");
        Ok(result)
    }

    async fn place_oso(
        &self,
        request: TradovatePlaceOsoRequest,
    ) -> Result<TradovatePlaceOsoResult, TradovateError> {
        let payload = request.order.to_payload(&request.context)?;
        let response = self
            .post_execution("order/placeoso", &request.context, payload)
            .await?;

        let result = response.into_place_oso_result()?;
        info!(
            order_id = result.order_id,
            oso1_id = ?result.oso1_id,
            oso2_id = ?result.oso2_id,
            "Tradovate placeoso accepted"
        );
        Ok(result)
    }

    async fn liquidate_position(
        &self,
        request: TradovateLiquidatePositionRequest,
    ) -> Result<TradovateLiquidatePositionResult, TradovateError> {
        if request.contract_id <= 0 {
            return Err(TradovateError::InvalidExecutionRequest {
                message: "Tradovate liquidation requires a positive contract id".to_owned(),
            });
        }

        let mut payload = Map::new();
        payload.insert(
            "accountSpec".to_owned(),
            Value::String(request.context.account_spec.clone()),
        );
        payload.insert(
            "accountId".to_owned(),
            Value::Number(Number::from(request.context.account_id)),
        );
        payload.insert(
            "contractId".to_owned(),
            Value::Number(Number::from(request.contract_id)),
        );
        payload.insert("admin".to_owned(), Value::Bool(request.admin));

        if let Some(tag) = request.custom_tag_50.as_deref() {
            payload.insert("customTag50".to_owned(), Value::String(tag.to_owned()));
        }

        let response = self
            .post_execution(
                "order/liquidateposition",
                &request.context,
                Value::Object(payload),
            )
            .await?;

        let result = response.into_liquidate_position_result()?;
        info!(order_id = result.order_id, "Tradovate liquidation accepted");
        Ok(result)
    }

    async fn cancel_order(
        &self,
        request: TradovateCancelOrderRequest,
    ) -> Result<TradovateCancelOrderResult, TradovateError> {
        if request.order_id <= 0 {
            return Err(TradovateError::InvalidExecutionRequest {
                message: "Tradovate order cancellation requires a positive order id".to_owned(),
            });
        }

        let mut payload = Map::new();
        payload.insert(
            "orderId".to_owned(),
            Value::Number(Number::from(request.order_id)),
        );
        payload.insert("isAutomated".to_owned(), Value::Bool(request.is_automated));

        let response = self
            .post_execution(
                "order/cancelorder",
                &request.context,
                Value::Object(payload),
            )
            .await?;

        let result = response.into_cancel_order_result(request.order_id)?;
        info!(
            order_id = result.order_id,
            "Tradovate cancellation accepted"
        );
        Ok(result)
    }
}

#[derive(Clone, Debug, Default)]
struct ExecutionResponse {
    failure_reason: Option<String>,
    failure_text: Option<String>,
    order_id: Option<i64>,
    oso1_id: Option<i64>,
    oso2_id: Option<i64>,
}

impl ExecutionResponse {
    fn into_place_order_result(self) -> Result<TradovatePlaceOrderResult, TradovateError> {
        self.ensure_success()?;
        Ok(TradovatePlaceOrderResult {
            order_id: self
                .order_id
                .ok_or_else(|| TradovateError::ExecutionTransport {
                    message: "Tradovate placeorder response did not include an order id".to_owned(),
                })?,
        })
    }

    fn into_place_oso_result(self) -> Result<TradovatePlaceOsoResult, TradovateError> {
        self.ensure_success()?;
        Ok(TradovatePlaceOsoResult {
            order_id: self
                .order_id
                .ok_or_else(|| TradovateError::ExecutionTransport {
                    message: "Tradovate placeoso response did not include an order id".to_owned(),
                })?,
            oso1_id: self.oso1_id,
            oso2_id: self.oso2_id,
        })
    }

    fn into_liquidate_position_result(
        self,
    ) -> Result<TradovateLiquidatePositionResult, TradovateError> {
        self.ensure_success()?;
        Ok(TradovateLiquidatePositionResult {
            order_id: self
                .order_id
                .ok_or_else(|| TradovateError::ExecutionTransport {
                    message: "Tradovate liquidateposition response did not include an order id"
                        .to_owned(),
                })?,
        })
    }

    fn into_cancel_order_result(
        self,
        requested_order_id: i64,
    ) -> Result<TradovateCancelOrderResult, TradovateError> {
        self.ensure_success()?;
        Ok(TradovateCancelOrderResult {
            order_id: self.order_id.unwrap_or(requested_order_id),
        })
    }

    fn ensure_success(&self) -> Result<(), TradovateError> {
        let Some(reason) = self.failure_reason.as_deref() else {
            return Ok(());
        };

        if reason.eq_ignore_ascii_case("success") {
            return Ok(());
        }

        Err(TradovateError::ExecutionRejected {
            reason: reason.to_owned(),
            message: self
                .failure_text
                .clone()
                .unwrap_or_else(|| "Tradovate rejected the request".to_owned()),
        })
    }
}

fn validate_symbol(symbol: &str) -> Result<(), TradovateError> {
    if symbol.trim().is_empty() {
        return Err(TradovateError::InvalidExecutionRequest {
            message: "Tradovate order requests require a symbol".to_owned(),
        });
    }

    Ok(())
}

fn validate_quantity(quantity: u32) -> Result<(), TradovateError> {
    if quantity == 0 {
        return Err(TradovateError::InvalidExecutionRequest {
            message: "Tradovate order quantity must be greater than zero".to_owned(),
        });
    }

    Ok(())
}

fn validate_price_requirements(
    order_type: TradovateOrderType,
    limit_price: Option<&Decimal>,
    stop_price: Option<&Decimal>,
) -> Result<(), TradovateError> {
    let needs_limit_price = matches!(
        order_type,
        TradovateOrderType::Limit | TradovateOrderType::StopLimit
    );
    let needs_stop_price = matches!(
        order_type,
        TradovateOrderType::Stop | TradovateOrderType::StopLimit
    );

    if needs_limit_price && limit_price.is_none() {
        return Err(TradovateError::InvalidExecutionRequest {
            message: format!("Tradovate {:?} orders require a limit price", order_type),
        });
    }

    if needs_stop_price && stop_price.is_none() {
        return Err(TradovateError::InvalidExecutionRequest {
            message: format!("Tradovate {:?} orders require a stop price", order_type),
        });
    }

    Ok(())
}

fn base_order_payload(
    context: &TradovateExecutionContext,
    side: TradeSide,
    symbol: &str,
    quantity: u32,
    order_type: TradovateOrderType,
) -> Result<Map<String, Value>, TradovateError> {
    if context.http_base_url.trim().is_empty() {
        return Err(TradovateError::InvalidExecutionRequest {
            message: "Tradovate execution context requires an http base url".to_owned(),
        });
    }

    if context.account_id <= 0 {
        return Err(TradovateError::InvalidExecutionRequest {
            message: "Tradovate execution context requires a positive account id".to_owned(),
        });
    }

    if context.account_spec.trim().is_empty() {
        return Err(TradovateError::InvalidExecutionRequest {
            message: "Tradovate execution context requires an account spec".to_owned(),
        });
    }

    let mut payload = Map::new();
    payload.insert(
        "accountSpec".to_owned(),
        Value::String(context.account_spec.clone()),
    );
    payload.insert(
        "accountId".to_owned(),
        Value::Number(Number::from(context.account_id)),
    );
    payload.insert("symbol".to_owned(), Value::String(symbol.trim().to_owned()));
    payload.insert(
        "action".to_owned(),
        Value::String(trade_side_to_api(side).to_owned()),
    );
    payload.insert("orderQty".to_owned(), Value::Number(Number::from(quantity)));
    payload.insert(
        "orderType".to_owned(),
        Value::String(order_type.as_api_str().to_owned()),
    );
    Ok(payload)
}

fn insert_order_optionals(
    payload: &mut Map<String, Value>,
    limit_price: Option<&Decimal>,
    stop_price: Option<&Decimal>,
    time_in_force: Option<TradovateTimeInForce>,
    expire_time: Option<DateTime<Utc>>,
    text: Option<&str>,
    activation_time: Option<DateTime<Utc>>,
    custom_tag_50: Option<&str>,
) {
    if let Some(price) = limit_price {
        payload.insert("price".to_owned(), decimal_to_json(price));
    }

    if let Some(stop_price) = stop_price {
        payload.insert("stopPrice".to_owned(), decimal_to_json(stop_price));
    }

    if let Some(time_in_force) = time_in_force {
        payload.insert(
            "timeInForce".to_owned(),
            Value::String(time_in_force.as_api_str().to_owned()),
        );
    }

    if let Some(expire_time) = expire_time {
        payload.insert(
            "expireTime".to_owned(),
            Value::String(expire_time.to_rfc3339()),
        );
    }

    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        payload.insert("text".to_owned(), Value::String(text.to_owned()));
    }

    if let Some(activation_time) = activation_time {
        payload.insert(
            "activationTime".to_owned(),
            Value::String(activation_time.to_rfc3339()),
        );
    }

    if let Some(tag) = custom_tag_50.filter(|tag| !tag.trim().is_empty()) {
        payload.insert("customTag50".to_owned(), Value::String(tag.to_owned()));
    }
}

fn trade_side_to_api(side: TradeSide) -> &'static str {
    match side {
        TradeSide::Buy => "Buy",
        TradeSide::Sell => "Sell",
    }
}

fn decimal_to_json(value: &Decimal) -> Value {
    Number::from_str(&value.normalize().to_string())
        .map(Value::Number)
        .unwrap_or_else(|_| Value::String(value.to_string()))
}

impl TradovateLiveClient {
    async fn post_execution(
        &self,
        path: &str,
        context: &TradovateExecutionContext,
        payload: Value,
    ) -> Result<ExecutionResponse, TradovateError> {
        let response = self
            .http_client
            .post(super::live::build_http_url(&context.http_base_url, path))
            .bearer_auth(context.access_token.access_token.expose_secret())
            .json(&payload)
            .send()
            .await
            .map_err(|error| TradovateError::ExecutionTransport {
                message: error.to_string(),
            })?;

        let response =
            response
                .error_for_status()
                .map_err(|error| TradovateError::ExecutionTransport {
                    message: error.to_string(),
                })?;

        let value =
            response
                .json::<Value>()
                .await
                .map_err(|error| TradovateError::ExecutionTransport {
                    message: error.to_string(),
                })?;

        parse_execution_response(value)
    }
}

fn parse_execution_response(value: Value) -> Result<ExecutionResponse, TradovateError> {
    let object = value
        .as_object()
        .ok_or_else(|| TradovateError::ExecutionTransport {
            message: "Tradovate execution response must be a JSON object".to_owned(),
        })?;

    Ok(ExecutionResponse {
        failure_reason: object
            .get("failureReason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        failure_text: object
            .get("failureText")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        order_id: object.get("orderId").and_then(value_to_i64),
        oso1_id: object.get("oso1Id").and_then(value_to_i64),
        oso2_id: object.get("oso2Id").and_then(value_to_i64),
    })
}

fn value_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse::<i64>().ok(),
        _ => None,
    }
}
