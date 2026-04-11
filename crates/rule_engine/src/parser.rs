use std::{collections::BTreeMap, str::FromStr};

use rust_decimal::Decimal;
use tv_bot_core_types::Timeframe;

use crate::{
    BreakoutParams, ConditionExpression, PullbackParams, RejectionParams, RuleEngineError,
    SmaCrossParams, SmaParams, TrendParams, VolumeGateParams,
};

pub fn parse_condition_expression(raw: &str) -> Result<ConditionExpression, RuleEngineError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(RuleEngineError::MalformedCondition {
            condition: raw.to_owned(),
            detail: "condition must not be empty".to_owned(),
        });
    }

    let (name, parameters) = if let Some(paren_index) = trimmed.find('(') {
        if !trimmed.ends_with(')') {
            return Err(RuleEngineError::MalformedCondition {
                condition: trimmed.to_owned(),
                detail: "missing closing `)`".to_owned(),
            });
        }
        let parameter_block = &trimmed[paren_index + 1..trimmed.len() - 1];
        (
            trimmed[..paren_index].trim(),
            parse_parameter_map(trimmed, parameter_block)?,
        )
    } else {
        (trimmed, BTreeMap::new())
    };

    match name {
        "trend_filter" => Ok(ConditionExpression::TrendFilter(parse_trend_params(
            trimmed,
            &parameters,
        )?)),
        "trend_up" => Ok(ConditionExpression::TrendUp(parse_trend_params(
            trimmed,
            &parameters,
        )?)),
        "trend_down" => Ok(ConditionExpression::TrendDown(parse_trend_params(
            trimmed,
            &parameters,
        )?)),
        "breakout_up" => Ok(ConditionExpression::BreakoutUp(parse_breakout_params(
            trimmed,
            &parameters,
        )?)),
        "breakout_down" => Ok(ConditionExpression::BreakoutDown(parse_breakout_params(
            trimmed,
            &parameters,
        )?)),
        "rejection" => Ok(ConditionExpression::Rejection(parse_rejection_params(
            trimmed,
            &parameters,
        )?)),
        "volume_gate" => Ok(ConditionExpression::VolumeGate(parse_volume_gate_params(
            trimmed,
            &parameters,
        )?)),
        "pullback_done" => Ok(ConditionExpression::PullbackDone(parse_pullback_params(
            trimmed,
            &parameters,
        )?)),
        "fail_structure" => Ok(ConditionExpression::FailStructure(parse_trend_params(
            trimmed,
            &parameters,
        )?)),
        "regime_invalid" => Ok(ConditionExpression::RegimeInvalid(parse_trend_params(
            trimmed,
            &parameters,
        )?)),
        "close_above_sma" => Ok(ConditionExpression::CloseAboveSma(parse_sma_params(
            trimmed,
            &parameters,
        )?)),
        "close_below_sma" => Ok(ConditionExpression::CloseBelowSma(parse_sma_params(
            trimmed,
            &parameters,
        )?)),
        "sma_cross_up" => Ok(ConditionExpression::SmaCrossUp(parse_cross_params(
            trimmed,
            &parameters,
        )?)),
        "sma_cross_down" => Ok(ConditionExpression::SmaCrossDown(parse_cross_params(
            trimmed,
            &parameters,
        )?)),
        other => Err(RuleEngineError::UnknownCondition {
            condition: other.to_owned(),
        }),
    }
}

fn parse_parameter_map(
    condition: &str,
    parameter_block: &str,
) -> Result<BTreeMap<String, String>, RuleEngineError> {
    let mut parameters = BTreeMap::new();
    if parameter_block.trim().is_empty() {
        return Ok(parameters);
    }

    for entry in parameter_block.split(',') {
        let piece = entry.trim();
        if piece.is_empty() {
            continue;
        }

        let mut parts = piece.splitn(2, '=');
        let key = parts.next().unwrap_or_default().trim();
        let value = parts.next().unwrap_or_default().trim();
        if key.is_empty() || value.is_empty() {
            return Err(RuleEngineError::MalformedCondition {
                condition: condition.to_owned(),
                detail: format!("expected key=value parameter, found `{piece}`"),
            });
        }

        parameters.insert(key.to_owned(), value.to_owned());
    }

    Ok(parameters)
}

fn parse_trend_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<TrendParams, RuleEngineError> {
    let fast_period = parse_usize_parameter(condition, parameters, "fast", 5)?;
    let slow_period = parse_usize_parameter(condition, parameters, "slow", 20)?;
    if fast_period >= slow_period {
        return Err(RuleEngineError::InvalidParameter {
            condition: condition.to_owned(),
            parameter: "fast".to_owned(),
            detail: "fast period must be less than slow period".to_owned(),
        });
    }

    Ok(TrendParams {
        timeframe: parse_timeframe_parameter(
            condition,
            parameters,
            "timeframe",
            Timeframe::OneMinute,
        )?,
        fast_period,
        slow_period,
    })
}

