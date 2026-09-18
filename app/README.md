# Bonsai Studio

A start/stop button, an options dialog, and an OpenAI-compatible API — in front
of `llama-server`, which does the actual serving.

## What it is

A Tauri app that supervises `llama-server` as a child process. It does not link
`libllama`. The server owns inference, the HTTP API, and the chat UI; this app
owns the settings, the lifecycle, and getting out of the way.

Running the server out-of-process is the point, not an accident. Model loads
OOM — a 100K context on a 12 GiB card sits close enough to the limit that it
happens in ordinary use — and a CUDA OOM aborts the process it occurs in. Out
here that is a red light and a restart button; linked in, it would take the
settings window down too, leaving no way to lower the context and retry.

## Running it

```bash
cd src-tauri
cargo build          # or: cargo tauri dev
./target/debug/bonsai-studio
```

It finds `llama-server` by checking, in order: the path in Settings,
`$BONSAI_SERVER_BIN`, the bundled copy in the app's resources, then
`src/llama.cpp/build/bin/` in this checkout, then `PATH`.

## Layout

| File | Role |
| --- | --- |
| `src-tauri/src/config.rs` | Settings, profiles, and the settings → CLI flag mapping |
| `src-tauri/src/server.rs` | Spawn, health poll, SIGTERM/SIGKILL, log capture, port check |
| `src-tauri/src/models.rs` | Variant discovery and resumable downloads |
| `src-tauri/src/vram.rs` | Pre-flight VRAM estimate |
| `src-tauri/src/lib.rs` | Tauri commands and window wiring |
| `src/` | Settings UI (plain HTML/CSS/JS, no bundler) |

## Where things live

| | |
| --- | --- |
| Models | `~/.local/share/bonsai-studio/models`, configurable |
| Settings | `~/.config/bonsai-studio/settings.json` |
| API | `http://127.0.0.1:11435/v1` |

On first run the app looks for already-downloaded weights in the configured
model directory and in this checkout's `models/`, so an existing 8 GB download
is adopted rather than fetched again.

## Downloading

The Download button fetches the selected variant's weights and projector.
Progress shows bytes, percentage, rate and ETA.

An interrupted transfer resumes: bytes land in a `.part` file and a retry issues
a Range request from where it stopped, which matters at 7.6 GB. Cancelling keeps
the partial file on purpose — starting again continues it.

**Re-download** forces a fresh copy. It takes two clicks, deletes the existing
weights and any partial file first, and is refused while the server is running,
since unlinking weights out from under `llama-server` leaves it holding an
unlinked inode and the next start loading a half-written file.

Start is disabled while a download runs. Stop is not: a running server and a
download do not conflict.

Downloads land in the directory the variant already occupies, falling back to
one holding a partial file, and only then to the configured model directory. The
resolution has to be stable across a forced re-download — keying it off "is it
installed" alone sends the retry somewhere else and strands the partial file.

## Profiles

Context size, KV cache type, and vision-tower placement have to move together;
exposing them as independent controls invites combinations that load and then
OOM partway through a long prompt.

| Profile | Context | KV cache | Vision tower | Est. peak |
| --- | --- | --- | --- | --- |
| Default | 16K | f16 | GPU | ~9 GB |
| 64K | 64K | q4_0 | system RAM | ~9.4 GB |
| 100K | 100K | q4_0 | system RAM | ~10.1 GB |
| Max | 262K | q4_0 | system RAM | ~12.8 GB |

Custom unlocks the individual fields. The estimate under the form is checked
against free VRAM before the server starts.

## API and access

Loopback already reaches every application on this machine, so "This PC" needs
no network exposure at all. Switching to "My network" binds `0.0.0.0` and
requires an API key — unauthenticated on loopback is defensible, unauthenticated
on a LAN is not. CORS defaults to allow-all because the common case here is
local development against the endpoint.

The default port is 11435. That is Ollama+1, which makes it memorable but does
collide with the conventional port for a second Ollama instance, so the
port-in-use error names that case explicitly.

## Packaging

```bash
../scripts/stage-sidecar.sh   # copy llama-server + its libraries into resources/
cd src-tauri && cargo tauri build
```

The staging script copies only libraries that ship with llama.cpp. System
libraries are left to the host deliberately: a CUDA build depends on
`libcublasLt` alone at ~517 MB. For a bundle that travels, build llama.cpp with
`-DGGML_VULKAN=ON` — Vulkan runs on NVIDIA, AMD, and Intel, and its runtime
comes from the user's driver.

On Linux the app needs WebKitGTK present for its own settings window; the `deb`
target declares it, and the AppImage target bundles it at the cost of size.
