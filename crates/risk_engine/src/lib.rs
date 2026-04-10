//! Strategy-agnostic risk evaluation for sizing and broker-protection gates.

use rust_decimal::{prelude::ToPrimitive, Decimal};
use tv_bot_core_types::{
    BrokerPositionSnapshot, BrokerPreference, CompiledStrategy, ExecutionIntent,
    PositionSizingMode, RiskDecision, RiskDecisionStatus,
};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BrokerProtectionSupport {
    pub stop_loss: bool,
    pub take_profit: bool,
    pub trailing_stop: bool,
    pub daily_loss_limit: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RiskInstrumentContext {
    pub tick_value_usd: Option<Decimal>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RiskStateContext {
    pub trades_today: u32,
    pub consecutive_losses: u32,
    pub current_position: Option<BrokerPositionSnapshot>,
    pub unrealized_pnl: Option<Decimal>,
    pub broker_support: BrokerProtectionSupport,
    pub hard_override_active: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RiskEvaluationRequest {
    pub strategy: CompiledStrategy,
    pub instrument: RiskInstrumentContext,
    pub state: RiskStateContext,
    pub intent: ExecutionIntent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RiskEvaluationOutcome {
    pub decision: RiskDecision,
    pub adjusted_intent: ExecutionIntent,
    pub approved_quantity: Option<u32>,
    pub hard_override_reasons: Vec<String>,
}

impl RiskEvaluationOutcome {
    pub fn allows_execution(&self) -> bool {
        self.decision.status == RiskDecisionStatus::Accepted
    }
}

pub struct RiskEvaluator;

impl RiskEvaluator {
    pub fn evaluate(request: &RiskEvaluationRequest) -> RiskEvaluationOutcome {
        let mut warnings = Vec::new();
        let hard_override_reasons =
            evaluate_broker_protection_support(&request.strategy, &request.state, &mut warnings);

        match &request.intent {
            ExecutionIntent::Enter { .. } => {
                Self::evaluate_enter(request, warnings, hard_override_reasons)
            }
            _ => build_override_outcome(
                request.intent.clone(),
                None,
                "risk checks passed",
                warnings,
                hard_override_reasons,
                request.state.hard_override_active,
            ),
        }
    }

    fn evaluate_enter(
        request: &RiskEvaluationRequest,
        warnings: Vec<String>,
        hard_override_reasons: Vec<String>,
    ) -> RiskEvaluationOutcome {
        if request.state.trades_today >= request.strategy.risk.max_trades_per_day {
            return rejected_outcome(
                request.intent.clone(),
                format!(
                    "max trades per day reached ({})",
                    request.strategy.risk.max_trades_per_day
                ),
                warnings,
                hard_override_reasons,
            );
        }

        if request.state.consecutive_losses >= request.strategy.risk.max_consecutive_losses {
            return rejected_outcome(
                request.intent.clone(),
                format!(
                    "max consecutive losses reached ({})",
                    request.strategy.risk.max_consecutive_losses
                ),
                warnings,
                hard_override_reasons,
            );
        }

        if let Some(limit) = request.strategy.risk.max_unrealized_drawdown_usd {
            if let Some(unrealized_pnl) = request.state.unrealized_pnl {
                if unrealized_pnl.is_sign_negative() && unrealized_pnl.abs() >= limit {
                    return rejected_outcome(
                        request.intent.clone(),
                        format!("max unrealized drawdown reached ({limit} USD)"),
                        warnings,
                        hard_override_reasons,
                    );
                }
            }
        }

        let approved_quantity = match resolve_entry_quantity(request) {
            Ok(quantity) => quantity,
            Err(reason) => {
                return rejected_outcome(
                    request.intent.clone(),
                    reason,
                    warnings,
                    hard_override_reasons,
                )
            }
        };

        let adjusted_intent = match &request.intent {
            ExecutionIntent::Enter {
                side,
                order_type,
                protective_brackets_expected,
                reason,
                ..
            } => ExecutionIntent::Enter {
                side: *side,
                order_type: *order_type,
                quantity: approved_quantity,
                protective_brackets_expected: *protective_brackets_expected,
                reason: reason.clone(),
            },
            other => other.clone(),
        };

        build_override_outcome(
            adjusted_intent,
            Some(approved_quantity),
            "risk checks passed",
            warnings,
            hard_override_reasons,
            request.state.hard_override_active,
        )
    }
}

fn build_override_outcome(
    adjusted_intent: ExecutionIntent,
    approved_quantity: Option<u32>,
    accepted_reason: impl Into<String>,
    mut warnings: Vec<String>,
    hard_override_reasons: Vec<String>,
    hard_override_active: bool,
) -> RiskEvaluationOutcome {
    warnings.extend(hard_override_reasons.iter().cloned());

    let decision = if hard_override_reasons.is_empty() {
        RiskDecision {
            status: RiskDecisionStatus::Accepted,
            reason: accepted_reason.into(),
            warnings,
        }
    } else if hard_override_active {
        RiskDecision {
            status: RiskDecisionStatus::Accepted,
            reason: "risk checks passed under active hard override".to_owned(),
            warnings,
        }
    } else {
        RiskDecision {
            status: RiskDecisionStatus::RequiresOverride,
            reason: "broker-required protections are unavailable".to_owned(),
            warnings,
        }
    };

    RiskEvaluationOutcome {
        decision,
        adjusted_intent,
        approved_quantity,
        hard_override_reasons,
    }
}

fn rejected_outcome(
    adjusted_intent: ExecutionIntent,
    reason: impl Into<String>,
    mut warnings: Vec<String>,
    hard_override_reasons: Vec<String>,
) -> RiskEvaluationOutcome {
    warnings.extend(hard_override_reasons.iter().cloned());

    RiskEvaluationOutcome {
        decision: RiskDecision {
            status: RiskDecisionStatus::Rejected,
            reason: reason.into(),
            warnings,
        },
        adjusted_intent,
        approved_quantity: None,
        hard_override_reasons,
    }
}

fn evaluate_broker_protection_support(
    strategy: &CompiledStrategy,
    state: &RiskStateContext,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let mut hard_override_reasons = Vec::new();

    if strategy.trade_management.initial_stop_ticks > 0 {
        evaluate_feature_support(
            strategy.execution.broker_preferences.stop_loss,
            state.broker_support.stop_loss,
            "broker-side stop-loss protection",
            warnings,
            &mut hard_override_reasons,
        );
    }

    if strategy.trade_management.take_profit_ticks > 0 {
        evaluate_feature_support(
            strategy.execution.broker_preferences.take_profit,
            state.broker_support.take_profit,
            "broker-side take-profit protection",
            warnings,
            &mut hard_override_reasons,
        );
    }

    if strategy
        .trade_management
        .trailing
        .as_ref()
        .is_some_and(|rule| rule.enabled)
    {
        evaluate_feature_support(
            strategy.execution.broker_preferences.trailing_stop,
            state.broker_support.trailing_stop,
            "broker-side trailing-stop protection",
            warnings,
            &mut hard_override_reasons,
        );
    }

    if strategy.risk.daily_loss.broker_side_required && !state.broker_support.daily_loss_limit {
        hard_override_reasons.push(
            "broker-side daily loss protection is unavailable for the loaded strategy".to_owned(),
        );
    }

    hard_override_reasons
}

fn evaluate_feature_support(
    preference: BrokerPreference,
    supported: bool,
    feature_label: &str,
    warnings: &mut Vec<String>,
    hard_override_reasons: &mut Vec<String>,
) {
    if supported {
        return;
    }

    match preference {
        BrokerPreference::BrokerRequired => {
            hard_override_reasons.push(format!("{feature_label} is unavailable"));
        }
        BrokerPreference::BrokerPreferred => {
            warnings.push(format!(
                "{feature_label} is unavailable; local runtime handling will be required"
            ));
        }
        BrokerPreference::BotAllowed => {}
    }
}

fn resolve_entry_quantity(request: &RiskEvaluationRequest) -> Result<u32, String> {
    validate_contract_bounds(
        request.strategy.position_sizing.min_contracts,
        request.strategy.position_sizing.max_contracts,
    )?;

    match request.strategy.position_sizing.mode {
        PositionSizingMode::Fixed => fixed_quantity(request),
        PositionSizingMode::RiskBased => risk_based_quantity(request),
    }
}

fn fixed_quantity(request: &RiskEvaluationRequest) -> Result<u32, String> {
    let requested_quantity = match &request.intent {
        ExecutionIntent::Enter { quantity, .. } => *quantity,
        _ => 0,
    };
    let quantity = request
        .strategy
        .position_sizing
        .contracts
        .unwrap_or(requested_quantity);

    if quantity == 0 {
        return Err("fixed sizing requires a positive contract quantity".to_owned());
    }

    Ok(apply_contract_bounds(
        quantity,
        request.strategy.position_sizing.min_contracts,
        request.strategy.position_sizing.max_contracts,
    ))
}

fn risk_based_quantity(request: &RiskEvaluationRequest) -> Result<u32, String> {
    let max_risk_usd = request
        .strategy
        .position_sizing
        .max_risk_usd
        .ok_or_else(|| "risk-based sizing requires max_risk_usd".to_owned())?;

    if max_risk_usd <= Decimal::ZERO {
        return Err("risk-based sizing requires max_risk_usd to be greater than zero".to_owned());
    }

    let stop_ticks = request.strategy.trade_management.initial_stop_ticks;
    if stop_ticks == 0 {
        return Err("risk-based sizing requires a positive initial_stop_ticks".to_owned());
    }

    let tick_value_usd = request
        .instrument
        .tick_value_usd
        .ok_or_else(|| "risk-based sizing requires instrument tick_value_usd".to_owned())?;

    if tick_value_usd <= Decimal::ZERO {
        return Err("risk-based sizing requires tick_value_usd to be greater than zero".to_owned());
    }

    let risk_per_contract = Decimal::from(stop_ticks) * tick_value_usd;
    let computed_quantity = (max_risk_usd / risk_per_contract)
        .floor()
        .to_u32()
        .unwrap_or(0);

    let quantity = if computed_quantity == 0 {
        request
            .strategy
            .position_sizing
            .fallback_fixed_contracts
            .unwrap_or(0)
    } else {
        computed_quantity
    };

    let quantity = apply_contract_bounds(
        quantity,
        request.strategy.position_sizing.min_contracts,
        request.strategy.position_sizing.max_contracts,
    );

    if quantity == 0 {
        return Err("risk-based sizing produced zero contracts".to_owned());
    }

    Ok(quantity)
}

fn validate_contract_bounds(
    min_contracts: Option<u32>,
    max_contracts: Option<u32>,
) -> Result<(), String> {
    if let (Some(min_contracts), Some(max_contracts)) = (min_contracts, max_contracts) {
        if min_contracts > max_contracts {
            return Err(
                "position sizing min_contracts cannot be greater than max_contracts".to_owned(),
            );
        }
    }

    Ok(())
}

fn apply_contract_bounds(
    quantity: u32,
    min_contracts: Option<u32>,
    max_contracts: Option<u32>,
) -> u32 {
    let quantity = min_contracts.map_or(quantity, |min| quantity.max(min));
    max_contracts.map_or(quantity, |max| quantity.min(max))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use tv_bot_core_types::{
        BreakEvenRule, BrokerPositionSnapshot, ContractMode, DailyLossLimit, DashboardDisplay,
        DataFeedRequirement, DataRequirements, EntryOrderType, EntryRules, ExecutionSpec,
        ExitRules, FailsafeRules, FeedType, MarketConfig, MarketSelection, PartialTakeProfitRule,
        PositionSizing, ReversalMode, RiskLimits, ScalingConfig, SessionMode, SessionRules,
        SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata, Timeframe,
        TradeManagement, TrailingRule,
    };

    use super::{
        BrokerProtectionSupport, RiskEvaluationRequest, RiskEvaluator, RiskInstrumentContext,
        RiskStateContext,
    };

    use tv_bot_core_types::{
        BrokerPreference, CompiledStrategy, ExecutionIntent, PositionSizingMode, RiskDecisionStatus,
    };

    fn sample_strategy() -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "gc_risk_test_v1".to_owned(),
                name: "GC Risk Test".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "risk tests".to_owned(),
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
                feeds: vec![DataFeedRequirement {
                    kind: FeedType::Trades,
                }],
                timeframes: vec![Timeframe::OneMinute],
                multi_timeframe: false,
                requires: None,
            },
            warmup: tv_bot_core_types::WarmupRequirements {
                bars_required: BTreeMap::from([(Timeframe::OneMinute, 10)]),
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
                contracts: Some(2),
                max_risk_usd: None,
                min_contracts: None,
                max_contracts: None,
                fallback_fixed_contracts: Some(1),
                rounding_mode: None,
            },
            execution: ExecutionSpec {
                reversal_mode: ReversalMode::FlattenFirst,
                scaling: ScalingConfig {
                    allow_scale_in: false,
                    allow_scale_out: false,
                    max_legs: 1,
                },
                broker_preferences: tv_bot_core_types::BrokerPreferences {
                    stop_loss: BrokerPreference::BotAllowed,
                    take_profit: BrokerPreference::BotAllowed,
                    trailing_stop: BrokerPreference::BotAllowed,
                },
            },
            trade_management: TradeManagement {
                initial_stop_ticks: 12,
                take_profit_ticks: 24,
                break_even: Some(BreakEvenRule {
                    enabled: true,
                    activate_at_ticks: Some(8),
                }),
                trailing: Some(TrailingRule {
                    enabled: true,
                    activate_at_ticks: Some(10),
                    trail_ticks: Some(4),
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
                    broker_side_required: false,
                    local_backup_enabled: true,
                },
                max_trades_per_day: 3,
                max_consecutive_losses: 2,
                max_open_positions: Some(1),
                max_unrealized_drawdown_usd: Some(Decimal::new(500, 0)),
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

    fn sample_instrument() -> RiskInstrumentContext {
        RiskInstrumentContext {
            tick_value_usd: Some(Decimal::new(10, 0)),
        }
    }

    fn sample_state() -> RiskStateContext {
        RiskStateContext {
            trades_today: 0,
            consecutive_losses: 0,
            current_position: None,
            unrealized_pnl: Some(Decimal::ZERO),
            broker_support: BrokerProtectionSupport {
                stop_loss: true,
                take_profit: true,
                trailing_stop: true,
                daily_loss_limit: true,
            },
            hard_override_active: false,
        }
    }

    fn sample_position(quantity: i32) -> BrokerPositionSnapshot {
        BrokerPositionSnapshot {
            symbol: "GCM2026".to_owned(),
            quantity,
            average_price: Some(Decimal::new(238_500, 2)),
            realized_pnl: None,
            unrealized_pnl: Some(Decimal::new(-125, 0)),
            protective_orders_present: true,
            captured_at: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
        }
    }

    fn sample_enter_intent(quantity: u32) -> ExecutionIntent {
        ExecutionIntent::Enter {
            side: tv_bot_core_types::TradeSide::Buy,
            order_type: EntryOrderType::Market,
            quantity,
            protective_brackets_expected: true,
            reason: "entry".to_owned(),
        }
    }

    #[test]
    fn fixed_sizing_overrides_requested_entry_quantity() {
        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy: sample_strategy(),
            instrument: sample_instrument(),
            state: sample_state(),
            intent: sample_enter_intent(1),
        });

        assert!(outcome.allows_execution());
        assert_eq!(outcome.approved_quantity, Some(2));
        assert_eq!(outcome.decision.status, RiskDecisionStatus::Accepted);

        match outcome.adjusted_intent {
            ExecutionIntent::Enter { quantity, .. } => assert_eq!(quantity, 2),
            other => panic!("unexpected adjusted intent: {other:?}"),
        }
    }

    #[test]
    fn risk_based_sizing_computes_contracts_from_stop_and_tick_value() {
        let mut strategy = sample_strategy();
        strategy.position_sizing.mode = PositionSizingMode::RiskBased;
        strategy.position_sizing.contracts = None;
        strategy.position_sizing.max_risk_usd = Some(Decimal::new(750, 0));
        strategy.position_sizing.fallback_fixed_contracts = None;
        strategy.trade_management.initial_stop_ticks = 15;

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy,
            instrument: RiskInstrumentContext {
                tick_value_usd: Some(Decimal::new(125, 1)),
            },
            state: sample_state(),
            intent: sample_enter_intent(1),
        });

        assert!(outcome.allows_execution());
        assert_eq!(outcome.approved_quantity, Some(4));

        match outcome.adjusted_intent {
            ExecutionIntent::Enter { quantity, .. } => assert_eq!(quantity, 4),
            other => panic!("unexpected adjusted intent: {other:?}"),
        }
    }

    #[test]
    fn rejects_when_max_trades_per_day_is_reached() {
        let strategy = sample_strategy();
        let mut state = sample_state();
        state.trades_today = strategy.risk.max_trades_per_day;

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy,
            instrument: sample_instrument(),
            state,
            intent: sample_enter_intent(1),
        });

        assert_eq!(outcome.decision.status, RiskDecisionStatus::Rejected);
        assert!(outcome.decision.reason.contains("max trades per day"));
    }

    #[test]
    fn rejects_when_max_consecutive_losses_is_reached() {
        let strategy = sample_strategy();
        let mut state = sample_state();
        state.consecutive_losses = strategy.risk.max_consecutive_losses;

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy,
            instrument: sample_instrument(),
            state,
            intent: sample_enter_intent(1),
        });

        assert_eq!(outcome.decision.status, RiskDecisionStatus::Rejected);
        assert!(outcome.decision.reason.contains("max consecutive losses"));
    }

    #[test]
    fn rejects_when_unrealized_drawdown_limit_is_breached() {
        let mut state = sample_state();
        state.current_position = Some(sample_position(1));
        state.unrealized_pnl = Some(Decimal::new(-600, 0));

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy: sample_strategy(),
            instrument: sample_instrument(),
            state,
            intent: sample_enter_intent(1),
        });

        assert_eq!(outcome.decision.status, RiskDecisionStatus::Rejected);
        assert!(outcome.decision.reason.contains("max unrealized drawdown"));
    }

    #[test]
    fn requires_override_when_broker_required_stop_support_is_unavailable() {
        let mut strategy = sample_strategy();
        strategy.execution.broker_preferences.stop_loss = BrokerPreference::BrokerRequired;

        let mut state = sample_state();
        state.broker_support.stop_loss = false;

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy,
            instrument: sample_instrument(),
            state,
            intent: sample_enter_intent(1),
        });

        assert_eq!(
            outcome.decision.status,
            RiskDecisionStatus::RequiresOverride
        );
        assert_eq!(
            outcome.hard_override_reasons,
            vec!["broker-side stop-loss protection is unavailable".to_owned()]
        );
        assert!(outcome
            .decision
            .warnings
            .contains(&"broker-side stop-loss protection is unavailable".to_owned()));
    }

    #[test]
    fn active_hard_override_allows_broker_required_protection_gap() {
        let mut strategy = sample_strategy();
        strategy.execution.broker_preferences.take_profit = BrokerPreference::BrokerRequired;

        let mut state = sample_state();
        state.broker_support.take_profit = false;
        state.hard_override_active = true;

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy,
            instrument: sample_instrument(),
            state,
            intent: sample_enter_intent(1),
        });

        assert_eq!(outcome.decision.status, RiskDecisionStatus::Accepted);
        assert!(outcome.decision.reason.contains("hard override"));
        assert_eq!(
            outcome.hard_override_reasons,
            vec!["broker-side take-profit protection is unavailable".to_owned()]
        );
    }

    #[test]
    fn broker_preferred_trailing_gap_becomes_warning_only() {
        let mut strategy = sample_strategy();
        strategy.execution.broker_preferences.trailing_stop = BrokerPreference::BrokerPreferred;

        let mut state = sample_state();
        state.broker_support.trailing_stop = false;

        let outcome = RiskEvaluator::evaluate(&RiskEvaluationRequest {
            strategy,
            instrument: sample_instrument(),
            state,
            intent: sample_enter_intent(1),
        });

        assert_eq!(outcome.decision.status, RiskDecisionStatus::Accepted);
        assert!(outcome.hard_override_reasons.is_empty());
        assert_eq!(outcome.decision.warnings.len(), 1);
        assert!(outcome.decision.warnings[0]
            .contains("broker-side trailing-stop protection is unavailable"));
    }
}
