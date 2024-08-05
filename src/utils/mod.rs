use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

use crate::utils::constants::DEFAULT_LOG_LEVEL;

pub mod encoding;
pub mod error;
pub mod prelude;
pub mod type_impls;

pub mod components;
pub mod config;
pub mod constants;
mod nbt_impls;

/// Sets up the logger. Needs to be run before anything else in order for logging to run end.
pub fn setup_logger() {
    let trace_level = std::env::args()
        .find(|arg| arg.starts_with("--log="))
        .map(|arg| arg.replace("--log=", ""));

    let mut trace_level: &str = trace_level.as_deref().unwrap_or("");
    if trace_level.is_empty() {
        eprintln!(
            "No log level specified, using default: {}",
            DEFAULT_LOG_LEVEL
        );
        trace_level = DEFAULT_LOG_LEVEL;
    }

    let _trace_level = match trace_level.trim().parse::<tracing::Level>() {
        Ok(level) => level,
        Err(_) => {
            eprintln!("Invalid log level: {}", trace_level);
            eprintln!("Possible values: trace, debug, info, warn, error");
            std::process::exit(1);
        }
    };

    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::OFF.into())
        .from_env()
        .unwrap()
        .add_directive("ferrumc2_0=debug".parse().unwrap());

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        //.with_max_level(trace_level)
        .compact()
        .init();
}
