#!/usr/bin/env bash
# Start the OpenAI-compatible server plus llama.cpp's built-in web chat.
#
#   ./03-server.sh                          # http://127.0.0.1:8080
#   BONSAI_CTX=32768 BONSAI_KV_TYPE=q8_0 ./03-server.sh
#
# Extra arguments pass through to llama-server, e.g. --reasoning-budget 2048 to
# cap how long the model is allowed to think by default.

source "$(dirname "${BASH_SOURCE[0]}")/bonsai.env"
require_binary llama-server
require_model
check_vram

# Bind to loopback only: llama-server has no authentication.
HOST="${BONSAI_HOST:-127.0.0.1}"
PORT="${BONSAI_PORT:-8080}"

echo "==> $MODEL_FILE on http://$HOST:$PORT (ctx $BONSAI_CTX, kv $BONSAI_KV_TYPE)"

exec "$BIN_DIR/llama-server" \
  "${COMMON_ARGS[@]}" \
  --host "$HOST" \
  --port "$PORT" \
  --alias bonsai-27b \
  "$@"
