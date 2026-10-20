# syntax=docker/dockerfile:1
# Standalone native runtime: no official client, desktop, X11, VNC or ptrace tools.
# Build from the repository root. Only this validated target is supported initially.
FROM --platform=linux/amd64 rust:1.93.1-bookworm AS builder
WORKDIR /build
COPY packages/native-client-rust/ ./
ENV CARGO_BUILD_JOBS=4
RUN cargo test --workspace --locked \
    && cargo build --release --locked --target x86_64-unknown-linux-gnu

FROM --platform=linux/amd64 debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 agent-wechat \
    && useradd --uid 10001 --gid 10001 --home-dir /var/lib/agent-wechat --no-create-home --shell /usr/sbin/nologin agent-wechat \
    && mkdir -p /var/lib/agent-wechat /var/cache/agent-wechat /etc/agent-wechat/server \
    && chown 10001:10001 /var/lib/agent-wechat /var/cache/agent-wechat \
    && chmod 0700 /var/lib/agent-wechat /var/cache/agent-wechat
COPY --from=builder /build/target/x86_64-unknown-linux-gnu/release/wx /usr/local/bin/wx
COPY --from=builder /build/crates/wechat-protocol/NOTICE /usr/share/doc/agent-wechat-native/NOTICE
COPY --from=builder /build/crates/wechat-protocol/fixtures/LICENSE-CITIZENLAB /usr/share/doc/agent-wechat-native/LICENSE-CITIZENLAB
COPY --from=builder /build/crates/wechat-protocol/vendor/silk/LICENSE-MIT /usr/share/doc/agent-wechat-native/LICENSE-SILK-MIT
COPY --from=builder /build/crates/wechat-protocol/vendor/silk/LICENSE-SDK /usr/share/doc/agent-wechat-native/LICENSE-SILK-SDK
USER 10001:10001
ENV HOME=/var/lib/agent-wechat
ENV TMPDIR=/var/cache/agent-wechat
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/wx"]
CMD ["serve", "--server-config", "/etc/agent-wechat/server/config.toml"]
