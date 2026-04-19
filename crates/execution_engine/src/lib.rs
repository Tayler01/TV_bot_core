//! Strategy-agnostic execution planning for broker-native order flows.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tv_bot_broker_tradovate::{
    Clock as TradovateClock, TradovateAccountApi, TradovateAuthApi, TradovateBracketOrder,
    TradovateError, TradovateExecutionApi, TradovateOrderPlacement, TradovateOrderType,
    TradovateOsoOrderPlacement, TradovateSessionManager, TradovateSyncApi, TradovateTimeInForce,
};
use tv_bot_core_types::{
    BrokerOrderUpdate, BrokerPositionSnapshot, BrokerPreference, CompiledStrategy, EntryOrderType,
    ExecutionIntent, ReversalMode, TradeSide,
};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionInstrumentContext {
    pub tradovate_symbol: String,
    pub tick_size: Decimal,
    pub entry_reference_price: Option<Decimal>,
    pub active_contract_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStateContext {
    pub runtime_can_submit_orders: bool,
    pub new_entries_allowed: bool,
    pub current_position: Option<BrokerPositionSnapshot>,
    pub working_orders: Vec<BrokerOrderUpdate>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequest {
    pub strategy: CompiledStrategy,
    pub instrument: ExecutionInstrumentContext,
    pub state: ExecutionStateContext,
    pub intent: ExecutionIntent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionPlan {
    pub actions: Vec<ExecutionAction>,
    pub warnings: Vec<String>,
}

impl ExecutionPlan {
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionDispatchReport {
    pub results: Vec<ExecutionDispatchResult>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionDispatchResult {
    OrderSubmitted {
        order_id: i64,
        symbol: String,
        used_brackets: bool,
    },
    OrderCancelled {
        order_id: i64,
        symbol: String,
        reason: String,
    },
    PositionLiquidated {
        order_id: i64,
        contract_id: i64,
        reason: String,
    },
    StrategyPaused {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionAction {
    SubmitOrder(TradovateOrderPlacement),
    SubmitOsoOrder(TradovateOsoOrderPlacement),
    CancelOrder {
        order_id: i64,
        symbol: String,
        reason: String,
    },
    LiquidatePosition {
        contract_id: i64,
        reason: String,
        custom_tag_50: Option<String>,
    },
    PauseStrategy {
        reason: String,
    },
}

pub struct ExecutionPlanner;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecutionEngineError {
    #[error("order placement is not permitted until the runtime is armed and ready")]
    OrderPlacementBlocked,
    #[error("new entries are blocked because execution dependencies are degraded")]
    NewEntriesBlocked,
    #[error("execution instrument tick size must be greater than zero")]
    InvalidTickSize,
    #[error("entry quantity must be greater than zero")]
    InvalidEntryQuantity,
    #[error("reduce quantity must be greater than zero")]
    InvalidReductionQuantity,
    #[error("limit or stop entry requires an entry reference price")]
    MissingEntryReferencePrice,
    #[error("broker-side protective brackets require an entry reference price")]
    MissingProtectiveReferencePrice,
    #[error("current broker position is required for `{intent}`")]
    MissingOpenPosition { intent: &'static str },
    #[error("current broker contract id is required for `{intent}`")]
    MissingContractId { intent: &'static str },
    #[error("no working broker orders are active for symbol `{symbol}`")]
    MissingWorkingOrders { symbol: String },
    #[error("working broker order id `{broker_order_id}` is not a valid Tradovate order id")]
    InvalidWorkingOrderId { broker_order_id: String },
    #[error("entry order type `{order_type:?}` is not supported by the current execution planner")]
    UnsupportedEntryOrderType { order_type: EntryOrderType },
    #[error("same-side scale-in is disabled by the loaded strategy")]
    ScaleInDisabled,
    #[error("same-side scale-in would exceed the configured maximum legs")]
    ScaleInMaxLegsReached,
    #[error("intent `{intent}` is not supported by the current execution planner")]
    UnsupportedIntent { intent: &'static str },
    #[error("broker-required protection `{feature}` cannot be satisfied by the current execution planner")]
    UnsupportedBrokerRequiredFeature { feature: &'static str },
    #[error("broker-required protective brackets are missing required stop/take-profit settings")]
    MissingRequiredProtectiveBracket,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecutionDispatchError {
    #[error("execution planning failed: {source}")]
    Planning { source: ExecutionEngineError },
    #[error("execution action `{action}` failed: {source}")]
    Broker {
        action: &'static str,
        source: TradovateError,
    },
}

impl ExecutionPlanner {
    pub fn plan_tradovate(
        request: &ExecutionRequest,
    ) -> Result<ExecutionPlan, ExecutionEngineError> {
        validate_instrument(&request.instrument)?;

        match &request.intent {
            ExecutionIntent::Enter {
                side,
                order_type,
                quantity,
                protective_brackets_expected,
                reason,
            } => Self::plan_entry(
                request,
                *side,
                *order_type,
                *quantity,
                *protective_brackets_expected,
                reason,
            ),
            ExecutionIntent::Exit { reason } | ExecutionIntent::Flatten { reason } => {
                Self::plan_flatten(request, reason, "flatten")
            }
            ExecutionIntent::ReducePosition { quantity, reason } => {
                Self::plan_reduce_position(request, *quantity, reason)
            }
            ExecutionIntent::PauseStrategy { reason } => Ok(ExecutionPlan {
                actions: vec![ExecutionAction::PauseStrategy {
                    reason: reason.clone(),
                }],
                warnings: Vec::new(),
            }),
            ExecutionIntent::CancelWorkingOrders { reason } => {
                Self::plan_cancel_working_orders(request, reason)
            }
        }
    }

    fn plan_cancel_working_orders(
        request: &ExecutionRequest,
        reason: &str,
    ) -> Result<ExecutionPlan, ExecutionEngineError> {
        let working_orders = request
            .state
            .working_orders
            .iter()
            .filter(|order| {
                order
                    .symbol
                    .eq_ignore_ascii_case(&request.instrument.tradovate_symbol)
            })
            .filter(|order| matches!(order.status, tv_bot_core_types::BrokerOrderStatus::Working))
            .map(|order| {
                let order_id = order.broker_order_id.parse::<i64>().map_err(|_| {
                    ExecutionEngineError::InvalidWorkingOrderId {
                        broker_order_id: order.broker_order_id.clone(),
                    }
                })?;
                Ok(ExecutionAction::CancelOrder {
                    order_id,
                    symbol: order.symbol.clone(),
                    reason: reason.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, ExecutionEngineError>>()?;

        if working_orders.is_empty() {
            return Err(ExecutionEngineError::MissingWorkingOrders {
                symbol: request.instrument.tradovate_symbol.clone(),
            });
        }

        Ok(ExecutionPlan {
            actions: working_orders,
            warnings: Vec::new(),
        })
    }

    fn plan_entry(
        request: &ExecutionRequest,
        side: TradeSide,
        order_type: EntryOrderType,
        quantity: u32,
        protective_brackets_expected: bool,
        reason: &str,
    ) -> Result<ExecutionPlan, ExecutionEngineError> {
        ensure_order_placement_allowed(request)?;

        if !request.state.new_entries_allowed {
            return Err(ExecutionEngineError::NewEntriesBlocked);
        }

        if quantity == 0 {
            return Err(ExecutionEngineError::InvalidEntryQuantity);
        }

        let mut warnings = Vec::new();
        assess_trade_management_support(&request.strategy, &mut warnings)?;
        ensure_required_protection_configuration(&request.strategy, protective_brackets_expected)?;

        let mut actions = Vec::new();
        let active_position = active_position(request.state.current_position.as_ref());
        let mut effective_quantity = quantity;

        if let Some(position) = active_position {
            let position_side = side_for_position(position.quantity);

            if position_side == side {
                if !request.strategy.execution.scaling.allow_scale_in {
                    return Err(ExecutionEngineError::ScaleInDisabled);
                }

                // Execution planning only has the live broker position snapshot, so use the
                // open same-side quantity as the conservative proxy for how many scaling units
                // are already active until richer leg tracking is threaded into this path.
                let active_scale_units = position.quantity.unsigned_abs();
                if active_scale_units >= request.strategy.execution.scaling.max_legs {
                    return Err(ExecutionEngineError::ScaleInMaxLegsReached);
                }
            } else {
                match request.strategy.execution.reversal_mode {
                    ReversalMode::FlattenFirst => {
                        let contract_id = resolve_contract_id(
                            &request.instrument,
                            request.state.current_position.as_ref(),
                        )
                        .ok_or(
                            ExecutionEngineError::MissingContractId {
                                intent: "flatten_first_reversal",
                            },
                        )?;

                        actions.push(ExecutionAction::LiquidatePosition {
                            contract_id,
                            reason: format!("flatten-first reversal: {reason}"),
                            custom_tag_50: strategy_tag(&request.strategy.metadata.strategy_id),
                        });
                    }
                    ReversalMode::DirectReverse => {
                        effective_quantity =
                            effective_quantity.saturating_add(position.quantity.unsigned_abs());
                    }
                }
            }
        }

        let entry_action = if protective_brackets_expected {
            ExecutionAction::SubmitOsoOrder(build_entry_oso(
                &request.strategy,
                &request.instrument,
                side,
                order_type,
                effective_quantity,
                reason,
            )?)
        } else {
            ExecutionAction::SubmitOrder(build_entry_order(
                &request.strategy,
                &request.instrument,
                side,
                order_type,
                effective_quantity,
                reason,
            )?)
        };

        actions.push(entry_action);

        Ok(ExecutionPlan { actions, warnings })
    }
    fn plan_flatten(
        request: &ExecutionRequest,
        reason: &str,
        intent_label: &'static str,
    ) -> Result<ExecutionPlan, ExecutionEngineError> {
        if active_position(request.state.current_position.as_ref()).is_none() {
            return Ok(ExecutionPlan {
                actions: Vec::new(),
                warnings: vec!["flatten requested while no broker position is open".to_owned()],
            });
        }

        let contract_id =
            resolve_contract_id(&request.instrument, request.state.current_position.as_ref())
                .ok_or(ExecutionEngineError::MissingContractId {
                    intent: intent_label,
                })?;

        Ok(ExecutionPlan {
            actions: vec![ExecutionAction::LiquidatePosition {
                contract_id,
                reason: reason.to_owned(),
                custom_tag_50: strategy_tag(&request.strategy.metadata.strategy_id),
            }],
            warnings: Vec::new(),
        })
    }

    fn plan_reduce_position(
        request: &ExecutionRequest,
        quantity: u32,
        reason: &str,
    ) -> Result<ExecutionPlan, ExecutionEngineError> {
        ensure_order_placement_allowed(request)?;

        if quantity == 0 {
            return Err(ExecutionEngineError::InvalidReductionQuantity);
        }

        let position = active_position(request.state.current_position.as_ref()).ok_or(
            ExecutionEngineError::MissingOpenPosition {
                intent: "reduce_position",
            },
        )?;

        if quantity >= position.quantity.unsigned_abs() {
            return Self::plan_flatten(request, reason, "reduce_position");
        }

        Ok(ExecutionPlan {
            actions: vec![ExecutionAction::SubmitOrder(TradovateOrderPlacement {
                symbol: request.instrument.tradovate_symbol.clone(),
                side: opposite_side(side_for_position(position.quantity)),
                quantity,
                order_type: TradovateOrderType::Market,
                limit_price: None,
                stop_price: None,
                time_in_force: Some(TradovateTimeInForce::Day),
                expire_time: None,
                text: order_text(reason),
                activation_time: None,
                custom_tag_50: strategy_tag(&request.strategy.metadata.strategy_id),
                is_automated: true,
            })],
            warnings: Vec::new(),
        })
    }
}

pub async fn execute_tradovate_plan<A, B, C, Clk, E>(
    plan: ExecutionPlan,
    session: &mut TradovateSessionManager<A, B, C, Clk>,
    execution_api: &E,
) -> Result<ExecutionDispatchReport, ExecutionDispatchError>
where
    A: TradovateAuthApi,
    B: TradovateAccountApi,
    C: TradovateSyncApi,
    Clk: TradovateClock,
    E: TradovateExecutionApi,
{
    let mut results = Vec::new();

    for action in plan.actions {
        match action {
            ExecutionAction::SubmitOrder(order) => {
                let symbol = order.symbol.clone();
                let result = session
                    .place_order(execution_api, order)
                    .await
                    .map_err(|source| ExecutionDispatchError::Broker {
                        action: "place_order",
                        source,
                    })?;

                results.push(ExecutionDispatchResult::OrderSubmitted {
                    order_id: result.order_id,
                    symbol,
                    used_brackets: false,
                });
            }
            ExecutionAction::SubmitOsoOrder(order) => {
                let symbol = order.symbol.clone();
                let result = session
                    .place_oso(execution_api, order)
                    .await
                    .map_err(|source| ExecutionDispatchError::Broker {
                        action: "place_oso",
                        source,
                    })?;

                results.push(ExecutionDispatchResult::OrderSubmitted {
                    order_id: result.order_id,
                    symbol,
                    used_brackets: true,
                });
            }
            ExecutionAction::CancelOrder {
                order_id,
                symbol,
                reason,
            } => {
                let result = session
                    .cancel_order(execution_api, order_id)
                    .await
                    .map_err(|source| ExecutionDispatchError::Broker {
                        action: "cancel_order",
                        source,
                    })?;

                results.push(ExecutionDispatchResult::OrderCancelled {
                    order_id: result.order_id,
                    symbol,
                    reason,
                });
            }
            ExecutionAction::LiquidatePosition {
                contract_id,
                reason,
                custom_tag_50,
            } => {
                let result = session
                    .liquidate_position(execution_api, contract_id, custom_tag_50)
                    .await
                    .map_err(|source| ExecutionDispatchError::Broker {
                        action: "liquidate_position",
                        source,
                    })?;

                results.push(ExecutionDispatchResult::PositionLiquidated {
                    order_id: result.order_id,
                    contract_id,
                    reason,
                });
            }
            ExecutionAction::PauseStrategy { reason } => {
                results.push(ExecutionDispatchResult::StrategyPaused { reason });
            }
        }
    }

    Ok(ExecutionDispatchReport {
        results,
        warnings: plan.warnings,
    })
}

pub async fn plan_and_execute_tradovate<A, B, C, Clk, E>(
    request: &ExecutionRequest,
    session: &mut TradovateSessionManager<A, B, C, Clk>,
    execution_api: &E,
) -> Result<ExecutionDispatchReport, ExecutionDispatchError>
where
    A: TradovateAuthApi,
    B: TradovateAccountApi,
    C: TradovateSyncApi,
    Clk: TradovateClock,
    E: TradovateExecutionApi,
{
    let plan = ExecutionPlanner::plan_tradovate(request)
        .map_err(|source| ExecutionDispatchError::Planning { source })?;

    execute_tradovate_plan(plan, session, execution_api).await
}

fn ensure_order_placement_allowed(request: &ExecutionRequest) -> Result<(), ExecutionEngineError> {
    if !request.state.runtime_can_submit_orders {
        return Err(ExecutionEngineError::OrderPlacementBlocked);
    }

    Ok(())
}

fn validate_instrument(
    instrument: &ExecutionInstrumentContext,
) -> Result<(), ExecutionEngineError> {
    if instrument.tick_size <= Decimal::ZERO {
        return Err(ExecutionEngineError::InvalidTickSize);
    }

    Ok(())
}

fn build_entry_order(
    strategy: &CompiledStrategy,
    instrument: &ExecutionInstrumentContext,
    side: TradeSide,
    order_type: EntryOrderType,
    quantity: u32,
    reason: &str,
) -> Result<TradovateOrderPlacement, ExecutionEngineError> {
    let (tradovate_order_type, limit_price, stop_price) =
        translate_entry_order(order_type, instrument.entry_reference_price)?;

    Ok(TradovateOrderPlacement {
        symbol: instrument.tradovate_symbol.clone(),
        side,
        quantity,
        order_type: tradovate_order_type,
        limit_price,
        stop_price,
        time_in_force: Some(TradovateTimeInForce::Day),
        expire_time: None,
        text: order_text(reason),
        activation_time: None,
        custom_tag_50: strategy_tag(&strategy.metadata.strategy_id),
        is_automated: true,
    })
}

fn build_entry_oso(
    strategy: &CompiledStrategy,
    instrument: &ExecutionInstrumentContext,
    side: TradeSide,
    order_type: EntryOrderType,
    quantity: u32,
    reason: &str,
) -> Result<TradovateOsoOrderPlacement, ExecutionEngineError> {
    let (tradovate_order_type, limit_price, stop_price) =
        translate_entry_order(order_type, instrument.entry_reference_price)?;
    let reference_price = instrument
        .entry_reference_price
        .ok_or(ExecutionEngineError::MissingProtectiveReferencePrice)?;
    let brackets = build_protective_brackets(
        strategy,
        side,
        quantity,
        instrument.tick_size,
        reference_price,
    )?;

    Ok(TradovateOsoOrderPlacement {
        symbol: instrument.tradovate_symbol.clone(),
        side,
        quantity,
        order_type: tradovate_order_type,
        limit_price,
        stop_price,
        time_in_force: Some(TradovateTimeInForce::Day),
        expire_time: None,
        text: order_text(reason),
        activation_time: None,
        custom_tag_50: strategy_tag(&strategy.metadata.strategy_id),
        is_automated: true,
        brackets,
    })
}

fn translate_entry_order(
    order_type: EntryOrderType,
    entry_reference_price: Option<Decimal>,
) -> Result<(TradovateOrderType, Option<Decimal>, Option<Decimal>), ExecutionEngineError> {
    match order_type {
        EntryOrderType::Market => Ok((TradovateOrderType::Market, None, None)),
        EntryOrderType::Limit => Ok((
            TradovateOrderType::Limit,
            Some(entry_reference_price.ok_or(ExecutionEngineError::MissingEntryReferencePrice)?),
            None,
        )),
        EntryOrderType::Stop => Ok((
            TradovateOrderType::Stop,
            None,
            Some(entry_reference_price.ok_or(ExecutionEngineError::MissingEntryReferencePrice)?),
        )),
        EntryOrderType::StopLimit => {
            Err(ExecutionEngineError::UnsupportedEntryOrderType { order_type })
        }
    }
}

fn build_protective_brackets(
    strategy: &CompiledStrategy,
    side: TradeSide,
    quantity: u32,
    tick_size: Decimal,
    reference_price: Decimal,
) -> Result<Vec<TradovateBracketOrder>, ExecutionEngineError> {
    let mut brackets = Vec::new();
    let exit_side = opposite_side(side);

    if strategy.trade_management.initial_stop_ticks > 0 {
        brackets.push(TradovateBracketOrder {
            side: exit_side,
            quantity: Some(quantity),
            order_type: TradovateOrderType::Stop,
            limit_price: None,
            stop_price: Some(offset_price(
                reference_price,
                tick_size,
                strategy.trade_management.initial_stop_ticks,
                opposite_side(side),
            )),
            time_in_force: Some(TradovateTimeInForce::Gtc),
            expire_time: None,
            text: Some("stop_loss".to_owned()),
            activation_time: None,
            custom_tag_50: None,
        });
    }

    if strategy.trade_management.take_profit_ticks > 0 {
        brackets.push(TradovateBracketOrder {
            side: exit_side,
            quantity: Some(quantity),
            order_type: TradovateOrderType::Limit,
            limit_price: Some(offset_price(
                reference_price,
                tick_size,
                strategy.trade_management.take_profit_ticks,
                side,
            )),
            stop_price: None,
            time_in_force: Some(TradovateTimeInForce::Gtc),
            expire_time: None,
            text: Some("take_profit".to_owned()),
            activation_time: None,
            custom_tag_50: None,
        });
    }

    if brackets.is_empty() {
        return Err(ExecutionEngineError::MissingRequiredProtectiveBracket);
    }

    Ok(brackets.into_iter().take(2).collect())
}

fn assess_trade_management_support(
    strategy: &CompiledStrategy,
    warnings: &mut Vec<String>,
) -> Result<(), ExecutionEngineError> {
    if strategy
        .trade_management
        .partial_take_profit
        .as_ref()
        .is_some_and(|rule| rule.enabled && !rule.targets.is_empty())
    {
        return Err(ExecutionEngineError::UnsupportedBrokerRequiredFeature {
            feature: "partial_take_profit",
        });
    }

    if strategy
        .trade_management
        .break_even
        .as_ref()
        .is_some_and(|rule| rule.enabled)
    {
        warnings.push(
            "break-even stop adjustments are not yet translated into broker-native modify flows"
                .to_owned(),
        );
    }

    if strategy
        .trade_management
        .trailing
        .as_ref()
        .is_some_and(|rule| rule.enabled)
    {
        if strategy.execution.broker_preferences.trailing_stop == BrokerPreference::BrokerRequired {
            return Err(ExecutionEngineError::UnsupportedBrokerRequiredFeature {
                feature: "trailing_stop",
            });
        }

        warnings.push(
            "trailing-stop management is not yet translated into broker-native execution flows"
                .to_owned(),
        );
    }

    Ok(())
}

fn ensure_required_protection_configuration(
    strategy: &CompiledStrategy,
    protective_brackets_expected: bool,
) -> Result<(), ExecutionEngineError> {
    let stop_required =
        strategy.execution.broker_preferences.stop_loss == BrokerPreference::BrokerRequired;
    let take_profit_required =
        strategy.execution.broker_preferences.take_profit == BrokerPreference::BrokerRequired;

    if (stop_required || take_profit_required) && !protective_brackets_expected {
        return Err(ExecutionEngineError::MissingRequiredProtectiveBracket);
    }

    if stop_required && strategy.trade_management.initial_stop_ticks == 0 {
        return Err(ExecutionEngineError::MissingRequiredProtectiveBracket);
    }

    if take_profit_required && strategy.trade_management.take_profit_ticks == 0 {
        return Err(ExecutionEngineError::MissingRequiredProtectiveBracket);
    }

    Ok(())
}

fn active_position(position: Option<&BrokerPositionSnapshot>) -> Option<&BrokerPositionSnapshot> {
    position.filter(|position| position.quantity != 0)
}

fn resolve_contract_id(
    instrument: &ExecutionInstrumentContext,
    current_position: Option<&BrokerPositionSnapshot>,
) -> Option<i64> {
    instrument.active_contract_id.or_else(|| {
        current_position.and_then(|position| {
            position
                .symbol
                .strip_prefix("contract:")
                .and_then(|contract_id| contract_id.parse::<i64>().ok())
        })
    })
}

fn side_for_position(quantity: i32) -> TradeSide {
    if quantity >= 0 {
        TradeSide::Buy
    } else {
        TradeSide::Sell
    }
}

fn opposite_side(side: TradeSide) -> TradeSide {
    match side {
        TradeSide::Buy => TradeSide::Sell,
        TradeSide::Sell => TradeSide::Buy,
    }
}

fn offset_price(
    reference_price: Decimal,
    tick_size: Decimal,
    ticks: u32,
    direction: TradeSide,
) -> Decimal {
    let offset = tick_size * Decimal::from(ticks);
    match direction {
        TradeSide::Buy => reference_price + offset,
        TradeSide::Sell => reference_price - offset,
    }
}

fn order_text(reason: &str) -> Option<String> {
    let trimmed = reason.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_tag(trimmed, 64))
    }
}

fn strategy_tag(strategy_id: &str) -> Option<String> {
    let trimmed = strategy_id.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_tag(trimmed, 64))
    }
}

fn truncate_tag(value: &str, max_len: usize) -> String {
    value.chars().take(max_len).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use secrecy::SecretString;
    use tv_bot_broker_tradovate::{
        TradovateAccessToken, TradovateAccount, TradovateAccountApi, TradovateAccountListRequest,
        TradovateAuthApi, TradovateAuthRequest, TradovateCancelOrderRequest,
        TradovateCancelOrderResult, TradovateCredentials, TradovateExecutionApi,
        TradovateLiquidatePositionRequest, TradovateLiquidatePositionResult,
        TradovatePlaceOrderRequest, TradovatePlaceOrderResult, TradovatePlaceOsoRequest,
        TradovatePlaceOsoResult, TradovateRoutingPreferences, TradovateSessionConfig,
        TradovateSessionManager, TradovateSyncApi, TradovateSyncConnectRequest, TradovateSyncEvent,
        TradovateSyncSnapshot, TradovateUserSyncRequest,
    };
    use tv_bot_core_types::{
        BrokerOrderUpdate, BrokerPreference, BrokerPreferences, DailyLossLimit, DashboardDisplay,
        DataFeedRequirement, DataRequirements, EntryRules, ExecutionSpec, ExitRules, FailsafeRules,
        FeedType, MarketConfig, MarketSelection, PartialTakeProfitRule, PositionSizing,
        PositionSizingMode, RiskLimits, ScalingConfig, SessionMode, SessionRules,
        SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata, Timeframe,
        TradeManagement, WarmupRequirements,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct FakeAuthApi {
        token: Arc<Mutex<Option<TradovateAccessToken>>>,
    }

    #[async_trait]
    impl TradovateAuthApi for FakeAuthApi {
        async fn request_access_token(
            &self,
            _request: TradovateAuthRequest,
        ) -> Result<TradovateAccessToken, TradovateError> {
            self.token
                .lock()
                .expect("auth mutex should not poison")
                .clone()
                .ok_or_else(|| TradovateError::AuthTransport {
                    message: "missing fake access token".to_owned(),
                })
        }

        async fn renew_access_token(
            &self,
            _request: tv_bot_broker_tradovate::TradovateRenewAccessTokenRequest,
        ) -> Result<TradovateAccessToken, TradovateError> {
            self.request_access_token(TradovateAuthRequest {
                http_base_url: String::new(),
                environment: tv_bot_core_types::BrokerEnvironment::Demo,
                credentials: sample_credentials(),
            })
            .await
        }
    }

    #[derive(Clone)]
    struct FakeAccountApi {
        accounts: Arc<Vec<TradovateAccount>>,
    }

    #[async_trait]
    impl TradovateAccountApi for FakeAccountApi {
        async fn list_accounts(
            &self,
            _request: TradovateAccountListRequest,
        ) -> Result<Vec<TradovateAccount>, TradovateError> {
            Ok(self.accounts.as_ref().clone())
        }
    }

    #[derive(Clone, Default)]
    struct FakeSyncApi {
        snapshots: Arc<Mutex<VecDeque<TradovateSyncSnapshot>>>,
    }

    #[async_trait]
    impl TradovateSyncApi for FakeSyncApi {
        async fn connect(
            &self,
            _request: TradovateSyncConnectRequest,
        ) -> Result<(), TradovateError> {
            Ok(())
        }

        async fn request_user_sync(
            &self,
            _request: TradovateUserSyncRequest,
        ) -> Result<TradovateSyncSnapshot, TradovateError> {
            self.snapshots
                .lock()
                .expect("sync mutex should not poison")
                .pop_front()
                .ok_or_else(|| TradovateError::SyncTransport {
                    message: "missing fake sync snapshot".to_owned(),
                })
        }

        async fn next_event(&self) -> Result<Option<TradovateSyncEvent>, TradovateError> {
            Ok(None)
        }

        async fn disconnect(&self) -> Result<(), TradovateError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeExecutionApi {
        place_orders: Arc<Mutex<Vec<TradovatePlaceOrderRequest>>>,
        place_osos: Arc<Mutex<Vec<TradovatePlaceOsoRequest>>>,
        liquidations: Arc<Mutex<Vec<TradovateLiquidatePositionRequest>>>,
        cancel_orders: Arc<Mutex<Vec<TradovateCancelOrderRequest>>>,
    }

    #[async_trait]
    impl TradovateExecutionApi for FakeExecutionApi {
        async fn place_order(
            &self,
            request: TradovatePlaceOrderRequest,
        ) -> Result<TradovatePlaceOrderResult, TradovateError> {
            self.place_orders
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovatePlaceOrderResult { order_id: 7001 })
        }

        async fn place_oso(
            &self,
            request: TradovatePlaceOsoRequest,
        ) -> Result<TradovatePlaceOsoResult, TradovateError> {
            self.place_osos
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovatePlaceOsoResult {
                order_id: 7002,
                oso1_id: Some(7101),
                oso2_id: Some(7102),
            })
        }

        async fn liquidate_position(
            &self,
            request: TradovateLiquidatePositionRequest,
        ) -> Result<TradovateLiquidatePositionResult, TradovateError> {
            self.liquidations
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovateLiquidatePositionResult { order_id: 7003 })
        }

        async fn cancel_order(
            &self,
            request: TradovateCancelOrderRequest,
        ) -> Result<TradovateCancelOrderResult, TradovateError> {
            self.cancel_orders
                .lock()
                .expect("execution mutex should not poison")
                .push(request.clone());
            Ok(TradovateCancelOrderResult {
                order_id: request.order_id,
            })
        }
    }

    fn sample_strategy(
        reversal_mode: ReversalMode,
        allow_scale_in: bool,
        max_legs: u32,
        stop_pref: BrokerPreference,
        take_profit_pref: BrokerPreference,
        trailing_pref: BrokerPreference,
    ) -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "gc_momentum_v1".to_owned(),
                name: "GC Momentum".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "execution tests".to_owned(),
                tags: Vec::new(),
                source: None,
                notes: None,
            },
            market: MarketConfig {
                market: "gold".to_owned(),
                selection: MarketSelection {
                    contract_mode: tv_bot_core_types::ContractMode::FrontMonthAuto,
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
                feeds: vec![DataFeedRequirement {
                    kind: FeedType::Trades,
                }],
                timeframes: vec![Timeframe::OneMinute],
                multi_timeframe: false,
                requires: None,
            },
            warmup: WarmupRequirements {
                bars_required: [(Timeframe::OneMinute, 10)].into_iter().collect(),
                ready_requires_all: true,
            },
            signal_confirmation: SignalConfirmation {
                mode: SignalCombinationMode::All,
                primary_conditions: vec!["trend".to_owned()],
                n_required: None,
                secondary_conditions: Vec::new(),
                score_threshold: None,
                regime_filter: None,
                sequence: Vec::new(),
            },
            entry_rules: EntryRules {
                long_enabled: true,
                short_enabled: true,
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
                reversal_mode,
                scaling: ScalingConfig {
                    allow_scale_in,
                    allow_scale_out: false,
                    max_legs,
                },
                broker_preferences: BrokerPreferences {
                    stop_loss: stop_pref,
                    take_profit: take_profit_pref,
                    trailing_stop: trailing_pref,
                },
            },
            trade_management: TradeManagement {
                initial_stop_ticks: 10,
                take_profit_ticks: 20,
                break_even: Some(tv_bot_core_types::BreakEvenRule {
                    enabled: true,
                    activate_at_ticks: Some(12),
                }),
                trailing: Some(tv_bot_core_types::TrailingRule {
                    enabled: trailing_pref != BrokerPreference::BotAllowed,
                    activate_at_ticks: Some(18),
                    trail_ticks: Some(6),
                }),
                partial_take_profit: Some(PartialTakeProfitRule {
                    enabled: false,
                    targets: Vec::new(),
                }),
                post_entry_rules: None,
                time_based_adjustments: None,
            },
            risk: RiskLimits {
                daily_loss: DailyLossLimit {
                    broker_side_required: true,
                    local_backup_enabled: true,
                },
                max_trades_per_day: 3,
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
                cooldown_after_loss_s: 120,
                max_reentries_per_side: 1,
                regime_mode: None,
                memory_reset_rules: None,
                post_win_cooldown_s: None,
                failed_setup_decay: None,
                reentry_logic: None,
            },
            dashboard_display: DashboardDisplay {
                show: vec!["pnl".to_owned()],
                default_overlay: "entries".to_owned(),
                debug_panels: Vec::new(),
                custom_labels: None,
                preferred_chart_timeframe: None,
            },
        }
    }

    fn instrument_context() -> ExecutionInstrumentContext {
        ExecutionInstrumentContext {
            tradovate_symbol: "GCM2026".to_owned(),
            tick_size: Decimal::new(10, 1),
            entry_reference_price: Some(Decimal::new(238_510, 2)),
            active_contract_id: Some(4444),
        }
    }

    fn state_context() -> ExecutionStateContext {
        ExecutionStateContext {
            runtime_can_submit_orders: true,
            new_entries_allowed: true,
            current_position: None,
            working_orders: Vec::new(),
        }
    }

    fn sample_position(quantity: i32) -> BrokerPositionSnapshot {
        BrokerPositionSnapshot {
            account_id: Some("acct-paper".to_owned()),
            symbol: "GCM2026".to_owned(),
            quantity,
            average_price: Some(Decimal::new(238_500, 2)),
            realized_pnl: None,
            unrealized_pnl: None,
            protective_orders_present: true,
            captured_at: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
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

    fn sample_token() -> TradovateAccessToken {
        TradovateAccessToken {
            access_token: SecretString::new("access-token".to_owned().into()),
            expiration_time: DateTime::parse_from_rfc3339("2026-04-10T15:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            issued_at: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            user_id: Some(7),
            person_id: Some(11),
            market_data_access: Some("realtime".to_owned()),
        }
    }

    fn empty_sync_snapshot() -> TradovateSyncSnapshot {
        TradovateSyncSnapshot {
            occurred_at: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            positions: Vec::new(),
            working_orders: Vec::new(),
            fills: Vec::new(),
            account_snapshot: None,
            mismatch_reason: None,
            detail: "synced".to_owned(),
        }
    }

    async fn sample_session_manager(
    ) -> TradovateSessionManager<FakeAuthApi, FakeAccountApi, FakeSyncApi> {
        let auth_api = FakeAuthApi {
            token: Arc::new(Mutex::new(Some(sample_token()))),
        };
        let account_api = FakeAccountApi {
            accounts: Arc::new(vec![TradovateAccount {
                account_id: 101,
                account_name: "paper-primary".to_owned(),
                nickname: None,
                active: true,
            }]),
        };
        let sync_api = FakeSyncApi {
            snapshots: Arc::new(Mutex::new(VecDeque::from([empty_sync_snapshot()]))),
        };

        let mut manager = TradovateSessionManager::with_system_clock(
            TradovateSessionConfig::new(
                tv_bot_core_types::BrokerEnvironment::Demo,
                "https://demo.tradovateapi.com/v1",
                "wss://demo.tradovateapi.com/v1/websocket",
            )
            .expect("config should be valid"),
            sample_credentials(),
            TradovateRoutingPreferences {
                paper_account_name: Some("paper-primary".to_owned()),
                live_account_name: None,
            },
            auth_api,
            account_api,
            sync_api,
        )
        .expect("manager should build");

        manager.authenticate().await.expect("auth should succeed");
        manager
            .select_account_for_mode(&tv_bot_core_types::RuntimeMode::Paper)
            .await
            .expect("account selection should succeed");
        manager
            .connect_user_sync()
            .await
            .expect("sync should connect");

        manager
    }

    #[test]
    fn blocks_entry_when_runtime_is_not_armed_ready() {
        let mut state = state_context();
        state.runtime_can_submit_orders = false;

        let error = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BrokerPreferred,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: true,
                reason: "entry".to_owned(),
            },
        })
        .expect_err("unarmed runtime should block order placement");

        assert_eq!(error, ExecutionEngineError::OrderPlacementBlocked);
    }

    #[test]
    fn market_entry_with_required_brackets_becomes_oso_plan() {
        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state: state_context(),
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: true,
                reason: "momentum entry".to_owned(),
            },
        })
        .expect("market entry should plan successfully");

        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.warnings.len(), 1);

        match &plan.actions[0] {
            ExecutionAction::SubmitOsoOrder(order) => {
                assert_eq!(order.symbol, "GCM2026");
                assert_eq!(order.order_type, TradovateOrderType::Market);
                assert_eq!(order.brackets.len(), 2);
                assert_eq!(order.brackets[0].stop_price, Some(Decimal::new(237_510, 2)));
                assert_eq!(
                    order.brackets[1].limit_price,
                    Some(Decimal::new(240_510, 2))
                );
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn limit_entry_requires_reference_price_and_translates_to_limit_order() {
        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state: state_context(),
            intent: ExecutionIntent::Enter {
                side: TradeSide::Sell,
                order_type: EntryOrderType::Limit,
                quantity: 2,
                protective_brackets_expected: false,
                reason: "fade".to_owned(),
            },
        })
        .expect("limit entry should plan");

        match &plan.actions[0] {
            ExecutionAction::SubmitOrder(order) => {
                assert_eq!(order.order_type, TradovateOrderType::Limit);
                assert_eq!(order.limit_price, Some(Decimal::new(238_510, 2)));
                assert_eq!(order.side, TradeSide::Sell);
                assert_eq!(order.quantity, 2);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn flatten_first_reversal_liquidates_then_reenters() {
        let mut state = state_context();
        state.current_position = Some(sample_position(-1));

        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: true,
                reason: "reversal".to_owned(),
            },
        })
        .expect("flatten-first reversal should plan");

        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(
            &plan.actions[0],
            ExecutionAction::LiquidatePosition {
                contract_id: 4444,
                ..
            }
        ));
        assert!(matches!(
            &plan.actions[1],
            ExecutionAction::SubmitOsoOrder(_)
        ));
    }

    #[test]
    fn direct_reverse_increases_order_quantity_without_pre_liquidation() {
        let mut state = state_context();
        state.current_position = Some(sample_position(-2));

        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::DirectReverse,
                false,
                1,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: false,
                reason: "direct reverse".to_owned(),
            },
        })
        .expect("direct reverse should plan");

        assert_eq!(plan.actions.len(), 1);

        match &plan.actions[0] {
            ExecutionAction::SubmitOrder(order) => {
                assert_eq!(order.quantity, 3);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn same_side_scale_in_is_blocked_when_strategy_disables_it() {
        let mut state = state_context();
        state.current_position = Some(sample_position(1));

        let error = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: false,
                reason: "scale in".to_owned(),
            },
        })
        .expect_err("same-side scale-in should be blocked");

        assert_eq!(error, ExecutionEngineError::ScaleInDisabled);
    }

    #[test]
    fn entry_is_blocked_when_runtime_disallows_new_positions() {
        let mut state = state_context();
        state.new_entries_allowed = false;

        let error = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: false,
                reason: "blocked new entry".to_owned(),
            },
        })
        .expect_err("new entries should be blocked when runtime disallows them");

        assert_eq!(error, ExecutionEngineError::NewEntriesBlocked);
    }

    #[test]
    fn same_side_scale_in_is_planned_when_strategy_enables_it() {
        let mut state = state_context();
        state.current_position = Some(sample_position(1));

        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                true,
                3,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 2,
                protective_brackets_expected: false,
                reason: "scale in".to_owned(),
            },
        })
        .expect("same-side scale-in should plan");

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            ExecutionAction::SubmitOrder(order) => {
                assert_eq!(order.side, TradeSide::Buy);
                assert_eq!(order.quantity, 2);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn same_side_scale_in_is_blocked_when_scale_units_reach_max_legs() {
        let mut state = state_context();
        state.current_position = Some(sample_position(3));

        let error = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                true,
                3,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: false,
                reason: "scale in after max legs".to_owned(),
            },
        })
        .expect_err("same-side scale-in should stop once the configured maximum is reached");

        assert_eq!(error, ExecutionEngineError::ScaleInMaxLegsReached);
    }

    #[test]
    fn reduce_position_uses_market_order_in_opposite_direction() {
        let mut state = state_context();
        state.current_position = Some(sample_position(3));

        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                true,
                3,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::ReducePosition {
                quantity: 1,
                reason: "trim".to_owned(),
            },
        })
        .expect("reduce position should plan");

        match &plan.actions[0] {
            ExecutionAction::SubmitOrder(order) => {
                assert_eq!(order.side, TradeSide::Sell);
                assert_eq!(order.quantity, 1);
                assert_eq!(order.order_type, TradovateOrderType::Market);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn flatten_without_position_is_safe_noop_with_warning() {
        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state: state_context(),
            intent: ExecutionIntent::Flatten {
                reason: "manual flatten".to_owned(),
            },
        })
        .expect("flatten without position should not fail");

        assert!(plan.is_noop());
        assert_eq!(plan.warnings.len(), 1);
    }

    #[test]
    fn flatten_remains_available_when_runtime_order_placement_is_otherwise_blocked() {
        let mut state = state_context();
        state.runtime_can_submit_orders = false;
        state.current_position = Some(sample_position(1));

        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BrokerPreferred,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Flatten {
                reason: "safety flatten".to_owned(),
            },
        })
        .expect("flatten should stay available for safety exits");

        assert!(matches!(
            &plan.actions[0],
            ExecutionAction::LiquidatePosition {
                contract_id: 4444,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn dispatch_executes_flatten_then_bracket_entry_through_session_manager() {
        let execution_api = FakeExecutionApi::default();
        let mut manager = sample_session_manager().await;

        let mut state = state_context();
        state.current_position = Some(sample_position(-1));

        let plan = ExecutionPlanner::plan_tradovate(&ExecutionRequest {
            strategy: sample_strategy(
                ReversalMode::FlattenFirst,
                false,
                1,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BrokerRequired,
                BrokerPreference::BotAllowed,
            ),
            instrument: instrument_context(),
            state,
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: true,
                reason: "dispatch reversal".to_owned(),
            },
        })
        .expect("plan should succeed");

        let report = execute_tradovate_plan(plan, &mut manager, &execution_api)
            .await
            .expect("dispatch should succeed");

        assert_eq!(report.results.len(), 2);
        assert!(matches!(
            &report.results[0],
            ExecutionDispatchResult::PositionLiquidated {
                order_id: 7003,
                contract_id: 4444,
                ..
            }
        ));
        assert!(matches!(
            &report.results[1],
            ExecutionDispatchResult::OrderSubmitted {
                order_id: 7002,
                used_brackets: true,
                ..
            }
        ));

        let orders = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].context.account_id, 101);
        assert_eq!(orders[0].order.symbol, "GCM2026");
        assert_eq!(liquidations.len(), 1);
        assert_eq!(liquidations[0].contract_id, 4444);
    }

    #[tokio::test]
    async fn dispatch_reports_pause_without_touching_broker_execution() {
        let execution_api = FakeExecutionApi::default();
        let mut manager = sample_session_manager().await;

        let report = execute_tradovate_plan(
            ExecutionPlan {
                actions: vec![ExecutionAction::PauseStrategy {
                    reason: "operator pause".to_owned(),
                }],
                warnings: vec!["existing warning".to_owned()],
            },
            &mut manager,
            &execution_api,
        )
        .await
        .expect("pause dispatch should succeed");

        assert_eq!(report.warnings, vec!["existing warning".to_owned()]);
        assert_eq!(
            report.results,
            vec![ExecutionDispatchResult::StrategyPaused {
                reason: "operator pause".to_owned(),
            }]
        );
        assert!(execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
        assert!(execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
        assert!(execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
    }

    #[tokio::test]
    async fn plan_and_execute_cancels_working_orders_for_loaded_symbol() {
        let execution_api = FakeExecutionApi::default();
        let mut manager = sample_session_manager().await;

        let mut state = state_context();
        state.working_orders = vec![BrokerOrderUpdate {
            broker_order_id: "8102".to_owned(),
            account_id: Some("101".to_owned()),
            symbol: "GCM2026".to_owned(),
            side: Some(TradeSide::Buy),
            quantity: Some(1),
            order_type: Some(EntryOrderType::Limit),
            status: tv_bot_core_types::BrokerOrderStatus::Working,
            filled_quantity: 0,
            limit_price: Some(Decimal::new(241_200, 2)),
            stop_price: None,
            average_fill_price: None,
            updated_at: Utc::now(),
        }];

        let report = plan_and_execute_tradovate(
            &ExecutionRequest {
                strategy: sample_strategy(
                    ReversalMode::FlattenFirst,
                    false,
                    1,
                    BrokerPreference::BrokerPreferred,
                    BrokerPreference::BrokerPreferred,
                    BrokerPreference::BotAllowed,
                ),
                instrument: instrument_context(),
                state,
                intent: ExecutionIntent::CancelWorkingOrders {
                    reason: "cancel stale working order".to_owned(),
                },
            },
            &mut manager,
            &execution_api,
        )
        .await
        .expect("working-order cancellation should dispatch successfully");

        assert_eq!(
            report.results,
            vec![ExecutionDispatchResult::OrderCancelled {
                order_id: 8102,
                symbol: "GCM2026".to_owned(),
                reason: "cancel stale working order".to_owned(),
            }]
        );

        let cancel_orders = execution_api
            .cancel_orders
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(cancel_orders.len(), 1);
        assert_eq!(cancel_orders[0].context.account_id, 101);
        assert_eq!(cancel_orders[0].order_id, 8102);
        assert!(cancel_orders[0].is_automated);
        drop(cancel_orders);

        assert!(execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
        assert!(execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
        assert!(execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
    }

    #[tokio::test]
    async fn plan_and_execute_dispatches_scale_in_through_paper_account() {
        let execution_api = FakeExecutionApi::default();
        let mut manager = sample_session_manager().await;

        let mut state = state_context();
        state.current_position = Some(sample_position(1));

        let report = plan_and_execute_tradovate(
            &ExecutionRequest {
                strategy: sample_strategy(
                    ReversalMode::FlattenFirst,
                    true,
                    3,
                    BrokerPreference::BrokerPreferred,
                    BrokerPreference::BrokerPreferred,
                    BrokerPreference::BotAllowed,
                ),
                instrument: instrument_context(),
                state,
                intent: ExecutionIntent::Enter {
                    side: TradeSide::Buy,
                    order_type: EntryOrderType::Market,
                    quantity: 1,
                    protective_brackets_expected: false,
                    reason: "paper scale in".to_owned(),
                },
            },
            &mut manager,
            &execution_api,
        )
        .await
        .expect("paper scale-in should dispatch successfully");

        assert_eq!(report.results.len(), 1);
        assert!(matches!(
            &report.results[0],
            ExecutionDispatchResult::OrderSubmitted {
                order_id: 7001,
                used_brackets: false,
                ..
            }
        ));

        let orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].context.account_id, 101);
        assert_eq!(orders[0].order.symbol, "GCM2026");
        assert_eq!(orders[0].order.quantity, 1);
        assert!(liquidations.is_empty());
    }

    #[tokio::test]
    async fn plan_and_execute_surfaces_planning_errors_before_broker_calls() {
        let execution_api = FakeExecutionApi::default();
        let mut manager = sample_session_manager().await;

        let mut state = state_context();
        state.runtime_can_submit_orders = false;

        let error = plan_and_execute_tradovate(
            &ExecutionRequest {
                strategy: sample_strategy(
                    ReversalMode::FlattenFirst,
                    false,
                    1,
                    BrokerPreference::BrokerRequired,
                    BrokerPreference::BrokerRequired,
                    BrokerPreference::BotAllowed,
                ),
                instrument: instrument_context(),
                state,
                intent: ExecutionIntent::Enter {
                    side: TradeSide::Buy,
                    order_type: EntryOrderType::Market,
                    quantity: 1,
                    protective_brackets_expected: true,
                    reason: "blocked".to_owned(),
                },
            },
            &mut manager,
            &execution_api,
        )
        .await
        .expect_err("planning error should be returned before dispatch");

        assert_eq!(
            error,
            ExecutionDispatchError::Planning {
                source: ExecutionEngineError::OrderPlacementBlocked,
            }
        );
        assert!(execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
        assert!(execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
        assert!(execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());
    }
}
