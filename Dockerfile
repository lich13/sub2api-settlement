ARG NODE_IMAGE=node:24.22.1-bookworm-slim@sha256:55fb5f5a527ea2c5c4a95f963e48ef90aa38c7b346054fdc8ee9957a2b55d205
ARG RUST_IMAGE=rust:1.99.1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
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
LABEL org.opencontainers.image.title="Sub2API Settlement" org.opencontainers.image.version="0.12.1" org.opencontainers.image.revision=${APP_REVISION}
WORKDIR /app
COPY --from=backend /build/target/release/settlement ./settlement
COPY --from=frontend /build/dist ./frontend/dist
USER 10002:10002
EXPOSE 18082
STOPSIGNAL SIGTERM
CMD ["/app/settlement"]
