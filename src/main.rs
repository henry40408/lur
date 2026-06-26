use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use lur::runtime::{DEFAULT_MEMORY_LIMIT_BYTES, RunError, Runtime, RuntimeConfig};

/// `lur` — run a sandboxed Lua (Luau) script.
#[derive(Parser)]
#[command(name = "lur", version, about)]
struct Cli {
    /// Path to the script to run.
    script: PathBuf,

    /// Wall-clock time limit in milliseconds (no limit if omitted).
    #[arg(long, value_name = "MS")]
    timeout_ms: Option<u64>,

    /// Memory cap in bytes (0 means unlimited).
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_MEMORY_LIMIT_BYTES)]
    max_memory: usize,

    /// Arguments passed to the script (exposed as `lur.args`).
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    script_args: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let source = match std::fs::read_to_string(&cli.script) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lur: cannot read {}: {e}", cli.script.display());
            return ExitCode::from(2);
        }
    };

    let config = RuntimeConfig {
        memory_limit: cli.max_memory,
        args: cli.script_args,
    };
    let rt = match Runtime::with_config(config) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::FAILURE;
        }
    };

    let timeout = cli.timeout_ms.map(Duration::from_millis);
    match rt.run_to_exit_code(&source, timeout) {
        Ok(code) => ExitCode::from(code as u8),
        Err(RunError::Timeout) => {
            eprintln!("lur: script exceeded its time limit");
            ExitCode::from(124)
        }
        Err(RunError::OutOfMemory) => {
            eprintln!("lur: script exceeded its memory limit");
            ExitCode::from(137)
        }
        Err(RunError::Script(e)) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
        Err(RunError::Init(e)) => {
            eprintln!("lur: {e}");
            ExitCode::FAILURE
        }
    }
}
