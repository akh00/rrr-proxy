# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

Limit build parallelism to 2 jobs (`-j 2`) on this host.

```bash
cargo build -j 2                       # debug build
cargo build -j 2 --release             # release build (what the Dockerfile produces)
cargo test -j 2                        # all tests (integration tests live in tests/integration_test.rs)
cargo test -j 2 --test integration_test it_should_pass -- --nocapture   # single test
cargo clippy -j 2
cargo fmt
```

- Integration tests bind real UDP/TCP sockets on `0.0.0.0:0` and start a `ws-mock` WebSocket server; `reporting_shold_work` intentionally runs for ~12s (heartbeat interval).
- aarch64 cross-builds use `aarch64-linux-gnu-gcc` as linker (`.cargo/config.toml`) and vendored OpenSSL (`Cargo.toml` target section).
- Running locally needs at least `REGISTER_ENDPOINT` pointing to a WebSocket registry (see `.vscode/launch.json`); without it the register agent retries `ws://localhost:5555`.

## Architecture

`rrr_proxy` is a UDP/TCP relay proxy (Rust 2024, tokio, axum). A controller allocates per-session proxy ports via HTTP; clients then send traffic to the proxy's public IP/port and it is relayed to a target media server (`target_ip:target_*_port`).

`main.rs` runs four long-lived tasks under one `try_join!` — if any returns `Err`, the process exits:
1. `MetricService` (`manager/pmetrics.rs`) – Prometheus exporter on `METRICS_PORT` (default 9321) at `METRICS_EXPOSE_PATH`. Global counters live in `pmetrics::globals` as `LazyLock<Counter>`.
2. `AllocatorService` (`manager/endpoint.rs`) – axum HTTP API on `SERVICE_PORT` (default 3333): `POST ALLOCATE_PATH` (camelCase `AllocateRequest` → `AllocateResponse`), `DELETE DELETE_PATH` (`/{port}/{session_id}`; tries UDP then TCP), `GET STATUS_PATH`. Wrapped with load-shed, concurrency limit, timeout, and an `http_metrics` middleware.
3. `RegisterAgent` (`manager/register.rs`) – keeps a WebSocket to the registry: sends a `register` message, then periodic `load` reports (CPU % via `sysinfo`, `manager/load.rs`) every `HEARTBIT_DELAY` ms; reconnects up to `MAX_REGISTRATION_ATTEMPTS`. Outgoing WS messages flow through an unbounded mpsc whose `tx` is shared.
4. `SignalHandler` (`manager/register.rs`) – on SIGTERM/SIGINT pushes a `deregister` message (with drain period) into the register agent's channel, sleeps `GRACEFULLL_SHUTDOWN_DELAY` seconds, then returns `Err` to terminate the process.

### Proxy core (actor/agent pattern, `src/proxy*`)

- `ProxyManager` (`proxy.rs`) is shared as `Arc<RwLock<ProxyManager>>` (`ProxyManagerShared`). Its public methods send `ProxyManagerMsg` requests with a `oneshot` reply channel to a background task spawned in `ProxyManager::new()`. That task owns the `Endpoint → handler` maps; the `ProxyManager` struct itself only keeps `Session{port, session_id} → Endpoint` lookup maps for DELETE.
- Endpoints signal their own termination (idle timeout, errors) back to the manager task via `UdpEndpointEnded` / `TcpEndpointEnded`, which removes them from the handler maps.
- UDP (`proxy/udp_client.rs`): `ProxyEndpoint` owns one public-facing socket bound to an ephemeral port. For each distinct client `SocketAddr` it creates a `ProxyClient` with its own socket `connect`ed to the target; replies from the target are `send_to` the client through the shared public socket. Clients and endpoints exit after `TRAFFIC_WAIT_TIMEOUT` seconds without traffic. `ProxyClientHandler` aborts its task on `Drop`.
- TCP (`proxy/tcp_clients.rs`): `ProxyTcpEndpointHandler` binds a listener on an ephemeral port; each accepted connection gets a `ProxyTcpClient` that opens a connection to the target and pumps bytes both ways with the same idle timeout.
- An allocation creates one UDP proxy plus optional TCP and SSL-TCP proxies (a target port of `"0"` means "not requested"). Ports in requests are strings.
- UDP has two modes (`UdpProxyMode`), chosen at start by `UDP_POOL_SIZE` (0 = `PerSession`, default). In `Pooled` mode `ProxyManager::with_udp_pool` binds N long-lived sockets (`proxy/udp_pooled.rs`) shared by all sessions: an unknown client's first datagram must be a STUN binding request whose local ufrag (`proxy/stun.rs`) picks the target, after which the client address routes the flow. Allocation requires `ufrag` in this mode; DELETE ends only that session. Pooled routing state lives in the endpoint task and changes via acknowledged control messages. SO_REUSEPORT is set only for fixed pooled ports (`UDP_POOL_BASE_PORT`), because on ephemeral binds the kernel can give two pooled sockets the same port.
- Errors are mostly `Box<dyn Error>`; the `FixBoxError` trait converts to `Send + Sync` boxes where needed across tasks.

### Configuration

All configuration is environment variables, read lazily in `lib.rs` `consts` (`LazyLock`, with defaults on missing/invalid values). Note units differ: `TRAFFIC_WAIT_TIMEOUT` and `GRACEFULLL_SHUTDOWN_DELAY` are seconds, `HEARTBIT_DELAY` and `HTTP_REQUEST_TIMEOUT` are milliseconds. Log level via `RUST_LOG` (`EnvFilter`; default `rrr_proxy=info`).

`src/ebpf.rs` and `src/ebpf/attack_detector.rs` are empty placeholders.
