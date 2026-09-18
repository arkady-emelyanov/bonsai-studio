#!/usr/bin/env bash
# One-shot generation or an interactive chat with Bonsai 27B.
#
#   ./02-chat.sh                                  # interactive
#   ./02-chat.sh -p "Explain KV cache growth."    # single prompt
#   ./02-chat.sh --image photo.jpg -p "Describe this."
#
# Any extra arguments are passed straight through to llama-cli.

source "$(dirname "${BASH_SOURCE[0]}")/bonsai.env"
require_binary llama-cli
require_model
check_vram

exec "$BIN_DIR/llama-cli" "${COMMON_ARGS[@]}" "$@"
