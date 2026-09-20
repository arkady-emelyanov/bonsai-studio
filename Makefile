# Bonsai Studio -- development entry points.
#
# The app is a Tauri crate under app/src-tauri with a static frontend, so every
# target here is a thin wrapper over cargo. The one piece of state that is not
# cargo's is the sidecar: llama-server has to be staged into the crate's
# resources before a run or a build carries a working runtime.

TAURI := cd app/src-tauri && cargo tauri
CARGO := cd app/src-tauri && cargo

# Which prebuilt sidecar archive to fetch. Overridable for cross-checking
# another platform's bundle: make sidecar PLATFORM=macos-arm64
UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
ifeq ($(UNAME_S),Darwin)
  PLATFORM ?= macos-arm64
else ifeq ($(UNAME_M),x86_64)
  PLATFORM ?= linux-x64
else
  PLATFORM ?= linux-x64
endif

SIDECAR := app/src-tauri/resources/llama/llama-server

.PHONY: run build test check fmt sidecar clean help

help:
	@echo "run      start the app (debug, hot-reloads the frontend)"
	@echo "build    release bundles in app/src-tauri/target/release/bundle"
	@echo "test     cargo test"
	@echo "check    cargo check + clippy"
	@echo "fmt      cargo fmt"
	@echo "sidecar  stage llama-server for $(PLATFORM) (re-run after changing .llama-cpp-version)"
	@echo "clean    cargo clean; leaves the staged sidecar in place"

run: $(SIDECAR)
	$(TAURI) dev

build: $(SIDECAR)
	$(TAURI) build

test:
	$(CARGO) test

check:
	$(CARGO) check
	$(CARGO) clippy

fmt:
	$(CARGO) fmt

# Forced re-stage. The rule below only fires when nothing is staged at all, so
# this is how a pin bump in .llama-cpp-version reaches the resources directory.
sidecar:
	./scripts/fetch-sidecar.sh $(PLATFORM)

# A missing sidecar is fetched automatically, since a bundle built without one
# starts and then fails at the first Start with "Could not find llama-server".
$(SIDECAR):
	./scripts/fetch-sidecar.sh $(PLATFORM)

clean:
	$(CARGO) clean
