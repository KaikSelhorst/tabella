#!/usr/bin/env bash
# Puts pinned, checksum-verified AppImage tools where cargo-packager looks for them, so it never
# downloads anything itself (its defaults include moving "continuous" builds).
#
#   source .github/scripts/appimage-tools.sh && cargo packager --release
#
# To update: pick a newer *versioned* release (never "continuous"), download the asset and put
# its `sha256sum` here.
set -euo pipefail

tools="${XDG_CACHE_HOME:-$HOME/.cache}/.cargo-packager/AppImage"
mkdir -p "$tools"

fetch() { # name url sha256
    local dest="$tools/$1"
    if ! echo "$3  $dest" | sha256sum --check --status 2>/dev/null; then
        curl -sSfL --retry 3 -o "$dest.part" "$2"
        echo "$3  $dest.part" | sha256sum --check --quiet
        mv "$dest.part" "$dest"
    fi
    chmod +x "$dest"
}

fetch linuxdeploy-x86_64.AppImage \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage \
    c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
fetch linuxdeploy-plugin-appimage.AppImage \
    https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-appimage-x86_64.AppImage \
    992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3
fetch AppRun-x86_64 \
    https://github.com/tauri-apps/binary-releases/releases/download/apprun-old/AppRun-x86_64 \
    f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f
# The AppImage runtime: appimagetool would otherwise fetch the "continuous" one.
fetch runtime-x86_64 \
    https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
    2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

export LDAI_RUNTIME_FILE="$tools/runtime-x86_64"
# The tools are AppImages themselves; CI runners (and WSL) have no FUSE to mount them.
export APPIMAGE_EXTRACT_AND_RUN=1
if [ -n "${GITHUB_ENV:-}" ]; then
    echo "LDAI_RUNTIME_FILE=$LDAI_RUNTIME_FILE" >> "$GITHUB_ENV"
    echo "APPIMAGE_EXTRACT_AND_RUN=1" >> "$GITHUB_ENV"
fi
