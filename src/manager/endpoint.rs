use axum::{
    Json, Router,
    body::{Body, Bytes},
    error_handling::HandleErrorLayer,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, post},
};
use std::{borrow::Cow, sync::Arc, time::Duration};
use tower::{BoxError, ServiceBuilder};
use tower_http::trace::TraceLayer;
use tracing::{debug, error};

use http_body_util::BodyExt;

use crate::manager::models::AllocateRequest;
use crate::proxy::ProxyManagerShared;

pub struct AllocatorService {
    pub app: Router,
}

impl AllocatorService {
    pub fn new(proxy: ProxyManagerShared) -> AllocatorService {
        let app = Router::new()
            .route("/api/v1/proxy-ports", post(allocate_proxy))
            .route(
                "/api/v1/proxy-ports/{port}/{session_id}",
                delete(delete_proxy),
            )
            .layer(
                ServiceBuilder::new()
                    // Handle errors from middleware
                    .layer(HandleErrorLayer::new(handle_error))
                    .load_shed()
                    .concurrency_limit(128) // TODO: move to properties
                    .timeout(Duration::from_secs(10)) // TODO: move to properties
                    .layer(TraceLayer::new_for_http()),
            )
            .layer(middleware::from_fn(print_request_body))
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

// middleware that shows how to consume the request body upfront
async fn print_request_body(request: Request, next: Next) -> Result<impl IntoResponse, Response> {
    let request = buffer_request_body(request).await?;
    let response = next.run(request).await;
    tracing::debug!(body = ?response.body(), "handler received body");
    Ok(response)
}

async fn buffer_request_body(request: Request) -> Result<Request, Response> {
    let (parts, body) = request.into_parts();

    // this won't work if the body is an long running stream
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
    Path((port, _)): Path<(u16, String)>,
) -> Response {
    let mut mgr = manager.write().await;
    match mgr.delete_udp_proxy(port).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => match mgr.delete_tcp_proxy(port).await {
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
