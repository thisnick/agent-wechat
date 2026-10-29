# syntax=docker/dockerfile:1
# Standalone native runtime: no official client, desktop, X11, VNC or ptrace tools.
# Build on a native x64 or ARM64 worker (CI runs each architecture separately).
FROM rust:1.93.1-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.77 --locked
WORKDIR /build
ENV CARGO_BUILD_JOBS=4

FROM chef AS planner
COPY packages/native-client-rust/ ./
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
# Native client CI owns workspace tests on both Linux architectures. This image
# builds the shipping CLI; distribution CI exercises the actual binary/container.
ENV CARGO_INCREMENTAL=0
# Exportable dependency layers survive source changes and fresh CI workers.
RUN cargo chef cook --package wx-native --release --locked --recipe-path recipe.json
COPY packages/native-client-rust/ ./
RUN cargo build --release --locked --package wx-native \
    && rustc --version > /build/rustc-version

# CI extracts one tested binary. Exporting this stage avoids loading the Rust
# toolchain and both profiles' compiled dependencies into the Docker daemon.
FROM scratch AS release-artifact
COPY --from=builder /build/target/release/wx /build/target/release/wx
COPY --from=builder /build/rustc-version /build/rustc-version
# docker create requires a command; this extraction image is never started.
CMD ["/build/target/release/wx"]

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 agent-wechat \
    && useradd --uid 10001 --gid 10001 --home-dir /var/lib/agent-wechat --no-create-home --shell /usr/sbin/nologin agent-wechat \
    && mkdir -p /var/lib/agent-wechat /var/cache/agent-wechat /etc/agent-wechat/server \
    && chown 10001:10001 /var/lib/agent-wechat /var/cache/agent-wechat \
    && chmod 0700 /var/lib/agent-wechat /var/cache/agent-wechat
COPY --from=builder /build/target/release/wx /usr/local/bin/wx
COPY LICENSE /usr/share/doc/agent-wechat-native/LICENSE
COPY packages/cli/THIRD_PARTY_NOTICES.txt /usr/share/doc/agent-wechat-native/THIRD-PARTY-NOTICES.txt
COPY --from=builder /build/crates/wechat-protocol/NOTICE /usr/share/doc/agent-wechat-native/NOTICE
COPY --from=builder /build/crates/wechat-protocol/fixtures/LICENSE-CITIZENLAB /usr/share/doc/agent-wechat-native/LICENSE-CITIZENLAB
COPY --from=builder /build/crates/wechat-protocol/vendor/silk/LICENSE-MIT /usr/share/doc/agent-wechat-native/LICENSE-SILK-MIT
COPY --from=builder /build/crates/wechat-protocol/vendor/silk/LICENSE-SDK /usr/share/doc/agent-wechat-native/LICENSE-SILK-SDK
COPY --from=builder /build/crates/wechat-media/NOTICE.md /usr/share/doc/agent-wechat-native/NOTICE-WXGF
COPY --from=builder /build/crates/wechat-media/LICENSE-CHATLOG /usr/share/doc/agent-wechat-native/LICENSE-CHATLOG
USER 10001:10001
ENV HOME=/var/lib/agent-wechat
ENV TMPDIR=/var/cache/agent-wechat
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/wx"]
CMD ["serve", "--server-config", "/etc/agent-wechat/server/config.toml"]
