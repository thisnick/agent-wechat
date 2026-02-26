# ============================================
# Stage 1: Build the Rust server binary
# ============================================
# Use the same Ubuntu version as the runtime stage so the compiled binary
# links against a compatible glibc (2.35).  Building on a newer distro
# (e.g. Debian Bookworm with glibc 2.36) can produce binaries that fail
# at runtime with "GLIBC_2.36 not found".
FROM ubuntu:22.04 AS builder

ARG BUILD_MODE=release

ENV DEBIAN_FRONTEND=noninteractive
# Rust installation
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:$PATH"

WORKDIR /build

# Install build dependencies + Rust toolchain
RUN apt-get update && apt-get install -y \
    curl build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --default-toolchain stable --profile minimal \
    && rustup show

# Copy manifests first for dependency caching
COPY agent-server-rust/Cargo.toml agent-server-rust/Cargo.lock* ./
# Create dummy src for dependency build
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs
RUN if [ "$BUILD_MODE" = "debug" ]; then cargo build 2>/dev/null || true; else cargo build --release 2>/dev/null || true; fi
# Remove dummy build artifacts
RUN rm -rf src target/release/deps/agent_server* target/debug/deps/agent_server*

# Copy actual source and migrations, then build
COPY agent-server-rust/src ./src
COPY agent-server-rust/migrations ./migrations
RUN if [ "$BUILD_MODE" = "debug" ]; then cargo build; else cargo build --release; fi

# Copy binary to a known location regardless of build mode
RUN mkdir -p /output && cp target/${BUILD_MODE}/agent-server /output/agent-server

# ============================================
# Stage 2: Final runtime image
# ============================================
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

# ============================================
# System packages
# ============================================
RUN apt-get update && apt-get install -y \
    # Display
    xvfb fluxbox x11vnc \
    # Input automation
    xdotool scrot \
    # Accessibility
    at-spi2-core gir1.2-atspi-2.0 python3 python3-gi \
    # Notifications
    dunst libnotify-bin \
    # Fonts
    fonts-noto-cjk \
    # System
    dbus dbus-x11 ca-certificates curl \
    # WeChat dependencies (basic)
    libatomic1 libasound2 libatk-bridge2.0-0 libatk1.0-0 \
    libgbm1 libglib2.0-0 libgtk-3-0 libnss3 libx11-6 \
    libxkbcommon0 libxkbcommon-x11-0 libxcb-icccm4 \
    libxcb-image0 libxcb-keysyms1 libxcb-render-util0 libxss1 \
    # Audio - needed for message notifications
    pulseaudio alsa-base alsa-utils \
    gstreamer1.0-pulseaudio gstreamer1.0-alsa \
    # Media conversion (WXGF→JPEG/GIF, SILK→MP3)
    ffmpeg \
    # Clipboard (for paste-image tool)
    xclip \
    # Transparent proxy
    redsocks iptables \
    # SQLCipher build deps (temp, for compiling sqlcipher CLI)
    libssl-dev build-essential tcl \
    # Python packages for tools
    python3-pip \
    && rm -rf /var/lib/apt/lists/*

# ============================================
# Python packages
# ============================================
RUN pip3 install frida-tools silk-python

# ============================================
# SQLCipher 4 CLI (needed for wechat-db reads)
# ============================================
RUN cd /tmp \
    && curl -L https://github.com/sqlcipher/sqlcipher/archive/refs/tags/v4.6.1.tar.gz | tar xz \
    && cd sqlcipher-4.6.1 \
    && ./configure --enable-tempstore=yes CFLAGS="-DSQLITE_HAS_CODEC -DSQLCIPHER_CRYPTO_OPENSSL" LDFLAGS="-lcrypto" \
    && make -j$(nproc) \
    && cp sqlcipher /usr/local/bin/sqlcipher \
    && cd / && rm -rf /tmp/sqlcipher-4.6.1

# ============================================
# X11 socket directory
# ============================================
RUN mkdir -p /tmp/.X11-unix && chown root:root /tmp/.X11-unix && chmod 1777 /tmp/.X11-unix

# ============================================
# WeChat
# If docker/wechat.deb exists in the build context, it is used directly.
# Otherwise the .deb is downloaded at build time (CI path).
# Run `pnpm download:wechat` to cache locally for faster builds.
# ============================================
ARG TARGETARCH
COPY wechat.de[b] /tmp/
RUN if [ -f /tmp/wechat.deb ]; then \
      echo "Installing WeChat from local .deb"; \
    else \
      ARCH_SUFFIX=$([ "$TARGETARCH" = "arm64" ] && echo "arm64" || echo "x86_64") \
      && echo "Downloading WeChat for ${ARCH_SUFFIX}..." \
      && curl -L -o /tmp/wechat.deb "https://dldir1v6.qq.com/weixin/Universal/Linux/WeChatLinux_${ARCH_SUFFIX}.deb"; \
    fi \
    && (dpkg -i /tmp/wechat.deb || (apt-get update && apt-get install -y -f)) \
    && rm -rf /var/lib/apt/lists/* /tmp/wechat.deb

RUN useradd -m -s /bin/bash wechat

# ============================================
# Tool scripts (screenshot, click, type, key, scroll, a11y-dump, etc.)
# ============================================
RUN mkdir -p /opt/tools
COPY tools/ /opt/tools/
RUN chmod +x /opt/tools/*

# ============================================
# Copy Rust binary from builder
# ============================================
COPY --from=builder /output/agent-server /opt/agent-server/agent-server

# ============================================
# Debug support (gdbserver for remote debugging)
# ============================================
ARG BUILD_MODE=release
RUN if [ "$BUILD_MODE" = "debug" ]; then \
      apt-get update && apt-get install -y gdbserver && rm -rf /var/lib/apt/lists/*; \
    fi

# ============================================
# Data directory for SQLite
# ============================================
RUN mkdir -p /data && chown wechat:wechat /data
VOLUME /data

# ============================================
# Environment
# ============================================
ENV DISPLAY=:99
ENV PATH="/opt/tools:$PATH"
ENV AGENT_DB_PATH=/data/agent.db
ENV AGENT_PORT=6174

# ============================================
# Entrypoint
# ============================================
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 6174 5900 1234

ENTRYPOINT ["/entrypoint.sh"]
