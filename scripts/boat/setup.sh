#!/usr/bin/env bash
# Prepare a Boat Ubuntu 24.04 sandbox to build and test agent-wechat.
# Reads pinned versions directly from the repository so no Boat-side env config is needed.
# Idempotent: subsequent runs skip already-installed tools and unchanged dependencies.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STATE_DIR="$HOME/.local/share/agent-wechat-boat"
PNPM_CLI_DIR="$HOME/.local/share/pnpm-cli"
ENV_FILE="$STATE_DIR/env.sh"
LOCK_HASH_FILE="$STATE_DIR/pnpm-lock.sha256"

INSTALL_RUST=1
INSTALL_NODE=1
SKIP_PNPM_INSTALL=0
WITH_DOCS_FONTS=0

for arg in "$@"; do
  case "$arg" in
    --rust-only) INSTALL_NODE=0 ;;
    --node-only) INSTALL_RUST=0 ;;
    --skip-pnpm-install) SKIP_PNPM_INSTALL=1 ;;
    --with-docs-fonts) WITH_DOCS_FONTS=1 ;;
    -h|--help)
      cat <<'EOF'
Usage: bash scripts/boat/setup.sh [options]

Options:
  --rust-only          Skip Node/pnpm setup and pnpm install
  --node-only          Skip Rust toolchain setup
  --skip-pnpm-install  Ensure pnpm is installed, but skip `pnpm install`
  --with-docs-fonts    Also install fonts-noto-cjk (needed only for docs builds)
EOF
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 2
      ;;
  esac
done

mkdir -p "$STATE_DIR" "$HOME/.local/bin"

if [ -z "${CARGO_HOME:-}" ] && [ -x /usr/local/cargo/bin/rustup ]; then
  export CARGO_HOME=/usr/local/cargo
fi
if [ -z "${RUSTUP_HOME:-}" ] && [ -d /usr/local/rustup ]; then
  export RUSTUP_HOME=/usr/local/rustup
fi

# nvm.sh and nvm subcommands are not `set -eu` safe; load them with -eu temporarily off,
# then prepend ~/.local/bin AFTER nvm so our pinned pnpm binary is never shadowed.
if [ -s "$HOME/.nvm/nvm.sh" ]; then
  export NVM_DIR="$HOME/.nvm"
  set +eu
  # shellcheck disable=SC1091
  . "$NVM_DIR/nvm.sh" >/dev/null 2>&1 || true
  set -eu
fi
export PATH="$HOME/.local/bin:/usr/local/cargo/bin:$PATH"

