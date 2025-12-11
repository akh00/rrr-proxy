use axum::{
    Json, Router,
    body::{Body, Bytes},
    error_handling::HandleErrorLayer,
    extract::{MatchedPath, Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use metrics::{counter, histogram};
use std::{borrow::Cow, future::ready, sync::Arc, time::Duration};
use tokio::time::Instant;
use tower::{BoxError, ServiceBuilder};
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info};

use http_body_util::BodyExt;

use crate::{consts, manager::models::AllocateRequest};
use crate::{manager::pmetrics, proxy::ProxyManagerShared};

pub struct AllocatorService {
    pub app: Router,
}

impl AllocatorService {
    pub fn new(proxy: ProxyManagerShared) -> AllocatorService {
        let app = Router::new()
            .route(&(*consts::ALLOCATE_PATH), post(allocate_proxy))
            .route(&(*consts::DELETE_PATH), delete(delete_proxy))
            .route(
                &(*consts::METRICS_EXPOSE_PATH),
                get(move || ready((*pmetrics::globals::PROMETHEUS_HANDLER).render())),
            )
            .layer(
                ServiceBuilder::new()
                    // Handle errors from middleware
                    .layer(HandleErrorLayer::new(handle_error))
                    .load_shed()
                    .concurrency_limit(*consts::HTTP_REQUEST_CONCURRENT_NUM) // TODO: move to properties
                    .timeout(Duration::from_millis(*consts::HTTP_REQUEST_TIMEOUT)) // TODO: move to properties
                    .layer(TraceLayer::new_for_http()),
            )
            // it's pure for debug purposes
            .layer(middleware::from_fn(print_request_body))
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

// for debug only
// middleware that shows how to consume the request body upfront
async fn print_request_body(request: Request, next: Next) -> Result<impl IntoResponse, Response> {
    let request = buffer_request_body(request).await?;
    let response = next.run(request).await;
    tracing::debug!(body = ?response.body(), "handler received body");
    Ok(response)
}

async fn buffer_request_body(request: Request) -> Result<Request, Response> {
    let (parts, body) = request.into_parts();

    let bytes = body
        .collect()
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response())?
        .to_bytes();

    do_thing_with_request_body(bytes.clone());

    Ok(Request::from_parts(parts, Body::from(bytes)))
}

fn do_thing_with_request_body(bytes: Bytes) {
    debug!(body = ?bytes);
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
