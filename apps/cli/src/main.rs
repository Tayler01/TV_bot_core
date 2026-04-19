mod commands;
mod console;

use std::{error::Error, path::PathBuf, time::Duration};

use clap::Parser;
use commands::CliCommand;
use tv_bot_config::StdEnvironment;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let environment = StdEnvironment;
    let config = commands::load_config(cli.config.as_deref(), &environment)?;
    let base_url = cli
        .base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", config.control_api.http_bind));
    let client = commands::build_client()?;

    match cli.command {
        CliCommand::Console { refresh_seconds } => {
            console::run_console(
                &client,
                &base_url,
                Duration::from_secs(refresh_seconds.max(1)),
            )
            .await?
        }
        command => commands::run_command(command, &client, &base_url, cli.config.clone()).await?,
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CliReconnectDecision, CliShutdownDecision};

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
            "strategies/examples/micro_silver_elephant_tradovate_v1.md",
        ])
        .expect("cli should parse");

        match cli.command {
            CliCommand::Launch { strategy, .. } => {
                assert_eq!(
                    strategy,
                    Some(PathBuf::from(
                        "strategies/examples/micro_silver_elephant_tradovate_v1.md"
                    ))
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_console_command() {
        let cli = Cli::try_parse_from(["tv-bot-cli", "console", "--refresh-seconds", "3"])
            .expect("cli should parse");

        match cli.command {
            CliCommand::Console { refresh_seconds } => {
                assert_eq!(refresh_seconds, 3);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn runtime_mode_labels_are_stable() {
        assert_eq!(
            commands::runtime_mode_label(&tv_bot_core_types::RuntimeMode::Observation),
            "observation"
        );
        assert_eq!(
            commands::warmup_status_label(&tv_bot_core_types::WarmupStatus::NotLoaded),
            "not_loaded"
        );
        assert_eq!(commands::format_optional_percent(Some(12.5)), "12.5%");
        assert_eq!(commands::format_optional_bytes(Some(2_048)), "2.00 KiB");
        assert_eq!(
            commands::chart_snapshot_limit(tv_bot_core_types::Timeframe::OneMinute, 1_440),
            132
        );
        assert_eq!(
            commands::chart_snapshot_limit(tv_bot_core_types::Timeframe::OneSecond, 1_440),
            7_200
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
