mod history;
mod host;
mod operator;

use std::{env, error::Error, path::PathBuf};

use tracing_subscriber::EnvFilter;
use tv_bot_config::{AppConfig, StdEnvironment};
use tv_bot_runtime_kernel::RuntimeStateMachine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let environment = StdEnvironment;
    let config_path = env::args().nth(1).map(PathBuf::from);
    let config = AppConfig::load(config_path.as_deref(), &environment)?;

    configure_tracing(&config.logging.level, config.logging.json);

    let runtime = RuntimeStateMachine::new(config.runtime.startup_mode.clone());
    host::run_runtime_host(config_path, config, runtime)
        .await
        .map_err(|error| -> Box<dyn Error> { Box::new(error) })
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
