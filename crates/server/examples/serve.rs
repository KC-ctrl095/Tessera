//! A bare server for development and live tests, without the CLI:
//!   TESSERA_HOME=/tmp/x cargo run -p tessera-server --example serve -- 127.0.0.1:7980

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7980".into());
    let paths = Paths::resolve();
    let engine = Engine::open(paths, EngineOptions { scheduler: false }).await?;
    let router = tessera_server::router(engine, None)?;
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("serving on http://{bind}");
    tessera_server::serve_on(listener, router).await?;
    Ok(())
}
