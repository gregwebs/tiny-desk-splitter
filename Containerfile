# Containerfile — OCI-compatible multi-stage build
#
# Build targets (use --target <name>):
#   base     — runtime deps only (ffmpeg, yt-dlp).   tag: tiny-desk-base
#   dev      — base + Rust + C++ toolchain for dev.  tag: tiny-desk-dev
#   release  — compiled binaries on top of base.     tag: tiny-desk
#
# Works with:  docker build  |  podman build  |  buildah bud
#
# ── build-time requirements ───────────────────────────────────────────────────
# The default paddle-ocr backend downloads a prebuilt static MNN library and
# three .mnn OCR model files during `cargo build`.  Build-time egress to:
#   github.com              (prebuilt MNN release — tag "dev", not pinned)
#   raw.githubusercontent.com  (OCR model files)
# is MANDATORY.  --network=none builds will fail at the cargo build step.
# Because the MNN release uses a moving "dev" tag, two builds at different
# times may produce slightly different binaries.
# ─────────────────────────────────────────────────────────────────────────────

# Bump these ARGs to update pinned versions without touching the stages below.
ARG DEBIAN_VERSION=bookworm-slim
# yt-dlp breaks regularly as YouTube changes.  Rebuild the base/release image
# when downloads stop working and bump this version.
ARG YT_DLP_VERSION=2025.06.09
ARG RUST_VERSION=1.92


# ─────────────────────────────────────────────────────────────────────────────
# Stage: base
#   Runtime dependencies only:
#     ffmpeg / ffprobe  — splitting, frame analysis, media probing
#     python3           — yt-dlp runtime dep (the GitHub binary is a zipapp)
#     ca-certificates   — TLS roots for yt-dlp HTTPS downloads
#     yt-dlp            — concert video downloader (pinned GitHub binary)
#   curl is installed temporarily to fetch the yt-dlp binary, then purged so
#   it does not appear in the final image layer.
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:${DEBIAN_VERSION} AS base

ARG YT_DLP_VERSION

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ffmpeg \
        python3 \
        ca-certificates \
        curl \
    && arch="$(uname -m)" \
    && case "$arch" in \
        x86_64)  ytdlp_asset="yt-dlp_linux" ;; \
        aarch64) ytdlp_asset="yt-dlp_linux_aarch64" ;; \
        *) echo "Unsupported arch: $arch" >&2; exit 1 ;; \
    esac \
    && curl -fsSL \
        "https://github.com/yt-dlp/yt-dlp/releases/download/${YT_DLP_VERSION}/${ytdlp_asset}" \
        -o /usr/local/bin/yt-dlp \
    && chmod +x /usr/local/bin/yt-dlp \
    && apt-get purge -y curl \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*


# ─────────────────────────────────────────────────────────────────────────────
# Stage: dev  (FROM base — inherits all runtime tools)
#   Adds the full build toolchain:
#     build-essential, g++      — C compiler + C++ compiler (cc crate wrapper)
#     clang, libclang-dev       — libclang required by bindgen (ocr-rs)
#     pkg-config                — used by various build.rs scripts
#     curl, ca-certificates     — build.rs downloads MNN prebuilt + OCR models
#     git                       — some build scripts probe the git tree
#     Rust (via rustup)         — pinned to RUST_VERSION
#
#   cmake is intentionally omitted: vendor/ocr-rs only invokes cmake on the
#   MNN source-build fallback path, which is never triggered for linux/x86_64
#   or linux/aarch64 (the prebuilt path is used instead).
# ─────────────────────────────────────────────────────────────────────────────
FROM base AS dev

ARG RUST_VERSION

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        g++ \
        clang \
        libclang-dev \
        pkg-config \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl -fsSL https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain ${RUST_VERSION} \
    && rustup component add rustfmt clippy


# ─────────────────────────────────────────────────────────────────────────────
# Stage: builder  (intermediate — not published as a named target)
#   Compiles the workspace in release mode with the default paddle-ocr feature.
#
#   Requires build-time network (see header comment).  live-set-song-splitter/
#   build.rs writes the downloaded .mnn model files into the source tree at
#   live-set-song-splitter/models/ — that directory is collected by the release
#   stage below.
#
#   Note: cargo build is NOT split into a deps-only pre-cache step here.  With
#   lto=true + codegen-units=1 the release build is slow regardless, and the
#   extra complexity of cargo-chef isn't worth it yet.
# ─────────────────────────────────────────────────────────────────────────────
FROM dev AS builder

WORKDIR /build
COPY . .

RUN cargo build --release


# ─────────────────────────────────────────────────────────────────────────────
# Stage: release  (FROM base — lean runtime image)
#   Copies only the compiled binaries and OCR model files; all build tooling
#   stays in the builder stage.
#
#   Model placement: live-set-splitter resolves models in this order:
#     1. $PADDLE_OCR_MODEL_DIR  (env var, set below)
#     2. models/ beside current_exe()
#     3. CARGO_MANIFEST_DIR/models (source-tree fallback, not present here)
#   OCR inference runs inside the *spawned* live-set-splitter subprocess, not
#   inside concert-web.  All binaries and models must therefore be co-located
#   under /app so that candidate #2 also works as belt-and-suspenders.
#
#   Entrypoint defaults:
#     --host 0.0.0.0       listen on all interfaces (required for container)
#     --open-cmd true      neutralize macOS `open` for headless use
#   Override individual CMD args by appending them:
#     docker run tiny-desk --port 8080
#   Use --entrypoint for the other CLIs:
#     docker run --entrypoint /app/concert-db tiny-desk list
# ─────────────────────────────────────────────────────────────────────────────
FROM base AS release

COPY --from=builder \
    /build/target/release/concert-web \
    /build/target/release/concert-db \
    /build/target/release/live-set-splitter \
    /build/target/release/scraper \
    /build/target/release/archive_scraper \
    /app/

COPY --from=builder /build/live-set-song-splitter/models/ /app/models/

# Resolution candidate #1: env var override (belt-and-suspenders alongside
# the models/ sibling placement above).
ENV PADDLE_OCR_MODEL_DIR=/app/models

WORKDIR /data
EXPOSE 3000

ENTRYPOINT ["/app/concert-web"]
CMD ["--host", "0.0.0.0", "--db", "/data/concerts.db", "--workdir", "/data", "--open-cmd", "true"]
