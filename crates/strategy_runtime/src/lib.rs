//! Strategy runtime compilation and evaluation.

use std::{collections::BTreeMap, str::FromStr};

use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde_json::Value;
use thiserror::Error;
use tv_bot_core_types::{
    BrokerPositionSnapshot, BrokerPreference, BrokerSyncState, CompiledStrategy, ExecutionIntent,
    FlattenRuleMode, PositionSizingMode, SessionMode, SignalDecision, SignalDirection, TradeSide,
    WarmupStatus,
};
use tv_bot_indicators::BarInput;
use tv_bot_rule_engine::{
    CompiledCondition, EvaluationSide, RuleEngine, RuleEngineError, RuleSetEvaluation, SignalPlan,
};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeTradeWindow {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFlattenRule {
    pub mode: FlattenRuleMode,
    pub time: Option<NaiveTime>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSignalPlan {
    pub primary: Vec<CompiledCondition>,
    pub secondary: Vec<CompiledCondition>,
    pub n_required: Option<u32>,
    pub score_threshold: Option<Decimal>,
    pub regime_filter: Option<CompiledCondition>,
    pub sequence: Vec<CompiledCondition>,
}

impl RuntimeSignalPlan {
    fn as_rule_plan(&self, strategy: &CompiledStrategy) -> SignalPlan<'_> {
        SignalPlan {
            mode: strategy.signal_confirmation.mode,
            primary: &self.primary,
            secondary: &self.secondary,
            n_required: self.n_required,
            score_threshold: self.score_threshold,
            regime_filter: self.regime_filter.as_ref(),
            sequence: &self.sequence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RuntimeEntryConditions {
    pub long: Vec<CompiledCondition>,
    pub short: Vec<CompiledCondition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeStrategyDefinition {
    pub strategy: CompiledStrategy,
    pub timezone: Tz,
    pub trade_window: Option<RuntimeTradeWindow>,
    pub no_new_entries_after: Option<NaiveTime>,
    pub flatten_rule: Option<RuntimeFlattenRule>,
    pub allowed_days: Vec<Weekday>,
    pub signal_plan: RuntimeSignalPlan,
    pub entry_conditions: RuntimeEntryConditions,
    pub exit_conditions: Vec<CompiledCondition>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct StrategyRuntimeState {
    pub long_signal_active: bool,
    pub short_signal_active: bool,
    pub long_exit_active: bool,
    pub short_exit_active: bool,
    pub last_signal_direction: Option<SignalDirection>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrategyMarketSnapshot {
    pub now: DateTime<Utc>,
    pub warmup_status: WarmupStatus,
    pub bars_by_timeframe: BTreeMap<tv_bot_core_types::Timeframe, Vec<BarInput>>,
    pub position: Option<BrokerPositionSnapshot>,
    pub market_data_degraded: bool,
    pub broker_sync_state: BrokerSyncState,
    pub reconnect_review_required: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrategyEvaluation {
    pub signal: SignalDecision,
    pub intent: Option<ExecutionIntent>,
}

#[derive(Clone, Debug, PartialEq)]
struct SideSignalEvaluation {
    active: bool,
    score: Decimal,
    rationale: Vec<String>,
}

pub struct StrategyRuntimeCompiler;

impl StrategyRuntimeCompiler {
    pub fn compile(
        strategy: &CompiledStrategy,
    ) -> Result<RuntimeStrategyDefinition, StrategyRuntimeError> {
        let timezone = Tz::from_str(strategy.session.timezone.trim()).map_err(|_| {
            StrategyRuntimeError::InvalidTimezone {
                timezone: strategy.session.timezone.clone(),
            }
        })?;

        let trade_window = strategy
            .session
            .trade_window
            .as_ref()
            .map(
                |window| -> Result<RuntimeTradeWindow, StrategyRuntimeError> {
                    Ok(RuntimeTradeWindow {
                        start: parse_time("session.trade_window.start", &window.start)?,
                        end: parse_time("session.trade_window.end", &window.end)?,
                    })
                },
            )
            .transpose()?;
        let no_new_entries_after = strategy
            .session
            .no_new_entries_after
            .as_deref()
            .map(|value| parse_time("session.no_new_entries_after", value))
            .transpose()?;
        let flatten_rule = strategy
            .session
            .flatten_rule
            .as_ref()
            .map(|rule| -> Result<RuntimeFlattenRule, StrategyRuntimeError> {
                Ok(RuntimeFlattenRule {
                    mode: rule.mode,
                    time: rule
                        .time
                        .as_deref()
                        .map(|value| parse_time("session.flatten_rule.time", value))
                        .transpose()?,
                })
            })
            .transpose()?;

        let allowed_days = strategy
            .session
            .allowed_days
            .iter()
            .map(|value| parse_weekday(value))
            .collect::<Result<Vec<_>, _>>()?;

        let signal_plan = RuntimeSignalPlan {
            primary: compile_conditions(&strategy.signal_confirmation.primary_conditions)?,
            secondary: compile_conditions(&strategy.signal_confirmation.secondary_conditions)?,
            n_required: strategy.signal_confirmation.n_required,
            score_threshold: strategy.signal_confirmation.score_threshold,
            regime_filter: strategy
                .signal_confirmation
                .regime_filter
                .as_deref()
                .map(CompiledCondition::parse)
                .transpose()?,
            sequence: compile_conditions(&strategy.signal_confirmation.sequence)?,
        };
        let entry_conditions =
            compile_entry_conditions(strategy.entry_rules.entry_conditions.as_ref())?;
        let exit_conditions = compile_conditions(&strategy.exit_rules.exit_conditions)?;

        for condition in signal_plan
            .primary
            .iter()
            .chain(signal_plan.secondary.iter())
            .chain(signal_plan.regime_filter.iter())
            .chain(signal_plan.sequence.iter())
            .chain(entry_conditions.long.iter())
            .chain(entry_conditions.short.iter())
            .chain(exit_conditions.iter())
        {
            validate_condition_requirements(strategy, condition)?;
        }

        Ok(RuntimeStrategyDefinition {
            strategy: strategy.clone(),
            timezone,
            trade_window,
            no_new_entries_after,
            flatten_rule,
            allowed_days,
            signal_plan,
            entry_conditions,
            exit_conditions,
        })
    }
}

pub struct StrategyRuntimeEngine;

#[derive(Debug, Error, PartialEq)]
pub enum StrategyRuntimeError {
    #[error("invalid timezone `{timezone}`")]
    InvalidTimezone { timezone: String },
    #[error("invalid time for `{field}`: `{value}`")]
    InvalidTime { field: String, value: String },
    #[error("invalid allowed day `{value}`")]
    InvalidAllowedDay { value: String },
    #[error("entry_conditions is invalid: {detail}")]
    InvalidEntryConditions { detail: String },
    #[error("condition `{condition}` references timeframe {timeframe:?}, which is not declared by the strategy")]
    UndeclaredConditionTimeframe {
        condition: String,
        timeframe: tv_bot_core_types::Timeframe,
    },
    #[error("condition `{condition}` requires {required_bars} warmup bars on {timeframe:?}, but the strategy only declares {declared_bars}")]
    InsufficientWarmupBars {
        condition: String,
        timeframe: tv_bot_core_types::Timeframe,
        required_bars: usize,
        declared_bars: u32,
    },
    #[error(transparent)]
    RuleEngine(#[from] RuleEngineError),
}

impl StrategyRuntimeEngine {
    pub fn evaluate(
        definition: &RuntimeStrategyDefinition,
        state: &mut StrategyRuntimeState,
        snapshot: &StrategyMarketSnapshot,
    ) -> Result<StrategyEvaluation, StrategyRuntimeError> {
        if snapshot.reconnect_review_required
            && definition
                .strategy
                .failsafes
                .pause_on_reconnect_until_reviewed
                .unwrap_or(false)
        {
            reset_state(state);
            return Ok(flat_evaluation(
                &definition.strategy,
                snapshot.now,
                Some(ExecutionIntent::PauseStrategy {
                    reason: "reconnect review is still required".to_owned(),
                }),
                vec!["reconnect review is still required".to_owned()],
            ));
        }

        if snapshot.broker_sync_state == BrokerSyncState::Mismatch
            && definition.strategy.failsafes.pause_on_broker_sync_mismatch
        {
            reset_state(state);
            return Ok(flat_evaluation(
                &definition.strategy,
                snapshot.now,
                Some(ExecutionIntent::PauseStrategy {
                    reason: "broker sync mismatch requires review".to_owned(),
                }),
                vec!["broker sync mismatch requires review".to_owned()],
            ));
        }

        if snapshot.warmup_status != WarmupStatus::Ready
            || !warmup_buffers_ready(definition, snapshot)
        {
            reset_state(state);
            return Ok(flat_evaluation(
                &definition.strategy,
                snapshot.now,
                None,
                vec![format!(
                    "warmup is not ready ({:?})",
                    snapshot.warmup_status
                )],
            ));
        }

        let sync_degraded = snapshot.broker_sync_state != BrokerSyncState::Synchronized;
        if (snapshot.market_data_degraded || sync_degraded) && snapshot.position.is_some() {
            reset_state(state);
            return Ok(flat_evaluation(
                &definition.strategy,
                snapshot.now,
                None,
                vec![format!(
                    "existing position is left broker-side because runtime health is degraded (data_degraded={}, broker_sync={:?})",
                    snapshot.market_data_degraded, snapshot.broker_sync_state
                )],
            ));
        }

        let rule_context = tv_bot_rule_engine::RuleEvaluationContext {
            bars_by_timeframe: snapshot.bars_by_timeframe.clone(),
            now: snapshot.now,
            position: snapshot.position.clone(),
        };

        let long_signal = evaluate_side_signal(definition, EvaluationSide::Long, &rule_context)?;
        let short_signal = evaluate_side_signal(definition, EvaluationSide::Short, &rule_context)?;
        let (direction, score, mut rationale) =
            choose_signal_direction(&long_signal, &short_signal);

        let (entries_allowed, session_blockers, flatten_due) =
            session_state(definition, snapshot, sync_degraded);
        rationale.extend(session_blockers);

        let position_side = position_side(snapshot.position.as_ref());
        let explicit_exit_active = match position_side {
            Some(EvaluationSide::Long) => evaluate_all_conditions(
                &definition.exit_conditions,
                EvaluationSide::Long,
                &rule_context,
            )?,
            Some(EvaluationSide::Short) => evaluate_all_conditions(
                &definition.exit_conditions,
                EvaluationSide::Short,
                &rule_context,
            )?,
            None => empty_rule_set(),
        };

        let mut intent = None;

        if flatten_due {
            intent = Some(ExecutionIntent::Flatten {
                reason: "session flatten rule triggered".to_owned(),
            });
            rationale.push("session flatten rule triggered".to_owned());
        } else if let Some(side) = position_side {
            let exit_edge = match side {
                EvaluationSide::Long => !state.long_exit_active,
                EvaluationSide::Short => !state.short_exit_active,
            };
            if explicit_exit_active.matched && exit_edge {
                intent = Some(ExecutionIntent::Exit {
                    reason: "explicit exit conditions matched".to_owned(),
                });
                rationale.push("explicit exit conditions matched".to_owned());
            } else if definition.strategy.exit_rules.exit_on_opposite_signal
                && opposite_direction(direction, side)
            {
                intent = Some(ExecutionIntent::Exit {
                    reason: "opposite signal matched".to_owned(),
                });
                rationale.push("opposite signal matched".to_owned());
            } else if entries_allowed && opposite_direction(direction, side) {
                intent = Some(entry_intent(&definition.strategy, direction, &rationale));
            }
        } else if entries_allowed {
            let entry_edge = match direction {
                SignalDirection::Long => !state.long_signal_active,
                SignalDirection::Short => !state.short_signal_active,
                SignalDirection::Flat => false,
            };

            if entry_edge {
                match direction {
                    SignalDirection::Long | SignalDirection::Short => {
                        intent = Some(entry_intent(&definition.strategy, direction, &rationale));
                    }
                    SignalDirection::Flat => {}
                }
            }
        }

        state.long_signal_active = long_signal.active;
        state.short_signal_active = short_signal.active;
        state.long_exit_active =
            position_side == Some(EvaluationSide::Long) && explicit_exit_active.matched;
        state.short_exit_active =
            position_side == Some(EvaluationSide::Short) && explicit_exit_active.matched;
        state.last_signal_direction = Some(direction);

        Ok(StrategyEvaluation {
            signal: SignalDecision {
                strategy_id: definition.strategy.metadata.strategy_id.clone(),
                direction,
                score,
                rationale,
                occurred_at: snapshot.now,
            },
            intent,
        })
    }
}

fn compile_conditions(
    raw_conditions: &[String],
) -> Result<Vec<CompiledCondition>, StrategyRuntimeError> {
    raw_conditions
        .iter()
        .map(|raw| CompiledCondition::parse(raw).map_err(StrategyRuntimeError::from))
        .collect()
}

fn compile_entry_conditions(
    raw: Option<&Value>,
) -> Result<RuntimeEntryConditions, StrategyRuntimeError> {
    let Some(raw) = raw else {
        return Ok(RuntimeEntryConditions::default());
    };

    let Value::Object(map) = raw else {
        return Err(StrategyRuntimeError::InvalidEntryConditions {
            detail: "entry_conditions must be an object with optional `long` and `short` arrays"
                .to_owned(),
        });
    };

    for key in map.keys() {
        if key != "long" && key != "short" {
            return Err(StrategyRuntimeError::InvalidEntryConditions {
                detail: format!("unknown entry_conditions key `{key}`"),
            });
        }
    }

    Ok(RuntimeEntryConditions {
        long: compile_condition_array(map.get("long"), "long")?,
        short: compile_condition_array(map.get("short"), "short")?,
    })
}

fn compile_condition_array(
    raw: Option<&Value>,
    side: &str,
) -> Result<Vec<CompiledCondition>, StrategyRuntimeError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };

    let Value::Array(items) = raw else {
        return Err(StrategyRuntimeError::InvalidEntryConditions {
            detail: format!("entry_conditions.{side} must be an array of strings"),
        });
    };

    let mut compiled = Vec::with_capacity(items.len());
    for item in items {
        let Value::String(raw_condition) = item else {
            return Err(StrategyRuntimeError::InvalidEntryConditions {
                detail: format!("entry_conditions.{side} must contain only strings"),
            });
        };
        compiled.push(CompiledCondition::parse(raw_condition)?);
    }

    Ok(compiled)
}

fn validate_condition_requirements(
    strategy: &CompiledStrategy,
    condition: &CompiledCondition,
) -> Result<(), StrategyRuntimeError> {
    let timeframe = condition.timeframe();
    if !strategy.data_requirements.timeframes.contains(&timeframe) {
        return Err(StrategyRuntimeError::UndeclaredConditionTimeframe {
            condition: condition.raw.clone(),
            timeframe,
        });
    }

    let declared = strategy
        .warmup
        .bars_required
        .get(&timeframe)
        .copied()
        .unwrap_or(0);
    let required = condition.required_bars();
    if declared < required as u32 {
        return Err(StrategyRuntimeError::InsufficientWarmupBars {
            condition: condition.raw.clone(),
            timeframe,
            required_bars: required,
            declared_bars: declared,
        });
    }

    Ok(())
}

fn parse_time(field: &str, raw: &str) -> Result<NaiveTime, StrategyRuntimeError> {
    NaiveTime::parse_from_str(raw, "%H:%M:%S").map_err(|_| StrategyRuntimeError::InvalidTime {
        field: field.to_owned(),
        value: raw.to_owned(),
    })
}

fn parse_weekday(raw: &str) -> Result<Weekday, StrategyRuntimeError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thurs" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        _ => Err(StrategyRuntimeError::InvalidAllowedDay {
            value: raw.to_owned(),
        }),
    }
}

fn warmup_buffers_ready(
    definition: &RuntimeStrategyDefinition,
    snapshot: &StrategyMarketSnapshot,
) -> bool {
    let mut readiness = definition
        .strategy
        .warmup
        .bars_required
        .iter()
        .map(|(timeframe, required)| {
            snapshot
                .bars_by_timeframe
                .get(timeframe)
                .map(|bars| bars.len() >= *required as usize)
                .unwrap_or(false)
        });

    if definition.strategy.warmup.ready_requires_all {
        readiness.all(|ready| ready)
    } else {
        readiness.any(|ready| ready)
    }
}

fn evaluate_side_signal(
    definition: &RuntimeStrategyDefinition,
    side: EvaluationSide,
    context: &tv_bot_rule_engine::RuleEvaluationContext,
) -> Result<SideSignalEvaluation, StrategyRuntimeError> {
    let side_enabled = match side {
        EvaluationSide::Long => definition.strategy.entry_rules.long_enabled,
        EvaluationSide::Short => definition.strategy.entry_rules.short_enabled,
    };
    if !side_enabled {
        return Ok(SideSignalEvaluation {
            active: false,
            score: Decimal::ZERO,
            rationale: vec![format!("{} side is disabled", side_label(side))],
        });
    }

    let signal_evaluation = RuleEngine::evaluate_signal_plan(
        &definition.signal_plan.as_rule_plan(&definition.strategy),
        side,
        context,
    )?;
    let mut rationale = summarize_rule_set(&signal_evaluation);
    if !signal_evaluation.matched {
        return Ok(SideSignalEvaluation {
            active: false,
            score: signal_evaluation.score,
            rationale,
        });
    }

    let entry_conditions = match side {
        EvaluationSide::Long => &definition.entry_conditions.long,
        EvaluationSide::Short => &definition.entry_conditions.short,
    };
    let side_evaluation = evaluate_all_conditions(entry_conditions, side, context)?;
    rationale.extend(summarize_rule_set(&side_evaluation));

    Ok(SideSignalEvaluation {
        active: signal_evaluation.matched && side_evaluation.matched,
        score: signal_evaluation.score,
        rationale,
    })
}

fn side_label(side: EvaluationSide) -> &'static str {
    match side {
        EvaluationSide::Long => "long",
        EvaluationSide::Short => "short",
    }
}

fn evaluate_all_conditions(
    conditions: &[CompiledCondition],
    side: EvaluationSide,
    context: &tv_bot_rule_engine::RuleEvaluationContext,
) -> Result<RuleSetEvaluation, StrategyRuntimeError> {
    if conditions.is_empty() {
        return Ok(empty_rule_set());
    }

    Ok(RuleEngine::evaluate_signal_plan(
        &SignalPlan {
            mode: tv_bot_core_types::SignalCombinationMode::All,
            primary: conditions,
            secondary: &[],
            n_required: None,
            score_threshold: None,
            regime_filter: None,
            sequence: &[],
        },
        side,
        context,
    )?)
}

fn empty_rule_set() -> RuleSetEvaluation {
    RuleSetEvaluation {
        matched: true,
        matched_conditions: 0,
        total_conditions: 0,
        score: Decimal::ONE,
        details: Vec::new(),
        blocking_reason: None,
    }
}

fn summarize_rule_set(evaluation: &RuleSetEvaluation) -> Vec<String> {
    let mut rationale = evaluation
        .details
        .iter()
        .map(|detail| {
            format!(
                "{} {} ({})",
                detail.raw,
                if detail.passed { "passed" } else { "failed" },
                detail.rationale
            )
        })
        .collect::<Vec<_>>();

    if let Some(reason) = &evaluation.blocking_reason {
        rationale.push(reason.clone());
    }

    rationale
}

fn choose_signal_direction(
    long_signal: &SideSignalEvaluation,
    short_signal: &SideSignalEvaluation,
) -> (SignalDirection, Option<Decimal>, Vec<String>) {
    match (long_signal.active, short_signal.active) {
        (true, false) => (
            SignalDirection::Long,
            Some(long_signal.score),
            long_signal.rationale.clone(),
        ),
        (false, true) => (
            SignalDirection::Short,
            Some(short_signal.score),
            short_signal.rationale.clone(),
        ),
        (true, true) if long_signal.score > short_signal.score => {
            let mut rationale = long_signal.rationale.clone();
            rationale.push("short setup also matched, but long score was higher".to_owned());
            (SignalDirection::Long, Some(long_signal.score), rationale)
        }
        (true, true) if short_signal.score > long_signal.score => {
            let mut rationale = short_signal.rationale.clone();
            rationale.push("long setup also matched, but short score was higher".to_owned());
            (SignalDirection::Short, Some(short_signal.score), rationale)
        }
        (true, true) => (
            SignalDirection::Flat,
            None,
            vec!["long and short setups matched with identical score".to_owned()],
        ),
        (false, false) => {
            let mut rationale = long_signal.rationale.clone();
            rationale.extend(short_signal.rationale.clone());
            if rationale.is_empty() {
                rationale.push("no setup matched".to_owned());
            }
            (SignalDirection::Flat, None, rationale)
        }
    }
}

fn session_state(
    definition: &RuntimeStrategyDefinition,
    snapshot: &StrategyMarketSnapshot,
    sync_degraded: bool,
) -> (bool, Vec<String>, bool) {
    let local_now = snapshot.now.with_timezone(&definition.timezone);
    let local_time = local_now.time();
    let weekday = local_now.weekday();

    let day_allowed =
        definition.allowed_days.is_empty() || definition.allowed_days.contains(&weekday);
    let within_window = match definition.strategy.session.mode {
        SessionMode::Always => true,
        SessionMode::FixedWindow => definition
            .trade_window
            .as_ref()
            .map(|window| trade_window_contains(window, local_time))
            .unwrap_or(false),
    };
    let after_entry_cutoff = definition
        .no_new_entries_after
        .map(|cutoff| local_time >= cutoff)
        .unwrap_or(false);

    let mut blockers = Vec::new();
    if !day_allowed {
        blockers.push(format!(
            "{} is not an allowed trading day",
            weekday_name(weekday)
        ));
    }
    if !within_window {
        blockers.push("current time is outside the configured trade window".to_owned());
    }
    if after_entry_cutoff {
        blockers.push("no-new-entry cutoff has been reached".to_owned());
    }
    if snapshot.market_data_degraded && definition.strategy.failsafes.no_new_entries_on_data_degrade
    {
        blockers.push("market data is degraded".to_owned());
    }
    if sync_degraded {
        blockers.push(format!(
            "broker sync is not healthy enough for new entries ({:?})",
            snapshot.broker_sync_state
        ));
    }

    let session_end_reached = match definition.strategy.session.mode {
        SessionMode::Always => false,
        SessionMode::FixedWindow => definition
            .trade_window
            .as_ref()
            .map(|window| trade_window_session_end_reached(window, local_time) || !day_allowed)
            .unwrap_or(false),
    };
    let flatten_due_to_rule =
        definition
            .flatten_rule
            .as_ref()
            .is_some_and(|rule| match rule.mode {
                FlattenRuleMode::None => false,
                FlattenRuleMode::SessionEnd => session_end_reached,
                FlattenRuleMode::ByTime => rule.time.is_some_and(|time| local_time >= time),
            });
    let flatten_due = snapshot.position.is_some()
        && ((definition.strategy.exit_rules.flatten_on_session_end && session_end_reached)
            || flatten_due_to_rule);

    (blockers.is_empty(), blockers, flatten_due)
}

fn trade_window_contains(window: &RuntimeTradeWindow, local_time: NaiveTime) -> bool {
    if trade_window_wraps_midnight(window) {
        local_time >= window.start || local_time < window.end
    } else {
        local_time >= window.start && local_time < window.end
    }
}

fn trade_window_session_end_reached(window: &RuntimeTradeWindow, local_time: NaiveTime) -> bool {
    if trade_window_wraps_midnight(window) {
        local_time >= window.end && local_time < window.start
    } else {
        local_time >= window.end
    }
}

fn trade_window_wraps_midnight(window: &RuntimeTradeWindow) -> bool {
    window.start >= window.end
}

fn opposite_direction(direction: SignalDirection, side: EvaluationSide) -> bool {
    matches!(
        (direction, side),
        (SignalDirection::Long, EvaluationSide::Short)
            | (SignalDirection::Short, EvaluationSide::Long)
    )
}

fn position_side(position: Option<&BrokerPositionSnapshot>) -> Option<EvaluationSide> {
    match position.map(|position| position.quantity) {
        Some(quantity) if quantity > 0 => Some(EvaluationSide::Long),
        Some(quantity) if quantity < 0 => Some(EvaluationSide::Short),
        _ => None,
    }
}

fn entry_intent(
    strategy: &CompiledStrategy,
    direction: SignalDirection,
    rationale: &[String],
) -> ExecutionIntent {
    let side = match direction {
        SignalDirection::Long => TradeSide::Buy,
        SignalDirection::Short => TradeSide::Sell,
        SignalDirection::Flat => unreachable!("flat signals do not create entry intents"),
    };

    ExecutionIntent::Enter {
        side,
        order_type: strategy.entry_rules.entry_order_type,
        quantity: requested_entry_quantity(strategy),
        protective_brackets_expected: protective_brackets_expected(strategy),
        reason: format!(
            "{} {} entry: {}",
            strategy.metadata.strategy_id,
            match direction {
                SignalDirection::Long => "long",
                SignalDirection::Short => "short",
                SignalDirection::Flat => "flat",
            },
            rationale
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

fn requested_entry_quantity(strategy: &CompiledStrategy) -> u32 {
    match strategy.position_sizing.mode {
        PositionSizingMode::Fixed => strategy.position_sizing.contracts.unwrap_or(1),
        PositionSizingMode::RiskBased => strategy
            .position_sizing
            .fallback_fixed_contracts
            .or(strategy.position_sizing.min_contracts)
            .or(strategy.position_sizing.max_contracts)
            .unwrap_or(1),
    }
}

fn protective_brackets_expected(strategy: &CompiledStrategy) -> bool {
    strategy.execution.broker_preferences.stop_loss != BrokerPreference::BotAllowed
        || strategy.execution.broker_preferences.take_profit != BrokerPreference::BotAllowed
}

fn flat_evaluation(
    strategy: &CompiledStrategy,
    occurred_at: DateTime<Utc>,
    intent: Option<ExecutionIntent>,
    rationale: Vec<String>,
) -> StrategyEvaluation {
    StrategyEvaluation {
        signal: SignalDecision {
            strategy_id: strategy.metadata.strategy_id.clone(),
            direction: SignalDirection::Flat,
            score: None,
            rationale,
            occurred_at,
        },
        intent,
    }
}

fn reset_state(state: &mut StrategyRuntimeState) {
    state.long_signal_active = false;
    state.short_signal_active = false;
    state.long_exit_active = false;
    state.short_exit_active = false;
    state.last_signal_direction = Some(SignalDirection::Flat);
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}
