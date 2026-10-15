use anyhow::Result;
use bazarr_parakeet::{
    cli::{self, Action, Cli},
    config::Config,
    server,
};
use clap::Parser;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bazarr_parakeet=info".into()),
        )
        .init();
    let config = Config::from_env()?;
    if let Some(Action::Transcribe(args)) = args.command {
        return cli::transcribe(args, config);
    }
    let address = config.bind.clone();
    let app = server::app(config)?;
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address, "bazarr-parakeet listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .unwrap();
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}
