use rust_decimal::Decimal;
use tv_bot_core_types::{SignalCombinationMode, Timeframe};
use tv_bot_indicators::{
    average_volume, bar_body, close_position_ratio, exponential_moving_average, highest_high,
    latest_bar, lower_wick, lowest_low, simple_moving_average, upper_wick, BarInput,
    IndicatorError,
};

use crate::{
    BreakoutParams, CompiledCondition, ConditionEvaluation, ConditionExpression, EvaluationSide,
    PullbackParams, RejectionParams, RuleEngineError, RuleEvaluationContext, RuleSetEvaluation,
    SignalPlan, SmaCrossParams, SmaParams, TrendParams, VolumeGateParams,
};

pub(crate) fn evaluate_condition(
    condition: &CompiledCondition,
    side: EvaluationSide,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    match &condition.expression {
        ConditionExpression::TrendFilter(params) => trend_filter(condition, params, side, context),
        ConditionExpression::TrendUp(params) => trend_up(condition, params, context),
        ConditionExpression::TrendDown(params) => trend_down(condition, params, context),
        ConditionExpression::BreakoutUp(params) => breakout_up(condition, params, context),
        ConditionExpression::BreakoutDown(params) => breakout_down(condition, params, context),
        ConditionExpression::Rejection(params) => rejection(condition, params, side, context),
        ConditionExpression::VolumeGate(params) => volume_gate(condition, params, context),
        ConditionExpression::PullbackDone(params) => {
            pullback_done(condition, params, side, context)
        }
        ConditionExpression::FailStructure(params) => {
            fail_structure(condition, params, side, context)
        }
        ConditionExpression::RegimeInvalid(params) => {
            fail_structure(condition, params, side, context).map(|mut evaluation| {
                evaluation.rationale = format!(
                    "regime invalid for {} because {}",
                    side.label(),
                    evaluation.rationale
                );
                evaluation
            })
        }
        ConditionExpression::CloseAboveSma(params) => close_above_sma(condition, params, context),
        ConditionExpression::CloseBelowSma(params) => close_below_sma(condition, params, context),
        ConditionExpression::SmaCrossUp(params) => sma_cross_up(condition, params, context),
        ConditionExpression::SmaCrossDown(params) => sma_cross_down(condition, params, context),
    }
}

pub(crate) fn evaluate_signal_plan(
    plan: &SignalPlan<'_>,
    side: EvaluationSide,
    context: &RuleEvaluationContext,
) -> Result<RuleSetEvaluation, RuleEngineError> {
    if let Some(filter) = plan.regime_filter {
        let evaluation = evaluate_condition(filter, side, context)?;
        if !evaluation.passed {
            return Ok(RuleSetEvaluation {
                matched: false,
                matched_conditions: 0,
                total_conditions: plan.primary.len(),
                score: Decimal::ZERO,
                details: vec![evaluation],
                blocking_reason: Some("regime filter failed".to_owned()),
            });
        }
    }

    let mut details = Vec::new();
    for condition in plan.sequence {
        let evaluation = evaluate_condition(condition, side, context)?;
        if !evaluation.passed {
            return Ok(RuleSetEvaluation {
                matched: false,
                matched_conditions: 0,
                total_conditions: plan.primary.len(),
                score: Decimal::ZERO,
                details: vec![evaluation],
                blocking_reason: Some("sequence requirements are not satisfied".to_owned()),
            });
        }
        details.push(evaluation);
    }

    let primary = plan
        .primary
        .iter()
        .map(|condition| evaluate_condition(condition, side, context))
        .collect::<Result<Vec<_>, _>>()?;
    let matched_primary = primary.iter().filter(|item| item.passed).count();
    let total_primary = primary.len();
    details.extend(primary.clone());

    let (matched, score) = match plan.mode {
        SignalCombinationMode::All => (
            total_primary > 0 && matched_primary == total_primary,
            score_ratio(matched_primary, total_primary),
        ),
        SignalCombinationMode::Any => (
            matched_primary > 0,
            score_ratio(matched_primary, total_primary),
        ),
        SignalCombinationMode::NOfM => {
            let required = plan.n_required.unwrap_or(0) as usize;
            (
                required > 0 && matched_primary >= required,
                score_ratio(matched_primary, total_primary),
            )
        }
        SignalCombinationMode::WeightedScore => {
            let secondary = plan
                .secondary
                .iter()
                .map(|condition| evaluate_condition(condition, side, context))
                .collect::<Result<Vec<_>, _>>()?;
            let passed = primary
                .iter()
                .chain(secondary.iter())
                .filter(|item| item.passed)
                .count();
            let total = primary.len() + secondary.len();
            details.extend(secondary);
            let score = score_ratio(passed, total);
            (
                total > 0 && score >= plan.score_threshold.unwrap_or(Decimal::ONE),
                score,
            )
        }
    };

    Ok(RuleSetEvaluation {
        matched,
        matched_conditions: matched_primary,
        total_conditions: total_primary,
        score,
        details,
        blocking_reason: None,
    })
}