# 1. Read pinned versions directly from repository manifests.
RUST_TOOLCHAIN="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO_ROOT/rust-toolchain.toml" | head -n 1)"
NODE_VERSION="$(tr -d '[:space:]' < "$REPO_ROOT/.node-version")"
PNPM_VERSION="$(sed -n 's/.*"packageManager"[[:space:]]*:[[:space:]]*"pnpm@\([^"]*\)".*/\1/p' "$REPO_ROOT/package.json" | head -n 1)"

if [ -z "$RUST_TOOLCHAIN" ] || [ -z "$NODE_VERSION" ] || [ -z "$PNPM_VERSION" ]; then
  echo "Failed to parse versions from rust-toolchain.toml / .node-version / package.json" >&2
  exit 1
fi

# 2. Install missing OS build packages (Boat Ubuntu 24.04 already includes cmake, ffmpeg, libpulse).
MISSING_APT=()
command -v cmake >/dev/null 2>&1 || MISSING_APT+=(cmake)
if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
  MISSING_APT+=(ffmpeg)
fi
if [ "$WITH_DOCS_FONTS" -eq 1 ] && ! dpkg -s fonts-noto-cjk >/dev/null 2>&1; then
  MISSING_APT+=(fonts-noto-cjk)
fi

if [ "${#MISSING_APT[@]}" -gt 0 ]; then
  echo "[boat-setup] Installing apt packages: ${MISSING_APT[*]}"
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${MISSING_APT[@]}"
fi

# 3. Ensure pinned Rust toolchain + rustfmt + clippy + wasm32 target.
if [ "$INSTALL_RUST" -eq 1 ]; then
  NEED_RUST=0
  if ! rustup toolchain list 2>/dev/null | grep -q "^${RUST_TOOLCHAIN}-"; then
    NEED_RUST=1
  elif ! cargo +"$RUST_TOOLCHAIN" fmt --version >/dev/null 2>&1; then
    NEED_RUST=1
  elif ! cargo +"$RUST_TOOLCHAIN" clippy --version >/dev/null 2>&1; then
    NEED_RUST=1
  elif ! rustup target list --installed --toolchain "$RUST_TOOLCHAIN" 2>/dev/null | grep -qx "wasm32-unknown-unknown"; then
    NEED_RUST=1
  fi

  if [ "$NEED_RUST" -eq 1 ]; then
    echo "[boat-setup] Installing Rust $RUST_TOOLCHAIN (rustfmt, clippy, wasm32-unknown-unknown)..."
    rustup toolchain install "$RUST_TOOLCHAIN" \
      --profile minimal \
      --component rustfmt \
      --component clippy \
      --target wasm32-unknown-unknown
  fi
fi

# 4. Ensure pinned Node and pnpm (installed under ~/.local/share so Boat snapshots keep them).
if [ "$INSTALL_NODE" -eq 1 ]; then
  CURRENT_NODE="$(node -v 2>/dev/null || true)"
  if [ "$CURRENT_NODE" != "v${NODE_VERSION}" ]; then
    if command -v nvm >/dev/null 2>&1; then
      echo "[boat-setup] Installing Node v${NODE_VERSION} via nvm..."
      set +eu
      nvm install "$NODE_VERSION" >/dev/null
      nvm alias default "$NODE_VERSION" >/dev/null
      nvm use "$NODE_VERSION" >/dev/null
      set -eu
      export PATH="$HOME/.local/bin:/usr/local/cargo/bin:$PATH"
    else
      echo "[boat-setup] Warning: node is ${CURRENT_NODE:-missing}, expected v${NODE_VERSION} (nvm unavailable)" >&2
    fi
  fi

  # Run outside REPO_ROOT so pnpm 10's packageManager auto-download (into unpersisted ~/.cache)
  # does not mask installing the pinned pnpm binary under ~/.local/share.
  CURRENT_PNPM="$(cd / && pnpm --version 2>/dev/null || true)"
  if [ "$CURRENT_PNPM" != "$PNPM_VERSION" ]; then
    echo "[boat-setup] Installing pnpm@${PNPM_VERSION} into $PNPM_CLI_DIR..."
    mkdir -p "$PNPM_CLI_DIR"
    npm install --no-audit --no-fund --silent --prefix "$PNPM_CLI_DIR" "pnpm@${PNPM_VERSION}"
    ln -sf "$PNPM_CLI_DIR/node_modules/.bin/pnpm" "$HOME/.local/bin/pnpm"
    hash -r
  fi
fi

# 5. Persist build environment defaults for every subsequent `boat exec`.
cat > "$ENV_FILE" <<'EOF'
if [ -z "${CARGO_HOME:-}" ] && [ -x /usr/local/cargo/bin/rustup ]; then
  export CARGO_HOME=/usr/local/cargo
fi
if [ -z "${RUSTUP_HOME:-}" ] && [ -d /usr/local/rustup ]; then
  export RUSTUP_HOME=/usr/local/rustup
fi
if [ -s "$HOME/.nvm/nvm.sh" ]; then
  export NVM_DIR="$HOME/.nvm"
  _aw_prev_opts="$-"
  set +eu
  # shellcheck disable=SC1091
  . "$NVM_DIR/nvm.sh" >/dev/null 2>&1 || true
  case "$_aw_prev_opts" in *e*) set -e ;; esac
  case "$_aw_prev_opts" in *u*) set -u ;; esac
  unset _aw_prev_opts
fi
export PATH="$HOME/.local/bin:/usr/local/cargo/bin:$PATH"
export CARGO_PROFILE_DEV_OPT_LEVEL="${CARGO_PROFILE_DEV_OPT_LEVEL:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$(nproc 2>/dev/null || echo 4)}"
EOF

# 6. Install JS dependencies only when missing or pnpm-lock.yaml changed.
if [ "$INSTALL_NODE" -eq 1 ] && [ "$SKIP_PNPM_INSTALL" -eq 0 ]; then
  LOCK_HASH="$(sha256sum "$REPO_ROOT/pnpm-lock.yaml" | awk '{print $1}')"
  PREV_HASH="$(cat "$LOCK_HASH_FILE" 2>/dev/null || true)"
  if [ ! -d "$REPO_ROOT/node_modules" ] || [ "$LOCK_HASH" != "$PREV_HASH" ]; then
    echo "[boat-setup] Running pnpm install --frozen-lockfile..."
    (cd "$REPO_ROOT" && pnpm install --frozen-lockfile)
    printf '%s\n' "$LOCK_HASH" > "$LOCK_HASH_FILE"
  fi
fi
