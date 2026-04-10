use std::{env, error::Error, path::PathBuf};

use tv_bot_config::{AppConfig, StdEnvironment};
use tv_bot_runtime_kernel::RuntimeStateMachine;

fn main() -> Result<(), Box<dyn Error>> {
    let environment = StdEnvironment;
    let config_path = env::args().nth(1).map(PathBuf::from);
    let config = AppConfig::load(config_path.as_deref(), &environment)?;
    let runtime = RuntimeStateMachine::new(config.runtime.startup_mode);

    println!("tv-bot CLI scaffold ({:?})", runtime.current_mode());
    Ok(())
}
