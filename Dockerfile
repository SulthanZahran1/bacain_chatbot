# Multi-stage build: rust builder → debian-slim runtime (non-root).
FROM rust:1.97-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin linkbot-bot

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 linkbot
USER linkbot
WORKDIR /app
COPY --from=builder /build/target/release/linkbot-bot /app/linkbot-bot
COPY --from=builder /build/optimized_policy.json /app/optimized_policy.json
ENV OPTIMIZED_POLICY_JSON=/app/optimized_policy.json
CMD ["/app/linkbot-bot"]
