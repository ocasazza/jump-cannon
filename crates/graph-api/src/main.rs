use std::path::PathBuf;
use clap::Parser;

use graph_api::{router, vault_loader, AppState};

#[derive(Parser)]
#[command(name = "graph-api")]
struct Args {
    /// Vault root directory (defaults to current directory)
    #[arg(short, long)]
    vault_root: Option<PathBuf>,
    /// Listen port (0 = OS picks)
    #[arg(short, long, default_value_t = 0)]
    port: u16,
    /// Don't auto-open the browser
    #[arg(long)]
    no_browser: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let vault_root = args
        .vault_root
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let graph = vault_loader::load(&vault_root);
    let state = AppState::new(vault_root, graph);

    let app = router(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let url = format!("http://{}/", bound);
    tracing::info!(%url, "listening");
    println!("{}", url);

    if !args.no_browser {
        graph_api::browser::open_url(&url);
    }

    axum::serve(listener, app).await?;
    Ok(())
}