fn trend_filter(
    condition: &CompiledCondition,
    params: &TrendParams,
    side: EvaluationSide,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    match side {
        EvaluationSide::Long => trend_up(condition, params, context),
        EvaluationSide::Short => trend_down(condition, params, context),
    }
}

fn trend_up(
    condition: &CompiledCondition,
    params: &TrendParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.fast_period)
    })?;
    let slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.slow_period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = fast > slow && latest.close > slow;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "fast SMA {} is {} slow SMA {} and close {} is {} slow SMA",
            fast,
            if fast > slow { "above" } else { "not above" },
            slow,
            latest.close,
            if latest.close > slow {
                "above"
            } else {
                "not above"
            }
        ),
    ))
}

fn trend_down(
    condition: &CompiledCondition,
    params: &TrendParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.fast_period)
    })?;
    let slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.slow_period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = fast < slow && latest.close < slow;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "fast SMA {} is {} slow SMA {} and close {} is {} slow SMA",
            fast,
            if fast < slow { "below" } else { "not below" },
            slow,
            latest.close,
            if latest.close < slow {
                "below"
            } else {
                "not below"
            }
        ),
    ))
}

fn breakout_up(
    condition: &CompiledCondition,
    params: &BreakoutParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    if bars.len() <= params.lookback {
        return Err(RuleEngineError::InsufficientBars {
            condition: condition.raw.clone(),
            timeframe: params.timeframe,
            needed: params.lookback + 1,
            available: bars.len(),
        });
    }

    let previous = &bars[..bars.len() - 1];
    let highest = indicator_for_condition(condition, params.timeframe, || {
        highest_high(previous, params.lookback)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = latest.close > highest;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "close {} is {} previous breakout high {}",
            latest.close,
            if passed { "above" } else { "not above" },
            highest
        ),
    ))
}

fn breakout_down(
    condition: &CompiledCondition,
    params: &BreakoutParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    if bars.len() <= params.lookback {
        return Err(RuleEngineError::InsufficientBars {
            condition: condition.raw.clone(),
            timeframe: params.timeframe,
            needed: params.lookback + 1,
            available: bars.len(),
        });
    }

    let previous = &bars[..bars.len() - 1];
    let lowest = indicator_for_condition(condition, params.timeframe, || {
        lowest_low(previous, params.lookback)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = latest.close < lowest;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "close {} is {} previous breakout low {}",
            latest.close,
            if passed { "below" } else { "not below" },
            lowest
        ),
    ))
}

fn rejection(
    condition: &CompiledCondition,
    params: &RejectionParams,
    side: EvaluationSide,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let body = bar_body(latest);
    let wick = match side {
        EvaluationSide::Long => lower_wick(latest),
        EvaluationSide::Short => upper_wick(latest),
    };
    let close_ratio = close_position_ratio(latest);
    let directional_close_ok = match side {
        EvaluationSide::Long => close_ratio >= params.close_fraction,
        EvaluationSide::Short => close_ratio <= Decimal::ONE - params.close_fraction,
    };
    let wick_ok = if body.is_zero() {
        wick > Decimal::ZERO
    } else {
        wick >= body * params.wick_ratio
    };
    let passed = wick_ok && directional_close_ok;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "{} wick {} with body {} and close ratio {}",
            match side {
                EvaluationSide::Long => "lower",
                EvaluationSide::Short => "upper",
            },
            wick,
            body,
            close_ratio
        ),
    ))
}

