use axum::{
    Json, Router,
    error_handling::HandleErrorLayer,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, post},
    serve::Serve,
};
use std::{borrow::Cow, sync::Arc, time::Duration};
use tower::{BoxError, ServiceBuilder};
use tower_http::trace::TraceLayer;

use crate::manager::models::AllocateRequest;
use crate::proxy::ProxyManagerShared;

pub struct AllocatorService {
    pub app: Router,
}

impl AllocatorService {
    pub fn new(proxy: ProxyManagerShared) -> AllocatorService {
        let app = Router::new()
            // `GET /` goes to `root`
            .route("/api/v1/allocate", post(allocate_proxy))
            .route("/api/v1/delete/udp/{port}", delete(delete_udp_proxy))
            .route("/api/v1/delete/tcp/{port}", delete(delete_tcp_proxy))
            .layer(
                ServiceBuilder::new()
                    // Handle errors from middleware
                    .layer(HandleErrorLayer::new(handle_error))
                    .load_shed()
                    .concurrency_limit(128) // TODO: move to properties
                    .timeout(Duration::from_secs(10)) // TODO: move to properties
                    .layer(TraceLayer::new_for_http()),
            )
            .with_state(Arc::clone(&proxy));
        AllocatorService { app }
    }

    pub async fn start(&mut self, port: u16) -> Result<(), String> {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:".to_owned() + &port.to_string())
            .await
            .unwrap();
        match axum::serve(listener, self.app.clone()).await {
            Ok(_) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
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
async fn delete_udp_proxy(
    State(manager): State<ProxyManagerShared>,
    Path(port): Path<u16>,
) -> Response {
    match manager.write().await.delete_udp_proxy(port).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

#[axum_macros::debug_handler]
async fn delete_tcp_proxy(
    State(manager): State<ProxyManagerShared>,
    Path(port): Path<u16>,
) -> Response {
    match manager.write().await.delete_tcp_proxy(port).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

#[axum_macros::debug_handler]
async fn allocate_proxy(
    State(manager): State<ProxyManagerShared>,
    Json(body): Json<AllocateRequest>,
) -> Response {
    match manager.write().await.create_proxy(body).await {
        Ok(res) => Json(res).into_response(),
        Err(_) => StatusCode::NOT_ACCEPTABLE.into_response(),
    }
}
