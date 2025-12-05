FROM rust:1.86.0 AS rrr-builder
ENV NAME=rrr_proxy

# First build a dummy project with our dependencies to cache them in Docker
WORKDIR /usr/src
RUN cargo new --bin ${NAME}
WORKDIR /usr/src/${NAME}
COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
RUN cargo build --release
RUN rm -rf src/*.rs
COPY . .
RUN cargo build --release

FROM debian:trixie-slim
ENV NAME=rrr_proxy

COPY --from=rrr-builder /usr/src/${NAME}/target/release/${NAME} /usr/local/bin/${NAME}
CMD ["${NAME}"]
