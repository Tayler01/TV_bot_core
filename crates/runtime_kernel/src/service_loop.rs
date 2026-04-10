use chrono::{DateTime, Utc};
use serde_json::json;
use thiserror::Error;
use tv_bot_broker_tradovate::{
    Clock as TradovateClock, TradovateAccountApi, TradovateAuthApi, TradovateExecutionApi,
    TradovateSessionManager, TradovateSyncApi,
};
use tv_bot_core_types::{ActionSource, EventJournalRecord, EventSeverity};
use tv_bot_execution_engine::{
    ExecutionDispatchError, ExecutionDispatchReport, ExecutionDispatchResult,
};
use tv_bot_journal::{EventJournal, JournalError};

use crate::{
    evaluate_risk_and_execute_tradovate, RuntimeExecutionError, RuntimeExecutionOutcome,
    RuntimeExecutionRequest,
};

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeCommand {
    ManualIntent(RuntimeExecutionRequest),
    StrategyIntent(RuntimeExecutionRequest),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeCommandOutcome {
    Execution(RuntimeExecutionOutcome),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeCommandError {
    #[error("journal append failed: {source}")]
    Journal { source: JournalError },
    #[error("runtime execution failed: {source}")]
    Execution { source: RuntimeExecutionError },
}

pub struct RuntimeControlLoop;

impl RuntimeControlLoop {
    pub async fn handle_command<A, B, C, Clk, E, J>(
        command: RuntimeCommand,
        session: &mut TradovateSessionManager<A, B, C, Clk>,
        execution_api: &E,
        journal: &J,
    ) -> Result<RuntimeCommandOutcome, RuntimeCommandError>
    where
        A: TradovateAuthApi,
        B: TradovateAccountApi,
        C: TradovateSyncApi,
        Clk: TradovateClock,
        E: TradovateExecutionApi,
        J: EventJournal,
    {
        let request = normalize_request(command);
        journal_intent_received(&request, journal)
            .map_err(|source| RuntimeCommandError::Journal { source })?;

        let outcome =
            match evaluate_risk_and_execute_tradovate(&request, session, execution_api, journal)
                .await
            {
                Ok(outcome) => outcome,
                Err(RuntimeExecutionError::Dispatch { source }) => {
                    journal_dispatch_failed(&request, &source, journal)
                        .map_err(|source| RuntimeCommandError::Journal { source })?;

                    return Err(RuntimeCommandError::Execution {
                        source: RuntimeExecutionError::Dispatch { source },
                    });
                }
                Err(source) => {
                    return Err(RuntimeCommandError::Execution { source });
                }
            };

        if let Some(dispatch) = &outcome.dispatch {
            journal_dispatch_succeeded(&request, dispatch, journal)
                .map_err(|source| RuntimeCommandError::Journal { source })?;
        } else {
            journal_dispatch_skipped(&request, &outcome, journal)
                .map_err(|source| RuntimeCommandError::Journal { source })?;
        }

        Ok(RuntimeCommandOutcome::Execution(outcome))
    }
}

fn normalize_request(command: RuntimeCommand) -> RuntimeExecutionRequest {
    match command {
        RuntimeCommand::ManualIntent(request) => request,
        RuntimeCommand::StrategyIntent(mut request) => {
            request.action_source = ActionSource::System;
            request
        }
    }
}

fn journal_intent_received<J: EventJournal>(
    request: &RuntimeExecutionRequest,
    journal: &J,
) -> Result<(), JournalError> {
    let occurred_at = Utc::now();
    journal.append(EventJournalRecord {
        event_id: event_id(
            category_for_source(request.action_source),
            "intent_received",
            occurred_at,
        ),
        category: category_for_source(request.action_source).to_owned(),
        action: "intent_received".to_owned(),
        source: request.action_source,
        severity: EventSeverity::Info,
        occurred_at,
        payload: json!({
            "mode": request.mode,
            "strategy_id": request.execution.strategy.metadata.strategy_id,
            "intent": intent_name(&request.execution.intent),
        }),
    })
}

fn journal_dispatch_succeeded<J: EventJournal>(
    request: &RuntimeExecutionRequest,
    dispatch: &ExecutionDispatchReport,
    journal: &J,
) -> Result<(), JournalError> {
    let occurred_at = Utc::now();
    journal.append(EventJournalRecord {
        event_id: event_id("execution", "dispatch_succeeded", occurred_at),
        category: "execution".to_owned(),
        action: "dispatch_succeeded".to_owned(),
        source: request.action_source,
        severity: EventSeverity::Info,
        occurred_at,
        payload: json!({
            "mode": request.mode,
            "strategy_id": request.execution.strategy.metadata.strategy_id,
            "intent": intent_name(&request.execution.intent),
            "result_count": dispatch.results.len(),
            "result_types": dispatch.results.iter().map(dispatch_result_name).collect::<Vec<_>>(),
            "warnings": dispatch.warnings,
        }),
    })
}

fn journal_dispatch_skipped<J: EventJournal>(
    request: &RuntimeExecutionRequest,
    outcome: &RuntimeExecutionOutcome,
    journal: &J,
) -> Result<(), JournalError> {
    let occurred_at = Utc::now();
    journal.append(EventJournalRecord {
        event_id: event_id("execution", "dispatch_skipped", occurred_at),
        category: "execution".to_owned(),
        action: "dispatch_skipped".to_owned(),
        source: request.action_source,
        severity: EventSeverity::Warning,
        occurred_at,
        payload: json!({
            "mode": request.mode,
            "strategy_id": request.execution.strategy.metadata.strategy_id,
            "intent": intent_name(&request.execution.intent),
            "decision_status": outcome.risk.decision.status,
            "decision_reason": outcome.risk.decision.reason,
        }),
    })
}

fn journal_dispatch_failed<J: EventJournal>(
    request: &RuntimeExecutionRequest,
    error: &ExecutionDispatchError,
    journal: &J,
) -> Result<(), JournalError> {
    let occurred_at = Utc::now();
    journal.append(EventJournalRecord {
        event_id: event_id("execution", "dispatch_failed", occurred_at),
        category: "execution".to_owned(),
        action: "dispatch_failed".to_owned(),
        source: request.action_source,
        severity: EventSeverity::Error,
        occurred_at,
        payload: json!({
            "mode": request.mode,
            "strategy_id": request.execution.strategy.metadata.strategy_id,
            "intent": intent_name(&request.execution.intent),
            "error": error.to_string(),
        }),
    })
}

fn category_for_source(source: ActionSource) -> &'static str {
    match source {
        ActionSource::System => "strategy",
        ActionSource::Dashboard | ActionSource::Cli => "manual",
    }
}

fn dispatch_result_name(result: &ExecutionDispatchResult) -> &'static str {
    match result {
        ExecutionDispatchResult::OrderSubmitted { .. } => "order_submitted",
        ExecutionDispatchResult::PositionLiquidated { .. } => "position_liquidated",
        ExecutionDispatchResult::StrategyPaused { .. } => "strategy_paused",
    }
}

fn intent_name(intent: &tv_bot_core_types::ExecutionIntent) -> &'static str {
    match intent {
        tv_bot_core_types::ExecutionIntent::Enter { .. } => "enter",
        tv_bot_core_types::ExecutionIntent::Exit { .. } => "exit",
        tv_bot_core_types::ExecutionIntent::Flatten { .. } => "flatten",
        tv_bot_core_types::ExecutionIntent::CancelWorkingOrders { .. } => "cancel_working_orders",
        tv_bot_core_types::ExecutionIntent::ReducePosition { .. } => "reduce_position",
        tv_bot_core_types::ExecutionIntent::PauseStrategy { .. } => "pause_strategy",
    }
}

fn event_id(category: &str, action: &str, occurred_at: DateTime<Utc>) -> String {
    let timestamp = occurred_at.timestamp_nanos_opt().unwrap_or_default();
    format!("{category}-{action}-{timestamp}")
}
