FROM rust:1.97-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --locked --release --bin gemini-bridge

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 gemini-bridge \
    && useradd --system --uid 10001 --gid gemini-bridge --home-dir /var/lib/gemini-bridge gemini-bridge \
    && install -d -m 0700 -o gemini-bridge -g gemini-bridge /var/lib/gemini-bridge
COPY --from=builder /src/target/release/gemini-bridge /usr/local/bin/gemini-bridge
COPY bridge.example.toml /etc/gemini-bridge/bridge.toml
ENV HOME=/var/lib/gemini-bridge
ENV BRIDGE_STORAGE_DATA_DIR=/var/lib/gemini-bridge
USER gemini-bridge:gemini-bridge
EXPOSE 8090
ENTRYPOINT ["/usr/local/bin/gemini-bridge"]
CMD ["--config", "/etc/gemini-bridge/bridge.toml"]
