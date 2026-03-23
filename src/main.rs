use rrr_proxy::consts;
use rrr_proxy::manager::pmetrics::MetricService;
use rrr_proxy::manager::register::SignalHandler;
use rrr_proxy::manager::{
    endpoint::AllocatorService, load::ReportLoadSysProvider, register::RegisterAgent,
};
use rrr_proxy::proxy::ProxyManager;
use signal_hook::consts::*;
use signal_hook_tokio::Signals;
use std::sync::Arc;
use tokio::{sync::RwLock, try_join};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            format!(
                "{}=info,tower_http=info,tokio=info,axum=info",
                env!("CARGO_CRATE_NAME")
            )
            .into()
        }))
        .with(fmt::layer())
        .init();
    let mut metrics_exp = MetricService::new();

    let proxy_manager = Arc::new(RwLock::<ProxyManager>::new(ProxyManager::new()));

    let load_reporter = Arc::new(ReportLoadSysProvider::new());
    let mut register_agent =
        RegisterAgent::new((*consts::REGISTER_ENDPOINT).to_string(), load_reporter);
    let mut proxy_app = AllocatorService::new(Arc::clone(&proxy_manager));
    let signals = Signals::new(&[SIGTERM, SIGINT])?;
    let mut signal_hnadler = SignalHandler::new(
        signals,
        *consts::GRACEFULLL_SHUTDOWN_DELAY,
        register_agent.tx.clone(),
    );
    let _ = try_join!(
        metrics_exp.start(*consts::METRICS_PORT),
        proxy_app.start(*consts::SERVICE_PORT),
        register_agent.run(),
        signal_hnadler.run()
    );
    Ok(())
}