fn volume_gate(
    condition: &CompiledCondition,
    params: &VolumeGateParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    if bars.len() <= params.period {
        return Err(RuleEngineError::InsufficientBars {
            condition: condition.raw.clone(),
            timeframe: params.timeframe,
            needed: params.period + 1,
            available: bars.len(),
        });
    }

    let previous = &bars[..bars.len() - 1];
    let baseline = indicator_for_condition(condition, params.timeframe, || {
        average_volume(previous, params.period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let current_volume = Decimal::from(latest.volume);
    let threshold = baseline * params.min_ratio;
    let passed = current_volume >= threshold;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "volume {} is {} threshold {}",
            current_volume,
            if passed { "above" } else { "below" },
            threshold
        ),
    ))
}

fn pullback_done(
    condition: &CompiledCondition,
    params: &PullbackParams,
    side: EvaluationSide,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let fast = indicator_for_condition(condition, params.timeframe, || {
        exponential_moving_average(bars, params.fast_period)
    })?;
    let slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.slow_period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = match side {
        EvaluationSide::Long => fast > slow && latest.low <= fast && latest.close >= fast,
        EvaluationSide::Short => fast < slow && latest.high >= fast && latest.close <= fast,
    };

    Ok(condition_result(
        condition,
        passed,
        format!(
            "pullback check with fast EMA {}, slow SMA {}, close {}, low {}, high {}",
            fast, slow, latest.close, latest.low, latest.high
        ),
    ))
}

fn fail_structure(
    condition: &CompiledCondition,
    params: &TrendParams,
    side: EvaluationSide,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.fast_period)
    })?;
    let slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.slow_period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = match side {
        EvaluationSide::Long => latest.close < slow || fast < slow,
        EvaluationSide::Short => latest.close > slow || fast > slow,
    };

    Ok(condition_result(
        condition,
        passed,
        format!(
            "structure check with fast SMA {}, slow SMA {}, close {}",
            fast, slow, latest.close
        ),
    ))
}

fn close_above_sma(
    condition: &CompiledCondition,
    params: &SmaParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let average = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = latest.close > average;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "close {} is {} SMA {}",
            latest.close,
            if passed { "above" } else { "not above" },
            average
        ),
    ))
}

fn close_below_sma(
    condition: &CompiledCondition,
    params: &SmaParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    let average = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.period)
    })?;
    let latest = latest_for_condition(condition, params.timeframe, bars)?;
    let passed = latest.close < average;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "close {} is {} SMA {}",
            latest.close,
            if passed { "below" } else { "not below" },
            average
        ),
    ))
}

fn sma_cross_up(
    condition: &CompiledCondition,
    params: &SmaCrossParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    if bars.len() <= params.slow_period {
        return Err(RuleEngineError::InsufficientBars {
            condition: condition.raw.clone(),
            timeframe: params.timeframe,
            needed: params.slow_period + 1,
            available: bars.len(),
        });
    }

    let previous = &bars[..bars.len() - 1];
    let previous_fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(previous, params.fast_period)
    })?;
    let previous_slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(previous, params.slow_period)
    })?;
    let current_fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.fast_period)
    })?;
    let current_slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.slow_period)
    })?;
    let passed = previous_fast <= previous_slow && current_fast > current_slow;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "SMA cross up with previous fast {}, previous slow {}, current fast {}, current slow {}",
            previous_fast, previous_slow, current_fast, current_slow
        ),
    ))
}

fn sma_cross_down(
    condition: &CompiledCondition,
    params: &SmaCrossParams,
    context: &RuleEvaluationContext,
) -> Result<ConditionEvaluation, RuleEngineError> {
    let bars = bars_for(condition, params.timeframe, context)?;
    if bars.len() <= params.slow_period {
        return Err(RuleEngineError::InsufficientBars {
            condition: condition.raw.clone(),
            timeframe: params.timeframe,
            needed: params.slow_period + 1,
            available: bars.len(),
        });
    }

    let previous = &bars[..bars.len() - 1];
    let previous_fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(previous, params.fast_period)
    })?;
    let previous_slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(previous, params.slow_period)
    })?;
    let current_fast = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.fast_period)
    })?;
    let current_slow = indicator_for_condition(condition, params.timeframe, || {
        simple_moving_average(bars, params.slow_period)
    })?;
    let passed = previous_fast >= previous_slow && current_fast < current_slow;

    Ok(condition_result(
        condition,
        passed,
        format!(
            "SMA cross down with previous fast {}, previous slow {}, current fast {}, current slow {}",
            previous_fast, previous_slow, current_fast, current_slow
        ),
    ))
}

