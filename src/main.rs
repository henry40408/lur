use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use lur::policy::Policy;
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

    /// Grant full filesystem access for this run.
    #[arg(short = 'A', long = "allow-all")]
    allow_all: bool,

    /// Add a readable path root (repeatable).
    #[arg(long = "allow-fs-read", value_name = "PATH")]
    allow_fs_read: Vec<PathBuf>,

    /// Add a writable path root (repeatable).
    #[arg(long = "allow-fs-write", value_name = "PATH")]
    allow_fs_write: Vec<PathBuf>,

    /// Add a path to both the read and write allowlists (repeatable).
    #[arg(long = "allow-fs", value_name = "PATH")]
    allow_fs: Vec<PathBuf>,

    /// Add an environment-variable name to the allowlist (repeatable).
    #[arg(long = "allow-env", value_name = "NAME")]
    allow_env: Vec<String>,

    /// Arguments passed to the script (exposed as `lur.args`).
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    script_args: Vec<String>,
}

/// Resolve the capability policy from the CLI flags. Roots are canonicalized
/// (and must exist) by [`Policy::from_roots`]; `-A` grants the whole tree.
fn build_policy(cli: &Cli) -> Result<Policy, String> {
    if cli.allow_all {
        let root = vec![PathBuf::from("/")];
        return Ok(Policy::from_roots(&root, &root)
            .map_err(|e| e.to_string())?
            .allow_all_env());
    }
    let mut read = cli.allow_fs_read.clone();
    let mut write = cli.allow_fs_write.clone();
    for p in &cli.allow_fs {
        read.push(p.clone());
        write.push(p.clone());
    }
    Ok(Policy::from_roots(&read, &write)
        .map_err(|e| format!("invalid --allow-fs path: {e}"))?
        .with_env(cli.allow_env.clone()))
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

    let policy = match build_policy(&cli) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::from(2);
        }
    };

    let config = RuntimeConfig {
        memory_limit: cli.max_memory,
        args: cli.script_args,
        policy,
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
        Err(RunError::AsyncRuntime(e)) => {
            eprintln!("lur: {e}");
            ExitCode::FAILURE
        }
    }
}
