use std::sync::Arc;

use rrr_proxy::{
    AllocatorService, ProxyManager, manager::load::ReportLoadSysProvider,
    manager::register::RegisterAgent,
};
use tokio::{sync::RwLock, try_join};
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

    let load_reporter = Arc::new(RwLock::<ReportLoadSysProvider>::new(
        ReportLoadSysProvider::new(),
    ));
    let register_agent =
        RegisterAgent::new("http://localhost:5555".to_string(), 500, load_reporter);
    let mut proxy_app = AllocatorService::new(Arc::clone(&proxy_manager));
    try_join!(proxy_app.start(3333), register_agent.run());
}