fn bars_for<'a>(
    condition: &CompiledCondition,
    timeframe: Timeframe,
    context: &'a RuleEvaluationContext,
) -> Result<&'a [BarInput], RuleEngineError> {
    let Some(bars) = context.bars_by_timeframe.get(&timeframe) else {
        return Err(RuleEngineError::MissingBars { timeframe });
    };

    if bars.is_empty() {
        return Err(RuleEngineError::EmptySeries {
            condition: condition.raw.clone(),
            timeframe,
        });
    }

    Ok(bars.as_slice())
}

fn latest_for_condition<'a>(
    condition: &CompiledCondition,
    timeframe: Timeframe,
    bars: &'a [BarInput],
) -> Result<&'a BarInput, RuleEngineError> {
    latest_bar(bars).map_err(|source| map_indicator_error(condition, timeframe, source))
}

fn indicator_for_condition<F>(
    condition: &CompiledCondition,
    timeframe: Timeframe,
    indicator: F,
) -> Result<Decimal, RuleEngineError>
where
    F: FnOnce() -> Result<Decimal, IndicatorError>,
{
    indicator().map_err(|source| map_indicator_error(condition, timeframe, source))
}

fn map_indicator_error(
    condition: &CompiledCondition,
    timeframe: Timeframe,
    source: IndicatorError,
) -> RuleEngineError {
    match source {
        IndicatorError::EmptySeries => RuleEngineError::EmptySeries {
            condition: condition.raw.clone(),
            timeframe,
        },
        IndicatorError::InsufficientBars { needed, available } => {
            RuleEngineError::InsufficientBars {
                condition: condition.raw.clone(),
                timeframe,
                needed,
                available,
            }
        }
        IndicatorError::InvalidPeriod => RuleEngineError::MalformedCondition {
            condition: condition.raw.clone(),
            detail: "indicator period must be greater than zero".to_owned(),
        },
        IndicatorError::TimeframeMismatch { expected, found } => {
            RuleEngineError::MalformedCondition {
                condition: condition.raw.clone(),
                detail: format!(
                    "indicator timeframe mismatch: expected {expected:?}, found {found:?}"
                ),
            }
        }
        IndicatorError::NotBarEvent => RuleEngineError::MalformedCondition {
            condition: condition.raw.clone(),
            detail: "indicator expected bar inputs".to_owned(),
        },
    }
}

fn condition_result(
    condition: &CompiledCondition,
    passed: bool,
    rationale: String,
) -> ConditionEvaluation {
    ConditionEvaluation {
        raw: condition.raw.clone(),
        passed,
        score: if passed { Decimal::ONE } else { Decimal::ZERO },
        rationale,
    }
}

