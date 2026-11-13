# Multi-stage build for the lan-mdns single-file artifact (F-I01 / F-I03).
# The binary itself is statically linked against musl and has no runtime
# dependencies; the small alpine base only supplies optional helpers:
#   iproute2       -> `ip -4 -o addr show` for interface enumeration
#   curl + CA      -> TLS bridge for public https:// alert webhooks
# Copying JUST target/release/lan-mdns onto a bare host works unchanged.
FROM rust:1.94-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY common ./common
COPY components ./components
COPY contracts ./contracts
RUN cargo build --release -p lan-mdns && strip target/release/lan-mdns

FROM alpine:3.20
RUN apk add --no-cache iproute2 curl ca-certificates \
    && addgroup -S lanstudio && adduser -S -H -G lanstudio lanstudio
COPY --from=builder /src/target/release/lan-mdns /usr/local/bin/lan-mdns

# Every piece of mutable state lives under one volume (F-I06). The binary
# generates config.yaml from its embedded template on first boot (F-E09).
ENV LAN_STUDIO_CONFIG=/data/config.yaml \
    LAN_STUDIO_AUTH_FILE=/data/auth.toml \
    LAN_STUDIO_LOG_DIR=/data/logs \
    LAN_STUDIO_HISTORY_DIR=/data/history \
    LAN_STUDIO_PORT=8080

VOLUME ["/data"]
EXPOSE 8080/tcp 5353/udp 80/tcp

# The binary is its own probe client, so no curl/wget is required here.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD /usr/local/bin/lan-mdns --healthcheck

USER lanstudio
ENTRYPOINT ["/usr/local/bin/lan-mdns"]
