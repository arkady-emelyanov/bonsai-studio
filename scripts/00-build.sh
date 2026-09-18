#!/usr/bin/env bash
# Clone and build llama.cpp with CUDA support.
#
# Stock upstream llama.cpp is enough for Bonsai 27B: both Q1_0 (1-bit) and the
# group-64 Q2_0 (ternary) kernels are merged there. Only Ternary Bonsai 2
# (PTQ1_0/PQ2_0) would need the PrismML fork -- see README.md.

source "$(dirname "${BASH_SOURCE[0]}")/bonsai.env"

REPO_URL="${BONSAI_REPO_URL:-https://github.com/ggml-org/llama.cpp}"
REPO_BRANCH="${BONSAI_REPO_BRANCH:-master}"

# CUDA builds are memory-hungry; the upstream guidance is to cap parallelism on
# cards below 16 GiB of VRAM.
JOBS="${BONSAI_BUILD_JOBS:-2}"

# sm_86 = RTX 30-series. Building a single architecture is much faster than the
# default fat binary; drop this to build for every supported GPU.
CUDA_ARCH="${BONSAI_CUDA_ARCH:-86}"

if [[ -d "$SRC_DIR/.git" ]]; then
  echo "==> Updating $SRC_DIR"
  git -C "$SRC_DIR" fetch --depth 1 origin "$REPO_BRANCH"
  git -C "$SRC_DIR" checkout -B "$REPO_BRANCH" FETCH_HEAD
else
  echo "==> Cloning $REPO_URL ($REPO_BRANCH) into $SRC_DIR"
  mkdir -p "$(dirname "$SRC_DIR")"
  git clone --depth 1 -b "$REPO_BRANCH" "$REPO_URL" "$SRC_DIR"
fi

echo "==> Configuring (CUDA, sm_$CUDA_ARCH)"
cmake -S "$SRC_DIR" -B "$SRC_DIR/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DGGML_CUDA=ON \
  -DCMAKE_CUDA_ARCHITECTURES="$CUDA_ARCH" \
  -DLLAMA_CURL=ON

echo "==> Building with -j$JOBS (this takes a while)"
cmake --build "$SRC_DIR/build" -j "$JOBS"

echo
echo "Binaries in $BIN_DIR:"
ls "$BIN_DIR" | grep -E '^llama-(cli|server|mtmd-cli|bench)$' || true
