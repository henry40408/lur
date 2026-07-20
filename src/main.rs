use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use lur::config::{Config, Profile, expand_tilde};
use lur::policy::Policy;
use lur::runtime::{
    DEFAULT_MAX_HTTP_BODY_BYTES, DEFAULT_MEMORY_LIMIT_BYTES, DEFAULT_SHUTDOWN_GRACE_MS, RunError,
    Runtime, RuntimeConfig,
};
use lur::serve::Server;
use lur::units::{parse_duration, parse_size};
use tracing_subscriber::{
    EnvFilter, Layer as _, fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt,
};

/// Log output format for `lur serve`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum LogFormat {
    #[default]
    Full,
    Compact,
    Pretty,
    Json,
}

/// Install the global `tracing` subscriber for server mode. One-shot mode does
/// not call this and keeps plain `eprintln!` for user-facing errors.
fn init_tracing(format: LogFormat) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error,lur=info"));
    let span_events = env_filter.max_level_hint().map_or(FmtSpan::CLOSE, |l| {
        if l >= tracing::Level::DEBUG {
            FmtSpan::CLOSE
        } else {
            FmtSpan::NONE
        }
    });
    let use_ansi = std::env::var_os("NO_COLOR").is_none();
    let layer = tracing_subscriber::fmt::layer()
        .with_span_events(span_events)
        .with_ansi(use_ansi);
    let layer = match format {
        LogFormat::Full => layer.with_filter(env_filter).boxed(),
        LogFormat::Compact => layer.compact().with_filter(env_filter).boxed(),
        LogFormat::Pretty => layer.pretty().with_filter(env_filter).boxed(),
        LogFormat::Json => layer.json().with_filter(env_filter).boxed(),
    };
    tracing_subscriber::registry().with(layer).init();
}

/// Capability and limit flags shared by one-shot and server mode.
#[derive(clap::Args)]
struct CommonFlags {
    /// Memory cap (e.g. `128m`, `512k`; 0 means unlimited).
    #[arg(long = "memory", value_name = "SIZE", default_value_t = DEFAULT_MEMORY_LIMIT_BYTES, value_parser = parse_size)]
    memory: usize,

    /// Cap on a buffered lur.http response body (e.g. `16m`).
    #[arg(long, value_name = "SIZE", default_value_t = DEFAULT_MAX_HTTP_BODY_BYTES, value_parser = parse_size)]
    max_http_body: usize,

    /// Grant full filesystem access for this run.
    #[arg(short = 'A', long = "allow-all")]
    allow_all: bool,

    /// Select the strict profile — deny by default (the shipped default).
    #[arg(long, conflicts_with = "loose")]
    strict: bool,

    /// Select the loose profile — permissive (full access, like `-A`).
    #[arg(long)]
    loose: bool,

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

    /// `SQLite` database path for lur.db / lur.kv.
    #[arg(long = "db", value_name = "PATH")]
    db: Option<PathBuf>,

    /// Cap on concurrently in-flight lur.async.* tasks per VM (unbounded if omitted).
    #[arg(long = "max-concurrency", value_name = "N")]
    max_concurrency: Option<usize>,

    /// Load a policy config file (default: `$XDG_CONFIG_HOME/lur/config`, else
    /// `~/.config/lur/config`).
    #[arg(long = "config", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Ignore the user config entirely → pure shipped strict, zero grants.
    #[arg(long = "no-config", conflicts_with = "config")]
    no_config: bool,
}

/// `lur` — run a sandboxed Lua (Luau) script. Run `lur docs` to print the embedded usage guide.
#[derive(Parser)]
#[command(name = "lur", version = env!("GIT_VERSION"), about)]
struct Cli {
    /// Path to the script to run.
    script: PathBuf,

    /// Wall-clock time limit (e.g. `5s`, `500ms`, `2m`; no limit if omitted).
    #[arg(long, value_name = "DUR", value_parser = parse_duration)]
    timeout: Option<Duration>,

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

    /// Address to bind the HTTP listener to. Defaults to loopback so a
    /// bare-metal run is not exposed on all interfaces without opting in; the
    /// container image sets `BIND=0.0.0.0:8080` so a reverse proxy can reach it.
    #[arg(
        long,
        value_name = "ADDR",
        env = "BIND",
        default_value = "127.0.0.1:8080"
    )]
    bind: SocketAddr,

    /// Number of pre-warmed VMs in the pool — the cap on concurrent requests.
    /// Defaults to the CPU count.
    #[arg(long, value_name = "N", default_value_t = default_pool_size())]
    pool_size: usize,

    /// Per-request wall-clock limit (e.g. `5s`, `500ms`); on timeout the request
    /// gets a 503 (no limit if omitted).
    #[arg(long, value_name = "DUR", value_parser = parse_duration)]
    timeout: Option<Duration>,

    /// Max request-body size (e.g. `2m`); a larger request gets a 413 (no limit
    /// if omitted).
    #[arg(long = "max-body", value_name = "SIZE", value_parser = parse_size)]
    max_body: Option<usize>,

    /// Grace period for draining in-flight work on SIGTERM/SIGINT (e.g. `10s`).
    #[arg(long = "shutdown-grace", value_name = "DUR", default_value = "10s", value_parser = parse_duration)]
    shutdown_grace: Duration,

    /// Log output format
    #[arg(long, env = "LOG_FORMAT", default_value = "full")]
    log_format: LogFormat,

    #[command(flatten)]
    common: CommonFlags,
}

