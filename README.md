# Bonsai Studio

A desktop app for running [Bonsai 27B](https://docs.prismml.com/models/bonsai-27b)
locally: a start/stop button, an options dialog, and an OpenAI-compatible API on
`http://127.0.0.1:11435/v1`.

Bonsai 27B is a Qwen3.6-27B derivative quantized end-to-end to ternary or binary
weights. The ternary build is ~7.2 GB deployed and keeps roughly 95% of FP16
quality, which is what puts a 27B model on a single consumer GPU.

![Bonsai Studio serving Ternary Bonsai 27B at a 100K context on a 12 GiB RTX 3060](docs/screenshot.png)

## Install

Download the build for your platform from
[Releases](https://github.com/arkady-emelyanov/bonsai-studio/releases). Model
weights are fetched on first run, not bundled.

Linux and Windows builds use the Vulkan backend, so they work on NVIDIA, AMD and
Intel GPUs with no CUDA install. macOS is Apple Silicon only and uses Metal.
The binaries are unsigned: macOS needs right-click → Open, and Windows
SmartScreen warns on first launch.

## Using it

Pick a variant, press **Download**, then **Start**. **Open chat** opens
llama-server's own web UI — image upload, reasoning-effort picker, MCP client —
in your browser.

Point any OpenAI client at `http://127.0.0.1:11435/v1`; the API key is ignored
unless you set one. The endpoint serves the same model, with vision and native
tool calling.

## Memory

Published peak figures for the ternary build, and what they mean for a 12 GiB
card:

| Profile | Context | KV cache | Est. peak |
| --- | --- | --- | --- |
| Default | 16K | f16 | ~9 GB |
| 64K | 64K | q4_0 | ~9.4 GB |
| 100K | 100K | q4_0 | ~10.1 GB |
| Max | 262K | q4_0 | ~12.8 GB |

Only 16 of the model's 64 blocks use full attention — the rest are linear —
which is why the cache grows so slowly. The app estimates peak usage and warns
before starting if it exceeds free VRAM.

## Building

```bash
scripts/fetch-sidecar.sh linux-x64   # or macos-arm64, windows-x64
cd app/src-tauri && cargo tauri build
```

The sidecar script stages `llama-server` and its libraries from the pinned
upstream release in `.llama-cpp-version`. See [app/README.md](app/README.md) for
the architecture and the settings model.

## Releasing

Push a version tag; `.github/workflows/release.yml` builds all four targets,
assembles a draft release, and publishes it once every platform succeeds.

```bash
git tag v1.0.0 && git push origin v1.0.0
```

## Running llama.cpp directly

The `scripts/` directory drives llama.cpp without the app — useful for
benchmarking or when you want a specific backend:

```bash
scripts/00-build.sh        # clone and build llama.cpp with CUDA
scripts/01-download.sh     # fetch weights into models/
BONSAI_PROFILE=64k scripts/03-server.sh
```

These build from source and default to CUDA, unlike the released app.

## Licence

The app is [MIT](LICENSE). llama.cpp is MIT and its binaries ship inside the
release bundles, carrying their own licence file. The Bonsai weights are
Apache-2.0 and are downloaded from Hugging Face at runtime, not redistributed
here.
