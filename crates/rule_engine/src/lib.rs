//! Strategy-agnostic parsing and evaluation of built-in rule conditions.

mod eval;
mod parser;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use thiserror::Error;
use tv_bot_core_types::{BrokerPositionSnapshot, SignalCombinationMode, Timeframe};
use tv_bot_indicators::BarInput;

pub use parser::parse_condition_expression;

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluationSide {
    Long,
    Short,
}

impl EvaluationSide {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleEvaluationContext {
    pub bars_by_timeframe: BTreeMap<Timeframe, Vec<BarInput>>,
    pub now: DateTime<Utc>,
    pub position: Option<BrokerPositionSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConditionEvaluation {
    pub raw: String,
    pub passed: bool,
    pub score: Decimal,
    pub rationale: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuleSetEvaluation {
    pub matched: bool,
    pub matched_conditions: usize,
    pub total_conditions: usize,
    pub score: Decimal,
    pub details: Vec<ConditionEvaluation>,
    pub blocking_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignalPlan<'a> {
    pub mode: SignalCombinationMode,
    pub primary: &'a [CompiledCondition],
    pub secondary: &'a [CompiledCondition],
    pub n_required: Option<u32>,
    pub score_threshold: Option<Decimal>,
    pub regime_filter: Option<&'a CompiledCondition>,
    pub sequence: &'a [CompiledCondition],
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledCondition {
    pub raw: String,
    pub expression: ConditionExpression,
}

impl CompiledCondition {
    pub fn parse(raw: &str) -> Result<Self, RuleEngineError> {
        let expression = parse_condition_expression(raw)?;
        Ok(Self {
            raw: raw.trim().to_owned(),
            expression,
        })
    }

    pub fn timeframe(&self) -> Timeframe {
        self.expression.timeframe()
    }

    pub fn required_bars(&self) -> usize {
        self.expression.required_bars()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConditionExpression {
    TrendFilter(TrendParams),
    TrendUp(TrendParams),
    TrendDown(TrendParams),
    BreakoutUp(BreakoutParams),
    BreakoutDown(BreakoutParams),
    Rejection(RejectionParams),
    VolumeGate(VolumeGateParams),
    PullbackDone(PullbackParams),
    FailStructure(TrendParams),
    RegimeInvalid(TrendParams),
    CloseAboveSma(SmaParams),
    CloseBelowSma(SmaParams),
    SmaCrossUp(SmaCrossParams),
    SmaCrossDown(SmaCrossParams),
}

impl ConditionExpression {
    pub(crate) fn timeframe(&self) -> Timeframe {
        match self {
            Self::TrendFilter(params)
            | Self::TrendUp(params)
            | Self::TrendDown(params)
            | Self::FailStructure(params)
            | Self::RegimeInvalid(params) => params.timeframe,
            Self::BreakoutUp(params) | Self::BreakoutDown(params) => params.timeframe,
            Self::Rejection(params) => params.timeframe,
            Self::VolumeGate(params) => params.timeframe,
            Self::PullbackDone(params) => params.timeframe,
            Self::CloseAboveSma(params) | Self::CloseBelowSma(params) => params.timeframe,
            Self::SmaCrossUp(params) | Self::SmaCrossDown(params) => params.timeframe,
        }
    }

    pub(crate) fn required_bars(&self) -> usize {
        match self {
            Self::TrendFilter(params)
            | Self::TrendUp(params)
            | Self::TrendDown(params)
            | Self::FailStructure(params)
            | Self::RegimeInvalid(params) => params.slow_period,
            Self::BreakoutUp(params) | Self::BreakoutDown(params) => params.lookback + 1,
            Self::Rejection(_) => 1,
            Self::VolumeGate(params) => params.period + 1,
            Self::PullbackDone(params) => params.fast_period.max(params.slow_period),
            Self::CloseAboveSma(params) | Self::CloseBelowSma(params) => params.period,
            Self::SmaCrossUp(params) | Self::SmaCrossDown(params) => params.slow_period + 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrendParams {
    pub timeframe: Timeframe,
    pub fast_period: usize,
    pub slow_period: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakoutParams {
    pub timeframe: Timeframe,
    pub lookback: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RejectionParams {
    pub timeframe: Timeframe,
    pub wick_ratio: Decimal,
    pub close_fraction: Decimal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VolumeGateParams {
    pub timeframe: Timeframe,
    pub period: usize,
    pub min_ratio: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullbackParams {
    pub timeframe: Timeframe,
    pub fast_period: usize,
    pub slow_period: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmaParams {
    pub timeframe: Timeframe,
    pub period: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmaCrossParams {
    pub timeframe: Timeframe,
    pub fast_period: usize,
    pub slow_period: usize,
}

#[derive(Debug, Error, PartialEq)]
pub enum RuleEngineError {
    #[error("unknown condition `{condition}`")]
    UnknownCondition { condition: String },
    #[error("condition `{condition}` is malformed: {detail}")]
    MalformedCondition { condition: String, detail: String },
    #[error("condition `{condition}` has invalid parameter `{parameter}`: {detail}")]
    InvalidParameter {
        condition: String,
        parameter: String,
        detail: String,
    },
    #[error("no bars are available for timeframe {timeframe:?}")]
    MissingBars { timeframe: Timeframe },
    #[error("condition `{condition}` requires at least {needed} bars on {timeframe:?} but only {available} are available")]
    InsufficientBars {
        condition: String,
        timeframe: Timeframe,
        needed: usize,
        available: usize,
    },
    #[error("bar series for timeframe {timeframe:?} is empty while evaluating `{condition}`")]
    EmptySeries {
        condition: String,
        timeframe: Timeframe,
    },
}

pub struct RuleEngine;

impl RuleEngine {
    pub fn evaluate_condition(
        condition: &CompiledCondition,
        side: EvaluationSide,
        context: &RuleEvaluationContext,
    ) -> Result<ConditionEvaluation, RuleEngineError> {
        eval::evaluate_condition(condition, side, context)
    }

    pub fn evaluate_signal_plan(
        plan: &SignalPlan<'_>,
        side: EvaluationSide,
        context: &RuleEvaluationContext,
    ) -> Result<RuleSetEvaluation, RuleEngineError> {
        eval::evaluate_signal_plan(plan, side, context)
    }
}
