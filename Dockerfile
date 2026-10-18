ARG NODE_IMAGE=node:24-bookworm-slim@sha256:a5f4366e834695d5f363b920c104953a535d3a9864d1bdf74566a0ebe01daee5
ARG RUST_IMAGE=rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
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

FROM ${RUNTIME_IMAGE} AS runtime-root
RUN useradd -u 10002 -M settlement \
    && rm -f -- \
       /usr/share/gcc/python/libstdcxx/__init__.py \
       /usr/share/gcc/python/libstdcxx/v6/__init__.py \
       /usr/share/gcc/python/libstdcxx/v6/printers.py \
       /usr/share/gcc/python/libstdcxx/v6/xmethods.py \
       /usr/share/gdb/auto-load/usr/lib/x86_64-linux-gnu/libstdc++.so.6.0.30-gdb.py \
       /usr/share/gdb/auto-load/usr/lib/aarch64-linux-gnu/libstdc++.so.6.0.30-gdb.py

# Copy the sanitized root filesystem so removed helpers are absent from every published layer.
FROM scratch
COPY --from=runtime-root / /
ARG APP_REVISION=development
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ENV DATA_DIR=/data APP_REVISION=${APP_REVISION} SETTLEMENT_ASSETS=/app/frontend/dist
LABEL org.opencontainers.image.title="Sub2API Settlement" org.opencontainers.image.version="0.12.0" org.opencontainers.image.revision=${APP_REVISION}
WORKDIR /app
COPY --from=backend /build/target/release/settlement ./settlement
COPY --from=frontend /build/dist ./frontend/dist
USER 10002:10002
EXPOSE 18082
STOPSIGNAL SIGTERM
CMD ["/app/settlement"]
