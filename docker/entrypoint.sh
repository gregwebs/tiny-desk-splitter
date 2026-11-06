#!/bin/sh
# dev-entrypoint — seed a bind-mounted checkout from the assets baked into this image.
#
# The image downloads two kinds of network artifact at build time so that a
# container with no egress can still build and test the workspace:
#
#   /opt/ocr-cache/mnn/     prebuilt MNN tarball, under the exact name ocr-rs's
#                           build.rs looks for
#   /opt/ocr-cache/models/  PaddleOCR model files, same
#   /opt/npm-cache/         npm cache holding every tarball `npm ci` needs
#
# Those caches can't be baked into their final locations, because the workspace
# is a bind mount that shadows whatever the image has at /work. So seed them on
# start, and only where the checkout is missing something:
#
#   * OCR assets become symlinks into the (gitignored) cache paths the build
#     scripts check before downloading. A symlink makes `Path::exists()` true,
#     which is all either script tests.
#   * node_modules is installed from the offline cache, since a copy would need
#     to be re-made on every start.
#
# Nothing here touches a path git tracks. Set DEV_ENTRYPOINT_SEED=0 to skip it.
set -eu

WORKSPACE="${WORKSPACE_DIR:-/work}"
OCR_CACHE="${OCR_CACHE:-/opt/ocr-cache}"

seed_ocr() {
    [ -f "$WORKSPACE/live-set-song-splitter/Cargo.toml" ] || return 0
    [ -d "$OCR_CACHE" ] || return 0

    prebuilt="$WORKSPACE/live-set-song-splitter/vendor/ocr-rs/3rd_party/prebuilt"
    if [ -d "$OCR_CACHE/mnn" ]; then
        mkdir -p "$prebuilt"
        for archive in "$OCR_CACHE"/mnn/*.tar.gz; do
            [ -f "$archive" ] || continue
            target="$prebuilt/$(basename "$archive")"
            if [ ! -e "$target" ]; then
                ln -s "$archive" "$target"
                echo "dev-entrypoint: linked $(basename "$archive") into the ocr-rs prebuilt cache"
            fi
        done
    fi

    if [ -d "$OCR_CACHE/models" ]; then
        models="$WORKSPACE/live-set-song-splitter/models"
        mkdir -p "$models"
        for model in "$OCR_CACHE"/models/*; do
            [ -f "$model" ] || continue
            target="$models/$(basename "$model")"
            if [ ! -e "$target" ]; then
                ln -s "$model" "$target"
                echo "dev-entrypoint: linked $(basename "$model") into live-set-song-splitter/models"
            fi
        done
    fi
}

seed_node_modules() {
    [ -f "$1/package-lock.json" ] || return 0
    [ -d "$1/node_modules" ] && return 0
    echo "dev-entrypoint: installing JS dependencies in $1 from the offline npm cache"
    if ! ( cd "$1" && npm ci --offline --no-audit --no-fund >/dev/null ); then
        echo "dev-entrypoint: offline npm install failed in $1" >&2
        echo "dev-entrypoint: the lockfile has probably moved past this image — run 'npm ci' with network" >&2
    fi
}

if [ "${DEV_ENTRYPOINT_SEED:-1}" != "0" ] && [ -d "$WORKSPACE" ]; then
    seed_ocr
    seed_node_modules "$WORKSPACE"
    seed_node_modules "$WORKSPACE/concert-tracker/frontend"
fi

exec "$@"
