use std::sync::Arc;

use rrr_proxy::{AllocatorService, ProxyManager};
use tokio::sync::RwLock;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            format!(
                "{}=debug,tower_http=debug,tokio=debug",
                env!("CARGO_CRATE_NAME")
            )
            .into()
        }))
        .with(fmt::layer())
        .init();
    let proxy_manager = Arc::new(RwLock::<ProxyManager>::new(ProxyManager::new()));

    let mut singnaling_app = AllocatorService::new(Arc::clone(&proxy_manager));
    singnaling_app.start(3333).await;
}