fn parse_breakout_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<BreakoutParams, RuleEngineError> {
    Ok(BreakoutParams {
        timeframe: parse_timeframe_parameter(
            condition,
            parameters,
            "timeframe",
            Timeframe::OneMinute,
        )?,
        lookback: parse_usize_parameter(condition, parameters, "lookback", 20)?,
    })
}

fn parse_rejection_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<RejectionParams, RuleEngineError> {
    let close_fraction =
        parse_decimal_parameter(condition, parameters, "close_fraction", Decimal::new(6, 1))?;
    if close_fraction <= Decimal::ZERO || close_fraction >= Decimal::ONE {
        return Err(RuleEngineError::InvalidParameter {
            condition: condition.to_owned(),
            parameter: "close_fraction".to_owned(),
            detail: "close_fraction must be between 0 and 1".to_owned(),
        });
    }

    Ok(RejectionParams {
        timeframe: parse_timeframe_parameter(
            condition,
            parameters,
            "timeframe",
            Timeframe::OneMinute,
        )?,
        wick_ratio: parse_decimal_parameter(condition, parameters, "wick_ratio", Decimal::from(2))?,
        close_fraction,
    })
}

fn parse_volume_gate_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<VolumeGateParams, RuleEngineError> {
    Ok(VolumeGateParams {
        timeframe: parse_timeframe_parameter(
            condition,
            parameters,
            "timeframe",
            Timeframe::OneMinute,
        )?,
        period: parse_usize_parameter(condition, parameters, "period", 20)?,
        min_ratio: parse_decimal_parameter(
            condition,
            parameters,
            "min_ratio",
            Decimal::new(12, 1),
        )?,
    })
}

fn parse_pullback_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<PullbackParams, RuleEngineError> {
    let trend = parse_trend_params(condition, parameters)?;
    Ok(PullbackParams {
        timeframe: trend.timeframe,
        fast_period: trend.fast_period,
        slow_period: trend.slow_period,
    })
}

fn parse_sma_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<SmaParams, RuleEngineError> {
    Ok(SmaParams {
        timeframe: parse_timeframe_parameter(
            condition,
            parameters,
            "timeframe",
            Timeframe::OneMinute,
        )?,
        period: parse_usize_parameter(condition, parameters, "period", 20)?,
    })
}

fn parse_cross_params(
    condition: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<SmaCrossParams, RuleEngineError> {
    let trend = parse_trend_params(condition, parameters)?;
    Ok(SmaCrossParams {
        timeframe: trend.timeframe,
        fast_period: trend.fast_period,
        slow_period: trend.slow_period,
    })
}

fn parse_usize_parameter(
    condition: &str,
    parameters: &BTreeMap<String, String>,
    key: &str,
    default: usize,
) -> Result<usize, RuleEngineError> {
    match parameters.get(key) {
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|_| RuleEngineError::InvalidParameter {
                condition: condition.to_owned(),
                parameter: key.to_owned(),
                detail: format!("`{raw}` is not a valid positive integer"),
            }),
        None => Ok(default),
    }
}

fn parse_decimal_parameter(
    condition: &str,
    parameters: &BTreeMap<String, String>,
    key: &str,
    default: Decimal,
) -> Result<Decimal, RuleEngineError> {
    match parameters.get(key) {
        Some(raw) => Decimal::from_str(raw).map_err(|_| RuleEngineError::InvalidParameter {
            condition: condition.to_owned(),
            parameter: key.to_owned(),
            detail: format!("`{raw}` is not a valid decimal"),
        }),
        None => Ok(default),
    }
}

fn parse_timeframe_parameter(
    condition: &str,
    parameters: &BTreeMap<String, String>,
    key: &str,
    default: Timeframe,
) -> Result<Timeframe, RuleEngineError> {
    let value = match parameters.get(key) {
        Some(value) => value.as_str(),
        None => return Ok(default),
    };

    match value {
        "1s" => Ok(Timeframe::OneSecond),
        "1m" => Ok(Timeframe::OneMinute),
        "5m" => Ok(Timeframe::FiveMinute),
        _ => Err(RuleEngineError::InvalidParameter {
            condition: condition.to_owned(),
            parameter: key.to_owned(),
            detail: format!("unsupported timeframe `{value}`"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_condition_with_explicit_parameters() {
        let expression = parse_condition_expression("trend_filter(timeframe=5m,fast=8,slow=21)")
            .expect("condition should parse");

        match expression {
            ConditionExpression::TrendFilter(params) => {
                assert_eq!(params.timeframe, Timeframe::FiveMinute);
                assert_eq!(params.fast_period, 8);
                assert_eq!(params.slow_period, 21);
            }
            other => panic!("unexpected expression: {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_cross_configuration() {
        let error = parse_condition_expression("sma_cross_up(fast=20,slow=10)")
            .expect_err("fast >= slow should fail");

        assert!(matches!(
            error,
            RuleEngineError::InvalidParameter { parameter, .. } if parameter == "fast"
        ));
    }
}
