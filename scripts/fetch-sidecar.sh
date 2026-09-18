#!/usr/bin/env bash
# Download a prebuilt llama.cpp release and stage llama-server plus its shared
# libraries as the Tauri sidecar.
#
# Building llama.cpp from source in CI would cost 20-40 minutes per platform and
# still leave the CUDA question open, so release binaries are used instead. The
# version is pinned in .llama-cpp-version rather than tracking "latest", so a
# tagged build is reproducible and upstream cannot break a release unannounced.
#
#   scripts/fetch-sidecar.sh <platform>
#
# Platforms: linux-x64, macos-arm64, windows-x64 (Apple Silicon only on macOS)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLATFORM="${1:?usage: fetch-sidecar.sh <linux-x64|macos-arm64|macos-x64|windows-x64>}"
TAG="${LLAMA_CPP_VERSION:-$(tr -d '[:space:]' < "$ROOT/.llama-cpp-version")}"
DEST="${BONSAI_STAGE_DIR:-$ROOT/app/src-tauri/resources/llama}"

# Vulkan on Linux and Windows: one build covers NVIDIA, AMD and Intel, and the
# runtime comes from the user's driver rather than the bundle. macOS ships Metal
# in the default build.
case "$PLATFORM" in
  linux-x64)   ASSET="llama-${TAG}-bin-ubuntu-vulkan-x64.tar.gz" ;;
  macos-arm64) ASSET="llama-${TAG}-bin-macos-arm64.tar.gz" ;;
  macos-x64)   ASSET="llama-${TAG}-bin-macos-x64.tar.gz" ;;
  windows-x64) ASSET="llama-${TAG}-bin-win-vulkan-x64.zip" ;;
  *) echo "Unknown platform '$PLATFORM'" >&2; exit 1 ;;
esac

URL="https://github.com/ggml-org/llama.cpp/releases/download/${TAG}/${ASSET}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> $ASSET"
curl -fL --retry 3 --retry-delay 5 -o "$WORK/$ASSET" "$URL"

# Git Bash on Windows ships GNU tar, which cannot read zip archives, so the
# extraction path has to be chosen rather than assumed.
extract_zip() {
  local archive="$1" dest="$2"
  if command -v unzip >/dev/null 2>&1; then
    unzip -q "$archive" -d "$dest"
  elif [[ -x /c/Windows/System32/tar.exe ]]; then
    # The system bsdtar does handle zip, unlike the MSYS GNU tar on PATH.
    /c/Windows/System32/tar.exe -xf "$(cygpath -w "$archive")" -C "$(cygpath -w "$dest")"
  elif command -v powershell.exe >/dev/null 2>&1; then
    powershell.exe -NoProfile -NonInteractive -Command \
      "Expand-Archive -Path '$(cygpath -w "$archive")' -DestinationPath '$(cygpath -w "$dest")' -Force"
  else
    # macOS and most Linux distributions ship bsdtar or a tar that copes.
    tar -xf "$archive" -C "$dest"
  fi
}

# The tarballs wrap everything in llama-<tag>/; the Windows zip is flat.
mkdir -p "$WORK/x"
if [[ "$ASSET" == *.zip ]]; then
  extract_zip "$WORK/$ASSET" "$WORK/x"
else
  tar -xzf "$WORK/$ASSET" -C "$WORK/x" --strip-components=1
fi

rm -rf "$DEST"
mkdir -p "$DEST"

# Keep llama-server and every shared library; drop the other CLI tools, which
# would roughly double the bundle for no benefit.
shopt -s nullglob
for f in "$WORK"/x/*; do
  base="$(basename "$f")"
  case "$base" in
    llama-server|llama-server.exe|*.so|*.so.*|*.dylib|*.dll|*.metallib|LICENSE*)
      cp -a "$f" "$DEST/" ;;
  esac
done

if [[ ! -e "$DEST/llama-server" && ! -e "$DEST/llama-server.exe" ]]; then
  echo "llama-server missing from $ASSET" >&2
  exit 1
fi
chmod +x "$DEST"/llama-server 2>/dev/null || true

echo "Staged $PLATFORM from $TAG:"
ls "$DEST" | wc -l | xargs echo "  files:"
du -sh "$DEST" | awk '{print "  size: " $1}'
