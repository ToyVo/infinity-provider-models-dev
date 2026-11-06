//! Standalone binary serving the multi-backend [`ModelProvider`] over a Unix
//! socket (see `infinity_provider_protocol::remote`), plus a `login`
//! subcommand for Claude subscription sign-in.
//!
//! Prints the generated socket path as the first (and only) stdout line so a
//! supervisor (e.g. `infinity-daemon`) can discover it; logs go to stderr.

use std::io::Write;
use std::sync::Arc;

use infinity_provider_models_dev::MultiProvider;
use infinity_provider_models_dev::oauth;
use infinity_provider_protocol::remote::serve_provider;
use tokio::io::AsyncBufReadExt as _;
use tracing_subscriber::EnvFilter;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let mut rest = std::env::args().skip(1);
    match rest.next().as_deref() {
        Some("login") => login(rest.collect::<Vec<_>>().as_slice()).await,
        Some(other) => {
            Err(format!("unknown command: {other} (expected `login` or no arguments)").into())
        }
        None => serve().await,
    }
}

/// Serve the provider over a Unix socket (daemon mode).
async fn serve() -> Result<(), BoxError> {
    // stdout is reserved for the socket path; log to stderr (captured into
    // the supervising daemon's log, so no ANSI styling).
    // Default to `info` so the activated backends are visible in the
    // daemon log; `RUST_LOG` overrides when set.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let provider = Arc::new(MultiProvider::from_env());
    let (socket_path, server) = serve_provider(provider)?;

    println!("{}", socket_path.display());
    std::io::stdout().flush()?;

    server.await;
    Ok(())
}

/// Browser sign-in for Claude Pro/Max subscriptions.
///
/// Prints an authorize URL, reads the pasted callback from stdin, exchanges
/// it, and stores the tokens in `~/.infinity/claude-oauth.json`.
async fn login(args: &[String]) -> Result<(), BoxError> {
    let mut mode = oauth::AccountMode::Max;
    let mut flags = args.iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--mode" => {
                let Some(value) = flags.next() else {
                    return Err("`login --mode` needs a value (max|console)".into());
                };
                mode = value.parse()?;
            }
            "--help" | "-h" => {
                println!("usage: infinity-provider-models-dev login [--mode max|console]");
                return Ok(());
            }
            other => {
                return Err(format!("unknown login flag: {other}").into());
            }
        }
    }

    let auth = oauth::authorize(mode)?;
    println!("Open this URL in your browser, sign in, and approve access:");
    println!();
    println!("{}", auth.url);
    println!();
    print!("Paste the redirected URL (or code) here: ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    tokio::io::BufReader::new(tokio::io::stdin())
        .read_line(&mut input)
        .await?;
    let Some(callback) = oauth::parse_callback(&input) else {
        return Err("could not find a code in that input; paste the full redirect URL".into());
    };
    if callback.state != auth.state {
        return Err("state mismatch: this code is for a different sign-in attempt".into());
    }

    let client = reqwest::Client::new();
    let issued = oauth::exchange_code(&client, &callback, &auth.verifier).await?;
    let Some(issued) = issued else {
        return Err("the endpoint rejected the code (expired or already used); try again".into());
    };
    let path = oauth::store_login(mode, &issued, oauth::now_ms())?;
    println!();
    println!("Signed in. Tokens stored in {}", path.display());
    println!("Enable the subscription backend with: export INFINITY_CLAUDE_OAUTH=1");
    Ok(())
}
