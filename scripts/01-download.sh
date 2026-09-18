#!/usr/bin/env bash
# Download the Bonsai 27B GGUF weights plus the vision projector.
#
#   ./01-download.sh                        # ternary, 1.58-bit (6.66 GiB + 0.63 GiB)
#   BONSAI_FAMILY=bonsai ./01-download.sh   # 1-bit          (3.53 GiB + 0.63 GiB)
#
# The repos are public; no Hugging Face token is needed. Downloads resume, so
# re-running after an interrupted transfer is safe.

source "$(dirname "${BASH_SOURCE[0]}")/bonsai.env"

mkdir -p "$MODEL_DIR"

download() {
  local file="$1"
  local dest="$MODEL_DIR/$file"
  local url="https://huggingface.co/$HF_REPO/resolve/main/$file?download=true"

  if [[ -f "$dest" ]]; then
    echo "==> $file already present, skipping"
    return
  fi

  echo "==> Downloading $file from $HF_REPO"
  # Write to a .part file so an interrupted run never leaves a truncated GGUF
  # that looks complete to the run scripts.
  curl -L --fail --progress-bar -C - -o "$dest.part" "$url"
  mv "$dest.part" "$dest"
}

download "$MODEL_FILE"
download "$MMPROJ_FILE"

echo
du -h "$MODEL_PATH" "$MMPROJ_PATH"