fn score_ratio(matched: usize, total: usize) -> Decimal {
    if total == 0 {
        Decimal::ZERO
    } else {
        Decimal::from(matched as u64) / Decimal::from(total as u64)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{Duration, Utc};
    use tv_bot_core_types::BrokerPositionSnapshot;

    use super::*;
    use crate::RuleEngine;

    fn bar(index: i64, close: i64, high: i64, low: i64, volume: u64) -> BarInput {
        BarInput {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(close - 1),
            high: Decimal::from(high),
            low: Decimal::from(low),
            close: Decimal::from(close),
            volume,
            closed_at: Utc::now() + Duration::minutes(index),
        }
    }

    fn bullish_context() -> RuleEvaluationContext {
        let mut bars = Vec::new();
        for (index, close) in [100, 101, 102, 103, 104, 105, 106, 107, 108]
            .iter()
            .enumerate()
        {
            bars.push(bar(index as i64, *close, close + 1, close - 2, 100));
        }
        bars.push(BarInput {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(107),
            high: Decimal::from(109),
            low: Decimal::from(101),
            close: Decimal::from(108),
            volume: 250,
            closed_at: Utc::now() + Duration::minutes(10),
        });

        let mut bars_by_timeframe = BTreeMap::new();
        bars_by_timeframe.insert(Timeframe::OneMinute, bars);

        RuleEvaluationContext {
            bars_by_timeframe,
            now: Utc::now(),
            position: None::<BrokerPositionSnapshot>,
        }
    }

    #[test]
    fn evaluates_bullish_trend_and_volume_conditions() {
        let context = bullish_context();
        let trend = CompiledCondition::parse("trend_filter(fast=3,slow=5)").expect("trend parses");
        let rejection =
            CompiledCondition::parse("rejection(wick_ratio=1.5)").expect("rejection parses");
        let volume =
            CompiledCondition::parse("volume_gate(period=3,min_ratio=1.5)").expect("volume parses");

        let trend_eval =
            RuleEngine::evaluate_condition(&trend, EvaluationSide::Long, &context).expect("trend");
        let rejection_eval =
            RuleEngine::evaluate_condition(&rejection, EvaluationSide::Long, &context)
                .expect("rejection");
        let volume_eval = RuleEngine::evaluate_condition(&volume, EvaluationSide::Long, &context)
            .expect("volume");

        assert!(trend_eval.passed);
        assert!(rejection_eval.passed);
        assert!(volume_eval.passed);
    }

    #[test]
    fn weighted_score_plan_uses_primary_and_secondary_conditions() {
        let context = bullish_context();
        let trend = CompiledCondition::parse("trend_filter(fast=3,slow=5)").expect("trend parses");
        let rejection =
            CompiledCondition::parse("rejection(wick_ratio=1.5)").expect("rejection parses");
        let pullback =
            CompiledCondition::parse("pullback_done(fast=3,slow=5)").expect("pullback parses");

        let plan = SignalPlan {
            mode: SignalCombinationMode::WeightedScore,
            primary: std::slice::from_ref(&trend),
            secondary: &[rejection, pullback],
            n_required: None,
            score_threshold: Some(Decimal::new(6, 1)),
            regime_filter: None,
            sequence: &[],
        };

        let evaluation =
            RuleEngine::evaluate_signal_plan(&plan, EvaluationSide::Long, &context).expect("plan");

        assert!(evaluation.matched);
        assert_eq!(evaluation.score, Decimal::ONE);
        assert_eq!(evaluation.details.len(), 3);
    }

    #[test]
    fn sma_cross_requires_a_real_crossing_bar() {
        let mut bars_by_timeframe = BTreeMap::new();
        bars_by_timeframe.insert(
            Timeframe::OneMinute,
            vec![
                bar(0, 10, 11, 9, 100),
                bar(1, 9, 10, 8, 100),
                bar(2, 8, 9, 7, 100),
                bar(3, 7, 8, 6, 100),
                bar(4, 10, 11, 9, 100),
            ],
        );
        let context = RuleEvaluationContext {
            bars_by_timeframe,
            now: Utc::now(),
            position: None,
        };
        let cross = CompiledCondition::parse("sma_cross_up(fast=2,slow=3)").expect("cross parses");

        let evaluation =
            RuleEngine::evaluate_condition(&cross, EvaluationSide::Long, &context).expect("cross");

        assert!(evaluation.passed);
        assert!(evaluation.rationale.contains("current fast"));
    }

    #[test]
    fn insufficient_bars_map_into_rule_errors() {
        let mut bars_by_timeframe = BTreeMap::new();
        bars_by_timeframe.insert(Timeframe::OneMinute, vec![bar(0, 10, 11, 9, 100)]);
        let context = RuleEvaluationContext {
            bars_by_timeframe,
            now: Utc::now(),
            position: None,
        };
        let condition =
            CompiledCondition::parse("trend_filter(fast=3,slow=5)").expect("trend parses");

        let error = RuleEngine::evaluate_condition(&condition, EvaluationSide::Long, &context)
            .expect_err("insufficient bars should fail");

        assert_eq!(
            error,
            RuleEngineError::InsufficientBars {
                condition: "trend_filter(fast=3,slow=5)".to_owned(),
                timeframe: Timeframe::OneMinute,
                needed: 3,
                available: 1,
            }
        );
    }
}
