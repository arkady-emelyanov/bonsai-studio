#!/usr/bin/env bash
# Stage llama-server and its shared libraries into the Tauri app's resources,
# so `cargo tauri build` produces a bundle that carries its own runtime.
#
# Only libraries that ship with llama.cpp are copied. System libraries (libc,
# libstdc++, the CUDA runtime, the Vulkan loader) are deliberately left to the
# host: bundling cuBLAS alone would add ~570 MB.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BONSAI_BIN_DIR:-$ROOT/src/llama.cpp/build/bin}"
DEST="${BONSAI_STAGE_DIR:-$ROOT/app/src-tauri/resources/llama}"

if [[ ! -x "$BIN_DIR/llama-server" ]]; then
  echo "No llama-server in $BIN_DIR -- run scripts/00-build.sh first." >&2
  exit 1
fi

mkdir -p "$DEST"
rm -f "$DEST"/llama-server "$DEST"/*.so*

cp -v "$BIN_DIR/llama-server" "$DEST/"

# Follow the dependency chain, keeping only libraries from the build directory.
ldd "$BIN_DIR/llama-server" \
  | awk '{for (i=1;i<=NF;i++) if ($i ~ /^\//) print $i}' \
  | grep -F "$BIN_DIR" \
  | sort -u \
  | while read -r lib; do
      # -a keeps symlinks as symlinks. Dereferencing here would copy both
      # libfoo.so and libfoo.so.0.24.0 in full, doubling the bundle.
      cp -a "$lib" "$DEST/"
      real="$(readlink -f "$lib")"
      [[ "$real" != "$lib" ]] && cp -an "$real" "$DEST/" 2>/dev/null || true
    done

# ggml loads its backends by name at runtime when built with GGML_BACKEND_DL,
# so they are not in ldd output but still have to ship.
for backend in "$BIN_DIR"/libggml-*.so*; do
  [[ -e "$backend" ]] || continue
  cp -an "$backend" "$DEST/" 2>/dev/null || true
done

echo
echo "Staged into $DEST:"
du -ch "$DEST"/* | tail -1
echo
echo "Note: a CUDA build pulls in libcublas/libcublasLt from the system (~570 MB)."
echo "For a portable bundle, build llama.cpp with -DGGML_VULKAN=ON instead."
