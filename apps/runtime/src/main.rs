use std::{env, error::Error, path::PathBuf};

use tracing_subscriber::EnvFilter;
use tv_bot_config::{AppConfig, StdEnvironment};
use tv_bot_runtime_kernel::RuntimeStateMachine;

fn main() -> Result<(), Box<dyn Error>> {
    let environment = StdEnvironment;
    let config_path = env::args().nth(1).map(PathBuf::from);
    let config = AppConfig::load(config_path.as_deref(), &environment)?;

    configure_tracing(&config.logging.level, config.logging.json);

    let runtime = RuntimeStateMachine::new(config.runtime.startup_mode.clone());

    tracing::info!(
        mode = ?runtime.current_mode(),
        http_bind = %config.control_api.http_bind,
        websocket_bind = %config.control_api.websocket_bind,
        "runtime scaffold initialized"
    );

    Ok(())
}

fn configure_tracing(level: &str, json: bool) {
    let env_filter = EnvFilter::new(level.to_owned());

    if json {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
}