/// Default VM-pool size: the number of CPUs available to the process.
fn default_pool_size() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Load the user config layer: `--no-config` drops it, `--config` forces a
/// specific file (which must exist), otherwise the default location is loaded
/// if present (absent → empty, no error).
fn load_config(flags: &CommonFlags) -> Result<Config, String> {
    if flags.no_config {
        return Ok(Config::empty());
    }
    if let Some(path) = &flags.config {
        return Config::load(path).map_err(|e| e.to_string());
    }
    match default_config_path() {
        Some(path) if path.exists() => Config::load(&path).map_err(|e| e.to_string()),
        _ => Ok(Config::empty()),
    }
}

/// The default config path: `$XDG_CONFIG_HOME/lur/config`, falling back to
/// `~/.config/lur/config`.
fn default_config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(xdg).join("lur").join("config"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("lur").join("config"))
}

/// Resolve the capability policy from the config layer and the per-run flags.
/// The profile is last-wins (flags beat config); allowlists are additive
/// (config standing grants ∪ flag per-run grants). `-A`/`--loose` force the
/// permissive profile (§5/§12).
fn build_policy(flags: &CommonFlags, config: &Config) -> Result<Policy, String> {
    let profile = if flags.allow_all || flags.loose {
        Profile::Loose
    } else if flags.strict {
        Profile::Strict
    } else {
        config.default_profile.unwrap_or(Profile::Strict)
    };

    if profile == Profile::Loose {
        return Policy::loose().map_err(|e| e.to_string());
    }

    // Strict: union config grants (config fs paths may use `~`) with flag grants.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let home = home.as_deref();
    let mut read: Vec<PathBuf> = config
        .fs_read
        .iter()
        .map(|p| expand_tilde(p, home))
        .collect();
    read.extend(flags.allow_fs_read.iter().cloned());
    let mut write: Vec<PathBuf> = config
        .fs_write
        .iter()
        .map(|p| expand_tilde(p, home))
        .collect();
    write.extend(flags.allow_fs_write.iter().cloned());
    for p in &flags.allow_fs {
        read.push(p.clone());
        write.push(p.clone());
    }

    let mut env = config.env.clone();
    env.extend(flags.allow_env.iter().cloned());
    let mut net = config.net.clone();
    net.extend(flags.allow_net.iter().cloned());

    let mut policy = Policy::from_roots(&read, &write)
        .map_err(|e| format!("invalid fs allowlist path: {e}"))?
        .with_env(env)
        .with_net(net);
    if flags.allow_private {
        policy = policy.allow_private();
    }
    Ok(policy)
}

/// Build a [`RuntimeConfig`] from the shared flags (policy resolved, args set
/// by the caller).
fn build_config(flags: &CommonFlags, args: Vec<String>) -> Result<RuntimeConfig, String> {
    let config = load_config(flags)?;
    let policy = Arc::new(build_policy(flags, &config)?);
    Ok(RuntimeConfig {
        memory_limit: flags.memory,
        args,
        policy,
        max_http_body: flags.max_http_body,
        max_body: None,
        db_path: flags.db.clone(),
        pool_size: 1,
        per_event_timeout: None,
        state: std::sync::Arc::default(),
        shutdown_grace: Duration::from_millis(DEFAULT_SHUTDOWN_GRACE_MS),
        max_concurrency: flags.max_concurrency,
        chunk_name: None,
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
    if argv.get(1).map(String::as_str) == Some("docs") {
        const GUIDE: &str = include_str!("../docs/GUIDE.md");
        print!("{}", lur::docs::render(GUIDE, lur::color::stdout_color()));
        return ExitCode::SUCCESS;
    }
    run_one_shot(Cli::parse())
}

/// Load `app.lua` and serve it forever (server mode).
fn run_serve(cli: ServeCli) -> ExitCode {
    init_tracing(cli.log_format);

    let source = match std::fs::read_to_string(&cli.app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lur: cannot read {}: {e}", cli.app.display());
            return ExitCode::from(2);
        }
    };

    let mut config = match build_config(&cli.common, Vec::new()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::from(2);
        }
    };
    config.pool_size = cli.pool_size;
    config.per_event_timeout = cli.timeout;
    config.max_body = cli.max_body;
    config.shutdown_grace = cli.shutdown_grace;
    config.chunk_name = Some(cli.app.display().to_string());

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

    let mut config = match build_config(&cli.common, cli.script_args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::from(2);
        }
    };
    config.chunk_name = Some(cli.script.display().to_string());
    let rt = match Runtime::with_config(config) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("lur: {e}");
            return ExitCode::FAILURE;
        }
    };

    let timeout = cli.timeout;
    match rt.run_to_exit_code(&source, timeout) {
        #[allow(
            clippy::cast_sign_loss,
            reason = "process exit codes are u8; truncation is the intended Unix semantics"
        )]
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
            let chunk = cli.script.display().to_string();
            eprintln!(
                "{}",
                lur::diagnostics::render(
                    &source,
                    &chunk,
                    &e.to_string(),
                    lur::color::stderr_color()
                )
            );
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
