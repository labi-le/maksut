use crate::*;
use tracing_subscriber::filter::LevelFilter;

/// Installs the global logger. The formatter prints the module target together
/// with `file:line`, and `#[instrument]` spans add the function name, so a line
/// names the call site that produced it.
pub(crate) fn init() -> Result<(), String> {
    let level = match config().log.level.as_str() {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "info" => LevelFilter::INFO,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        "off" => LevelFilter::OFF,
        other => {
            return Err(format!(
                "log.level must be one of trace, debug, info, warn, error, off (got {other})"
            ))
        }
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|_| "logging subscriber was already installed".to_string())
}
