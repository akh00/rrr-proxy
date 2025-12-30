use axum::{
    Json, Router,
    body::Body,
    error_handling::HandleErrorLayer,
    extract::{MatchedPath, Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use metrics::{counter, histogram};
use std::{borrow::Cow, sync::Arc, time::Duration};
use tokio::time::Instant;
use tower::{BoxError, ServiceBuilder};
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info};

use crate::proxy::ProxyManagerShared;
use crate::{consts, manager::models::AllocateRequest};

pub struct AllocatorService {
    pub app: Router,
}

impl AllocatorService {
    pub fn new(proxy: ProxyManagerShared) -> AllocatorService {
        let app = Router::new()
            .route(&(*consts::ALLOCATE_PATH), post(allocate_proxy))
            .route(&(*consts::DELETE_PATH), delete(delete_proxy))
            .route(&(*consts::STATUS_PATH), get(status_proxy))
            .layer(
                ServiceBuilder::new()
                    // Handle errors from middleware
                    .layer(HandleErrorLayer::new(handle_error))
                    .load_shed()
                    .concurrency_limit(*consts::HTTP_REQUEST_CONCURRENT_NUM) // TODO: move to properties
                    .timeout(Duration::from_millis(*consts::HTTP_REQUEST_TIMEOUT)) // TODO: move to properties
                    .layer(TraceLayer::new_for_http()),
            )
            .layer(middleware::from_fn(http_metrics))
            .with_state(Arc::clone(&proxy));
        AllocatorService { app }
    }

    pub async fn start(&mut self, port: u16) -> Result<(), anyhow::Error> {
        let listener =
            tokio::net::TcpListener::bind("0.0.0.0:".to_owned() + &port.to_string()).await?;
        match axum::serve(listener, self.app.clone()).await {
            Ok(_) => Ok(()),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }
}

// metrcis
pub async fn http_metrics(req: Request<Body>, next: Next) -> impl IntoResponse {
    let start = Instant::now();

    let path = if let Some(matched_path) = req.extensions().get::<MatchedPath>() {
        matched_path.as_str().to_owned()
    } else {
        req.uri().path().to_owned()
    };

    if path.contains(&(*consts::METRICS_EXPOSE_PATH)) {
        return next.run(req).await;
    };

    let method = req.method().clone();

    // Run the rest of the request handling first, so we can measure it and get response
    // codes.
    let response = next.run(req).await;

    let latency = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    let labels = [
        ("method", method.to_string()),
        ("path", path),
        ("status", status),
    ];

    let http_total = counter!("http_requests_total", &labels);
    http_total.increment(1);
    let latency_his = histogram!("http_requests_duration_seconds", &labels);
    latency_his.record(latency);

    response
}

async fn handle_error(error: BoxError) -> impl IntoResponse {
    if error.is::<tower::timeout::error::Elapsed>() {
        return (StatusCode::REQUEST_TIMEOUT, Cow::from("request timed out"));
    }

    if error.is::<tower::load_shed::error::Overloaded>() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Cow::from("service is overloaded, try again later"),
        );
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Cow::from(format!("Unhandled internal error: {error}")),
    )
}

#[axum_macros::debug_handler]
async fn delete_proxy(
    State(manager): State<ProxyManagerShared>,
    Path((port, session)): Path<(u16, String)>,
) -> Response {
    info!("delete request {:?} {:?}", port, session);
    let mut mgr = manager.write().await;
    match mgr.delete_udp_proxy(port, session.clone()).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => match mgr.delete_tcp_proxy(port, session).await {
            Ok(_) => StatusCode::OK.into_response(),
            Err(_) => StatusCode::BAD_REQUEST.into_response(),
        },
    }
}

#[axum_macros::debug_handler]
async fn status_proxy() -> Response {
    debug!("status request");
    StatusCode::OK.into_response()
}

#[axum_macros::debug_handler]
async fn allocate_proxy(
    State(manager): State<ProxyManagerShared>,
    Json(body): Json<AllocateRequest>,
) -> Response {
    match manager.write().await.create_proxy(body).await {
        Ok(res) => {
            debug!("Allocation request is successfull");
            Json(res).into_response()
        }
        Err(err) => {
            error!(error = ?err, "Error during ports allocations");
            StatusCode::NOT_ACCEPTABLE.into_response()
        }
    }
}
