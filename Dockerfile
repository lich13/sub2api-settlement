ARG NODE_IMAGE=node:24-bookworm-slim
ARG RUST_IMAGE=rust:1.99.0-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
FROM ${NODE_IMAGE} AS frontend
WORKDIR /build
RUN corepack enable
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY frontend/ ./
RUN pnpm build

FROM ${RUST_IMAGE} AS backend
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY xtask ./xtask
RUN cargo build --release --locked --bin settlement

FROM ${RUNTIME_IMAGE}
ARG APP_REVISION=development
ENV DATA_DIR=/data APP_REVISION=${APP_REVISION} SETTLEMENT_ASSETS=/app/frontend/dist
LABEL org.opencontainers.image.title="Sub2API Settlement" org.opencontainers.image.version="0.9.0" org.opencontainers.image.revision=${APP_REVISION}
RUN useradd -u 10002 -M settlement
WORKDIR /app
COPY --from=backend /build/target/release/settlement ./settlement
COPY --from=frontend /build/dist ./frontend/dist
USER 10002:10002
EXPOSE 18082
STOPSIGNAL SIGTERM
CMD ["/app/settlement"]
