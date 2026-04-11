use std::{
    env,
    error::Error,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use clap::{Parser, Subcommand, ValueEnum};
use reqwest::Client;
use tokio::{process::Command, time::sleep};
use tv_bot_config::{AppConfig, StdEnvironment};
use tv_bot_control_api::{
    ManualCommandSource, RuntimeHistorySnapshot, RuntimeLifecycleCommand, RuntimeLifecycleRequest,
    RuntimeLifecycleResponse, RuntimeReadinessSnapshot, RuntimeReconnectDecision,
    RuntimeShutdownDecision, RuntimeStatusSnapshot,
};

#[derive(Parser, Debug)]
#[command(name = "tv-bot-cli")]
#[command(about = "Local control CLI for the TV futures bot runtime")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    base_url: Option<String>,
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    Launch {
        #[arg(long)]
        runtime_bin: Option<PathBuf>,
        #[arg(long)]
        strategy: Option<PathBuf>,
    },
    Status,
    Readiness,
    History,
    Load {
        path: PathBuf,
    },
    Warmup {
        #[command(subcommand)]
        command: WarmupCommand,
    },
    Start {
        mode: CliRuntimeMode,
        #[arg(long)]
        yes: bool,
    },
    Pause,
    Resume,
    Arm {
        #[arg(long)]
        allow_override: bool,
        #[arg(long)]
        yes: bool,
    },
    Disarm,
    ReconnectReview {
        decision: CliReconnectDecision,
        #[arg(long)]
        contract_id: Option<i64>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    Shutdown {
        decision: CliShutdownDecision,
        #[arg(long)]
        contract_id: Option<i64>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    Flatten {
        contract_id: i64,
        #[arg(long, default_value = "manual flatten")]
        reason: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WarmupCommand {
    Start,
    Ready,
    Fail {
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliRuntimeMode {
    Paper,
    Live,
    Observation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliReconnectDecision {
    ClosePosition,
    LeaveBrokerProtected,
    ReattachBotManagement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliShutdownDecision {
    FlattenFirst,
    LeaveBrokerProtected,
}

impl From<CliRuntimeMode> for tv_bot_core_types::RuntimeMode {
    fn from(value: CliRuntimeMode) -> Self {
        match value {
            CliRuntimeMode::Paper => Self::Paper,
            CliRuntimeMode::Live => Self::Live,
            CliRuntimeMode::Observation => Self::Observation,
        }
    }
}

impl From<CliReconnectDecision> for RuntimeReconnectDecision {
    fn from(value: CliReconnectDecision) -> Self {
        match value {
            CliReconnectDecision::ClosePosition => Self::ClosePosition,
            CliReconnectDecision::LeaveBrokerProtected => Self::LeaveBrokerProtected,
            CliReconnectDecision::ReattachBotManagement => Self::ReattachBotManagement,
        }
    }
}

impl From<CliShutdownDecision> for RuntimeShutdownDecision {
    fn from(value: CliShutdownDecision) -> Self {
        match value {
            CliShutdownDecision::FlattenFirst => Self::FlattenFirst,
            CliShutdownDecision::LeaveBrokerProtected => Self::LeaveBrokerProtected,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let environment = StdEnvironment;
    let config = load_config(cli.config.as_deref(), &environment)?;
    let base_url = cli
        .base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", config.control_api.http_bind));
    let client = Client::builder().timeout(Duration::from_secs(15)).build()?;

    match cli.command {
        CliCommand::Launch {
            runtime_bin,
            strategy,
        } => {
            launch_runtime(
                &client,
                &base_url,
                runtime_bin,
                cli.config.clone(),
                strategy,
            )
            .await?
        }
        CliCommand::Status => {
            let status = fetch_status(&client, &base_url).await?;
            print_status(&status);
        }
        CliCommand::Readiness => {
            let readiness = fetch_readiness(&client, &base_url).await?;
            print_readiness(&readiness);
        }
        CliCommand::History => {
            let history = fetch_history(&client, &base_url).await?;
            print_history(&history);
        }
        CliCommand::Load { path } => {
            let response = send_runtime_command(
                &client,
                &base_url,
                RuntimeLifecycleCommand::LoadStrategy { path },
            )
            .await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Warmup { command } => {
            let command = match command {
                WarmupCommand::Start => RuntimeLifecycleCommand::StartWarmup,
                WarmupCommand::Ready => RuntimeLifecycleCommand::MarkWarmupReady,
                WarmupCommand::Fail { reason } => {
                    RuntimeLifecycleCommand::MarkWarmupFailed { reason }
                }
            };
            let response = send_runtime_command(&client, &base_url, command).await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Start { mode, yes } => {
            let mode: tv_bot_core_types::RuntimeMode = mode.into();
            if mode == tv_bot_core_types::RuntimeMode::Live {
                confirm(
                    yes,
                    "Switch runtime mode to live? This makes live arming possible later.",
                )?;
            }
            let response = send_runtime_command(
                &client,
                &base_url,
                RuntimeLifecycleCommand::SetMode { mode },
            )
            .await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Pause => {
            let response =
                send_runtime_command(&client, &base_url, RuntimeLifecycleCommand::Pause).await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Resume => {
            let response =
                send_runtime_command(&client, &base_url, RuntimeLifecycleCommand::Resume).await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Arm {
            allow_override,
            yes,
        } => {
            let status = fetch_status(&client, &base_url).await?;
            if status.mode == tv_bot_core_types::RuntimeMode::Live {
                confirm(yes, "Arm the runtime while in live mode?")?;
            }
            if allow_override {
                confirm(
                    yes,
                    "Arm the runtime with a temporary override? Review the readiness report first.",
                )?;
            }
            let response = send_runtime_command(
                &client,
                &base_url,
                RuntimeLifecycleCommand::Arm { allow_override },
            )
            .await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Disarm => {
            let response =
                send_runtime_command(&client, &base_url, RuntimeLifecycleCommand::Disarm).await?;
            print_lifecycle_response(&response);
        }
        CliCommand::ReconnectReview {
            decision,
            contract_id,
            reason,
            yes,
        } => {
            confirm(yes, reconnect_review_prompt(decision))?;
            let response = send_runtime_command(
                &client,
                &base_url,
                RuntimeLifecycleCommand::ResolveReconnectReview {
                    decision: decision.into(),
                    contract_id,
                    reason,
                },
            )
            .await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Shutdown {
            decision,
            contract_id,
            reason,
            yes,
        } => {
            confirm(yes, shutdown_prompt(decision))?;
            let response = send_runtime_command(
                &client,
                &base_url,
                RuntimeLifecycleCommand::Shutdown {
                    decision: decision.into(),
                    contract_id,
                    reason,
                },
            )
            .await?;
            print_lifecycle_response(&response);
        }
        CliCommand::Flatten {
            contract_id,
            reason,
            yes,
        } => {
            confirm(
                yes,
                "Submit a flatten command for the specified contract id through the audited execution path?",
            )?;
            let response = send_runtime_command(
                &client,
                &base_url,
                RuntimeLifecycleCommand::Flatten {
                    contract_id,
                    reason,
                },
            )
            .await?;
            print_lifecycle_response(&response);
        }
    }

    Ok(())
}

async fn launch_runtime(
    client: &Client,
    base_url: &str,
    runtime_bin: Option<PathBuf>,
    config_path: Option<PathBuf>,
    strategy: Option<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let runtime_binary = resolve_runtime_binary(runtime_bin)?;
    let mut command = Command::new(&runtime_binary);
    if let Some(config_path) = config_path {
        command.arg(config_path);
    }
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let child = command.spawn()?;
    println!(
        "Launched runtime `{}` (pid: {}).",
        runtime_binary.display(),
        child.id().unwrap_or_default()
    );

    if let Some(strategy_path) = strategy {
        wait_for_runtime(client, base_url).await?;
        let response = send_runtime_command(
            client,
            base_url,
            RuntimeLifecycleCommand::LoadStrategy {
                path: strategy_path,
            },
        )
        .await?;
        print_lifecycle_response(&response);
    }

    Ok(())
}

async fn wait_for_runtime(client: &Client, base_url: &str) -> Result<(), Box<dyn Error>> {
    for _ in 0..40 {
        if client
            .get(format!("{base_url}/health"))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return Ok(());
        }

        sleep(Duration::from_millis(500)).await;
    }

    Err("runtime host did not become ready in time".into())
}

async fn fetch_status(
    client: &Client,
    base_url: &str,
) -> Result<RuntimeStatusSnapshot, Box<dyn Error>> {
    let response = client.get(format!("{base_url}/status")).send().await?;
    Ok(response.error_for_status()?.json().await?)
}

async fn fetch_readiness(
    client: &Client,
    base_url: &str,
) -> Result<RuntimeReadinessSnapshot, Box<dyn Error>> {
    let response = client.get(format!("{base_url}/readiness")).send().await?;
    Ok(response.error_for_status()?.json().await?)
}

async fn fetch_history(
    client: &Client,
    base_url: &str,
) -> Result<RuntimeHistorySnapshot, Box<dyn Error>> {
    let response = client.get(format!("{base_url}/history")).send().await?;
    Ok(response.error_for_status()?.json().await?)
}

async fn send_runtime_command(
    client: &Client,
    base_url: &str,
    command: RuntimeLifecycleCommand,
) -> Result<RuntimeLifecycleResponse, Box<dyn Error>> {
    let response = client
        .post(format!("{base_url}/runtime/commands"))
        .json(&RuntimeLifecycleRequest {
            source: ManualCommandSource::Cli,
            command,
        })
        .send()
        .await?;

    Ok(response.json().await?)
}

fn print_status(status: &RuntimeStatusSnapshot) {
    println!("Mode: {}", runtime_mode_label(&status.mode));
    println!("Arm State: {}", arm_state_label(&status.arm_state));
    println!("Warmup: {}", warmup_status_label(&status.warmup_status));
    println!(
        "Strategy: {}",
        status
            .current_strategy
            .as_ref()
            .map(|strategy| {
                format!(
                    "{} ({}) from {}",
                    strategy.name,
                    strategy.strategy_id,
                    strategy.path.display()
                )
            })
            .unwrap_or_else(|| "none loaded".to_owned())
    );
    println!(
        "Account: {}",
        status
            .current_account_name
            .as_deref()
            .unwrap_or("none selected")
    );
    if let Some(broker_status) = &status.broker_status {
        println!(
            "Broker: {:?} / {:?} ({:?})",
            broker_status.connection_state, broker_status.sync_state, broker_status.health
        );
    }
    println!(
        "Reconnect Review: required={} open_positions={} working_orders={} last_decision={:?}",
        status.reconnect_review.required,
        status.reconnect_review.open_position_count,
        status.reconnect_review.working_order_count,
        status.reconnect_review.last_decision
    );
    if let Some(reason) = &status.reconnect_review.reason {
        println!("Reconnect Review Reason: {reason}");
    }
    if let Some(market_data_status) = &status.market_data_status {
        println!(
            "Market Data: {:?} / {:?} (warmup: {:?}, trade_ready: {})",
            market_data_status.session.market_data.connection_state,
            market_data_status.session.market_data.health,
            market_data_status.session.market_data.warmup.status,
            market_data_status.trade_ready
        );
    } else if let Some(detail) = &status.market_data_detail {
        println!("Market Data: unavailable ({detail})");
    }
    println!(
        "Storage: {:?} / {} / durable={} / fallback_activated={} ({})",
        status.storage_status.mode,
        status.storage_status.active_backend,
        status.storage_status.durable,
        status.storage_status.fallback_activated,
        status.storage_status.detail
    );
    println!(
        "Journal: {} ({})",
        status.journal_status.backend, status.journal_status.detail
    );
    if let Some(system_health) = &status.system_health {
        println!(
            "System Health: reconnects={} errors={} db_write_latency_ms={:?} queue_lag_ms={:?} feed_degraded={}",
            system_health.reconnect_count,
            system_health.error_count,
            system_health.db_write_latency_ms,
            system_health.queue_lag_ms,
            system_health.feed_degraded
        );
    }
    if let Some(latency) = &status.latest_trade_latency {
        println!(
            "Latest Trade Latency: action={} ack_ms={:?} fill_ms={:?} sync_ms={:?} records={}",
            latency.action_id,
            latency.latency.broker_ack_latency_ms,
            latency.latency.end_to_end_fill_latency_ms,
            latency.latency.end_to_end_sync_latency_ms,
            status.recorded_trade_latency_count
        );
    }
    println!(
        "Shutdown Review: pending_signal={} blocked={} awaiting_flatten={} open_positions={} broker_protected={} decision={:?}",
        status.shutdown_review.pending_signal,
        status.shutdown_review.blocked,
        status.shutdown_review.awaiting_flatten,
        status.shutdown_review.open_position_count,
        status.shutdown_review.all_positions_broker_protected,
        status.shutdown_review.decision
    );
    if let Some(reason) = &status.shutdown_review.reason {
        println!("Shutdown Review Reason: {reason}");
    }
    println!(
        "Dispatch: {} ({})",
        if status.command_dispatch_ready {
            "ready"
        } else {
            "unavailable"
        },
        status.command_dispatch_detail
    );
    if let Some(mapping) = &status.instrument_mapping {
        println!("Symbol Mapping: {}", mapping.summary);
    } else if let Some(error) = &status.instrument_resolution_error {
        println!("Symbol Mapping: unresolved ({error})");
    }
}

fn print_readiness(readiness: &RuntimeReadinessSnapshot) {
    print_status(&readiness.status);
    println!(
        "Readiness: {}",
        if readiness.report.is_ready_without_override() {
            "ready"
        } else if readiness.report.hard_override_required {
            "override required"
        } else {
            "blocked"
        }
    );
    println!("Risk Summary: {}", readiness.report.risk_summary);
    for check in &readiness.report.checks {
        println!(
            "- [{}] {}: {}",
            readiness_check_label(check.status),
            check.name,
            check.message
        );
    }
}

fn print_history(history: &RuntimeHistorySnapshot) {
    let projection = &history.projection;
    println!(
        "Runs: {} total / {} active",
        projection.total_strategy_run_records,
        projection.active_run_ids.len()
    );
    println!(
        "Orders: {} total / {} working",
        projection.total_order_records,
        projection.working_order_ids.len()
    );
    println!(
        "Fills: {} total | Positions: {} total / {} open",
        projection.total_fill_records,
        projection.total_position_records,
        projection.open_position_symbols.len()
    );
    println!(
        "Trades: {} total / {} open / {} closed",
        projection.total_trade_summary_records,
        projection.open_trade_ids.len(),
        projection.closed_trade_count
    );
    println!(
        "PnL: gross={} net={} fees={} commissions={} slippage={}",
        projection.closed_trade_gross_pnl,
        projection.closed_trade_net_pnl,
        projection.closed_trade_fees,
        projection.closed_trade_commissions,
        projection.closed_trade_slippage
    );
    if let Some(run) = &projection.latest_run {
        println!(
            "Latest Run: {} / {:?} / {:?}",
            run.strategy_id, run.mode, run.status
        );
    }
    if let Some(order) = &projection.latest_order {
        println!(
            "Latest Order: {} {} {:?} x{} {:?}",
            order.broker_order_id, order.symbol, order.side, order.quantity, order.status
        );
    }
    if let Some(fill) = &projection.latest_fill {
        println!(
            "Latest Fill: {} {} {:?} x{} @ {}",
            fill.fill_id, fill.symbol, fill.side, fill.quantity, fill.price
        );
    }
    if let Some(position) = &projection.latest_position {
        println!(
            "Latest Position: {} qty={} avg={}",
            position.symbol,
            position.quantity,
            position
                .average_price
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".to_owned())
        );
    }
}

fn print_lifecycle_response(response: &RuntimeLifecycleResponse) {
    println!("{}", response.message);
    println!(
        "Result: {}",
        match response.status_code {
            tv_bot_control_api::HttpStatusCode::Ok => "ok",
            tv_bot_control_api::HttpStatusCode::Conflict => "conflict",
            tv_bot_control_api::HttpStatusCode::PreconditionRequired => {
                "precondition_required"
            }
            tv_bot_control_api::HttpStatusCode::InternalServerError => "internal_server_error",
        }
    );
    if let Some(command_result) = &response.command_result {
        println!(
            "Dispatch: {} | Risk: {:?} | Reason: {}",
            if command_result.dispatch_performed {
                "performed"
            } else {
                "not performed"
            },
            command_result.risk_status,
            command_result.reason
        );
        if !command_result.warnings.is_empty() {
            println!("Warnings:");
            for warning in &command_result.warnings {
                println!("- {warning}");
            }
        }
    }
    println!(
        "State: mode={} arm={} warmup={}",
        runtime_mode_label(&response.status.mode),
        arm_state_label(&response.status.arm_state),
        warmup_status_label(&response.status.warmup_status)
    );
}

fn confirm(yes: bool, prompt: &str) -> Result<(), Box<dyn Error>> {
    if yes {
        return Ok(());
    }

    print!("{prompt} [y/N]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    if matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err("command cancelled".into())
    }
}

fn resolve_runtime_binary(runtime_bin: Option<PathBuf>) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(runtime_bin) = runtime_bin {
        return Ok(runtime_bin);
    }

    let current_exe = env::current_exe()?;
    let Some(parent) = current_exe.parent() else {
        return Err("unable to determine CLI executable directory".into());
    };

    let runtime_name = if env::consts::EXE_EXTENSION.is_empty() {
        "tv-bot-runtime".to_owned()
    } else {
        format!("tv-bot-runtime.{}", env::consts::EXE_EXTENSION)
    };
    let candidate = parent.join(runtime_name);

    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(format!(
            "could not find runtime binary next to CLI executable: {}",
            candidate.display()
        )
        .into())
    }
}

fn load_config(
    path: Option<&Path>,
    environment: &StdEnvironment,
) -> Result<AppConfig, Box<dyn Error>> {
    Ok(AppConfig::load(path, environment)?)
}

fn runtime_mode_label(mode: &tv_bot_core_types::RuntimeMode) -> &'static str {
    match mode {
        tv_bot_core_types::RuntimeMode::Paper => "paper",
        tv_bot_core_types::RuntimeMode::Live => "live",
        tv_bot_core_types::RuntimeMode::Observation => "observation",
        tv_bot_core_types::RuntimeMode::Paused => "paused",
    }
}

fn arm_state_label(state: &tv_bot_core_types::ArmState) -> &'static str {
    match state {
        tv_bot_core_types::ArmState::Disarmed => "disarmed",
        tv_bot_core_types::ArmState::Armed => "armed",
    }
}

fn warmup_status_label(status: &tv_bot_core_types::WarmupStatus) -> &'static str {
    match status {
        tv_bot_core_types::WarmupStatus::NotLoaded => "not_loaded",
        tv_bot_core_types::WarmupStatus::Loaded => "loaded",
        tv_bot_core_types::WarmupStatus::Warming => "warming",
        tv_bot_core_types::WarmupStatus::Ready => "ready",
        tv_bot_core_types::WarmupStatus::Failed => "failed",
    }
}

fn readiness_check_label(status: tv_bot_core_types::ReadinessCheckStatus) -> &'static str {
    match status {
        tv_bot_core_types::ReadinessCheckStatus::Pass => "pass",
        tv_bot_core_types::ReadinessCheckStatus::Warning => "warning",
        tv_bot_core_types::ReadinessCheckStatus::Blocking => "blocking",
    }
}

fn reconnect_review_prompt(decision: CliReconnectDecision) -> &'static str {
    match decision {
        CliReconnectDecision::ClosePosition => {
            "Resolve reconnect review by closing the active broker position?"
        }
        CliReconnectDecision::LeaveBrokerProtected => {
            "Resolve reconnect review by leaving the broker-protected position in place?"
        }
        CliReconnectDecision::ReattachBotManagement => {
            "Resolve reconnect review by reattaching bot management to the active position?"
        }
    }
}

fn shutdown_prompt(decision: CliShutdownDecision) -> &'static str {
    match decision {
        CliShutdownDecision::FlattenFirst => {
            "Approve shutdown after flattening the active broker position?"
        }
        CliShutdownDecision::LeaveBrokerProtected => {
            "Approve shutdown and leave the broker-protected position in place?"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_load_command() {
        let cli =
            Cli::try_parse_from(["tv-bot-cli", "load", "strategy.md"]).expect("cli should parse");

        match cli.command {
            CliCommand::Load { path } => {
                assert_eq!(path, PathBuf::from("strategy.md"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_launch_with_strategy() {
        let cli = Cli::try_parse_from([
            "tv-bot-cli",
            "launch",
            "--strategy",
            "strategies/examples/gc_momentum_fade_v1.md",
        ])
        .expect("cli should parse");

        match cli.command {
            CliCommand::Launch { strategy, .. } => {
                assert_eq!(
                    strategy,
                    Some(PathBuf::from("strategies/examples/gc_momentum_fade_v1.md"))
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn runtime_mode_labels_are_stable() {
        assert_eq!(
            runtime_mode_label(&tv_bot_core_types::RuntimeMode::Observation),
            "observation"
        );
        assert_eq!(
            warmup_status_label(&tv_bot_core_types::WarmupStatus::NotLoaded),
            "not_loaded"
        );
    }

    #[test]
    fn parses_reconnect_review_command() {
        let cli = Cli::try_parse_from([
            "tv-bot-cli",
            "reconnect-review",
            "leave-broker-protected",
            "--reason",
            "keep brackets live",
        ])
        .expect("cli should parse");

        match cli.command {
            CliCommand::ReconnectReview {
                decision, reason, ..
            } => {
                assert_eq!(decision, CliReconnectDecision::LeaveBrokerProtected);
                assert_eq!(reason.as_deref(), Some("keep brackets live"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_shutdown_command() {
        let cli = Cli::try_parse_from([
            "tv-bot-cli",
            "shutdown",
            "flatten-first",
            "--contract-id",
            "4444",
            "--yes",
        ])
        .expect("cli should parse");

        match cli.command {
            CliCommand::Shutdown {
                decision,
                contract_id,
                yes,
                ..
            } => {
                assert_eq!(decision, CliShutdownDecision::FlattenFirst);
                assert_eq!(contract_id, Some(4444));
                assert!(yes);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
