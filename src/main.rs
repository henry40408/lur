use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use lur::policy::Policy;
use lur::runtime::{
    DEFAULT_MAX_HTTP_BODY_BYTES, DEFAULT_MEMORY_LIMIT_BYTES, RunError, Runtime, RuntimeConfig,
};
use lur::serve::Server;

/// Capability and limit flags shared by one-shot and server mode.
#[derive(clap::Args)]
struct CommonFlags {
    /// Memory cap in bytes (0 means unlimited).
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_MEMORY_LIMIT_BYTES)]
    max_memory: usize,

    /// Cap on a buffered lur.http response body, in bytes.
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_MAX_HTTP_BODY_BYTES)]
    max_http_body: usize,

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

    /// Add a network host (`host` or `host:port`) to the allowlist (repeatable).
    #[arg(long = "allow-net", value_name = "HOST")]
    allow_net: Vec<String>,

    /// Permit connections to loopback/private/link-local IPs (off by default).
    #[arg(long = "allow-private")]
    allow_private: bool,

    /// SQLite database path for lur.db / lur.kv.
    #[arg(long = "db", value_name = "PATH")]
    db: Option<PathBuf>,
}

/// `lur` — run a sandboxed Lua (Luau) script.
#[derive(Parser)]
#[command(name = "lur", version, about)]
struct Cli {
    /// Path to the script to run.
    script: PathBuf,

    /// Wall-clock time limit in milliseconds (no limit if omitted).
    #[arg(long, value_name = "MS")]
    timeout_ms: Option<u64>,

    #[command(flatten)]
    common: CommonFlags,

    /// Arguments passed to the script (exposed as `lur.args`).
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    script_args: Vec<String>,
}

/// `lur serve` — run `app.lua` as a long-running HTTP server.
#[derive(Parser)]
#[command(name = "lur serve", about = "Run app.lua as a long-running server")]
struct ServeCli {
    /// Path to the server application script.
    app: PathBuf,

    /// Address to bind the HTTP listener to.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    #[command(flatten)]
    common: CommonFlags,
}

/// Resolve the capability policy from the shared flags. Roots are canonicalized
/// (and must exist) by [`Policy::from_roots`]; `-A` grants the whole tree.
fn build_policy(flags: &CommonFlags) -> Result<Policy, String> {
    if flags.allow_all {
        let root = vec![PathBuf::from("/")];
        return Ok(Policy::from_roots(&root, &root)
            .map_err(|e| e.to_string())?
            .allow_all_env()
            .with_net(vec!["*".to_string()])
            .allow_private());
    }
    let mut read = flags.allow_fs_read.clone();
    let mut write = flags.allow_fs_write.clone();
    for p in &flags.allow_fs {
        read.push(p.clone());
        write.push(p.clone());
    }
    let mut policy = Policy::from_roots(&read, &write)
        .map_err(|e| format!("invalid --allow-fs path: {e}"))?
        .with_env(flags.allow_env.clone())
        .with_net(flags.allow_net.clone());
    if flags.allow_private {
        policy = policy.allow_private();
    }
    Ok(policy)
}

/// Build a [`RuntimeConfig`] from the shared flags (policy resolved, args set
/// by the caller).
fn build_config(flags: &CommonFlags, args: Vec<String>) -> Result<RuntimeConfig, String> {
    let policy = Arc::new(build_policy(flags)?);
    Ok(RuntimeConfig {
        memory_limit: flags.max_memory,
        args,
        policy,
        max_http_body: flags.max_http_body,
        db_path: flags.db.clone(),
    })
}

fn main() -> ExitCode {
    // `lur serve ...` routes to server mode; everything else is one-shot. Peeked
    // manually so the one-shot `lur script.lua [args]` grammar stays untouched.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("serve") {
        let serve_argv = std::iter::once(argv[0].clone()).chain(argv.iter().skip(2).cloned());
        return run_serve(ServeCli::parse_from(serve_argv));
    }
    run_one_shot(Cli::parse())
}

/// Load `app.lua` and serve it forever (server mode).
fn run_serve(cli: ServeCli) -> ExitCode {
    let source = match std::fs::read_to_string(&cli.app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lur: cannot read {}: {e}", cli.app.display());
            return ExitCode::from(2);
        }
    };

    let config = match build_config(&cli.common, Vec::new()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::from(2);
        }
    };

    let server = match Server::load(&source, config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::FAILURE;
        }
    };

    match server.run(cli.bind) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lur: server error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run a single script to completion (one-shot mode).
fn run_one_shot(cli: Cli) -> ExitCode {
    let source = match std::fs::read_to_string(&cli.script) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lur: cannot read {}: {e}", cli.script.display());
            return ExitCode::from(2);
        }
    };

    let config = match build_config(&cli.common, cli.script_args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::from(2);
        }
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
