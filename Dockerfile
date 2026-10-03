ARG RUST_IMAGE=rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
ARG RUNTIME_IMAGE=debian:bookworm-20260918-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
FROM ${RUST_IMAGE} AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY schema ./schema
COPY src ./src
COPY bridge.example.toml ./bridge.example.toml
RUN cargo build --locked --release --bin gemini-bridge

FROM ${RUNTIME_IMAGE} AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 gemini-bridge \
    && useradd --system --uid 10001 --gid gemini-bridge --home-dir /var/lib/gemini-bridge gemini-bridge \
    && install -d -m 0700 -o gemini-bridge -g gemini-bridge /var/lib/gemini-bridge \
    && install -d -m 0755 /etc/gemini-bridge
COPY --from=builder /src/target/release/gemini-bridge /usr/local/bin/gemini-bridge
COPY LICENSE /usr/share/licenses/gemini-bridge/LICENSE
COPY bridge.example.toml /etc/gemini-bridge/bridge.toml
ENV HOME=/var/lib/gemini-bridge \
    BRIDGE_STORAGE_DATA_DIR=/var/lib/gemini-bridge
USER 10001:10001
EXPOSE 8090
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD curl --fail --silent http://127.0.0.1:8090/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/gemini-bridge"]
CMD ["--config", "/etc/gemini-bridge/bridge.toml"]
