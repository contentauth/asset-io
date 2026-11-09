.PHONY: ci check-format fmt check-docs clippy test test-each-format test-c-ffi doc fuzz-build fuzz fuzz-parse fuzz-write fuzz-xmp fuzz-mpf fuzz-conform fuzz-regress

ifeq ($(OS),Windows_NT)
	PLATFORM := win
else
	UNAME := $(shell uname)
	ifeq ($(UNAME),Linux)
		PLATFORM := linux
	endif
	ifeq ($(UNAME),Darwin)
		PLATFORM := mac
	endif
endif

# Use cargo-nextest if installed (faster), otherwise fall back to cargo test.
# Install nextest with: cargo install --cargo-nextest
CARGO_TEST := $(shell cargo nextest --version > /dev/null 2>&1 && echo "cargo nextest run" || echo "cargo test")

# Formatting follows c2pa-rs: rustfmt.toml uses unstable options, so it needs
# nightly rustfmt (rustup toolchain add nightly).
# The root workspace includes asset-io-c; fuzz is a separate cargo-fuzz workspace.
FMT_MANIFESTS := Cargo.toml fuzz/Cargo.toml

# Run the full CI preflight locally (mirrors .github/workflows/ci.yml).
# Run this before pushing a PR.
ci: check-format check-docs clippy test

check-format:
	@set -e; for m in $(FMT_MANIFESTS); do \
		cargo +nightly fmt --manifest-path $$m --all -- --check; \
	done

# Auto format code according to standards
fmt:
	@set -e; for m in $(FMT_MANIFESTS); do \
		cargo +nightly fmt --manifest-path $$m --all; \
	done

check-docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

clippy:
	cargo clippy --all-features --all-targets -- -D warnings
	cargo clippy --no-default-features --all-targets -- -D warnings
	cargo clippy -p asset-io-c --all-features --all-targets -- -D warnings

test: check-format
	$(CARGO_TEST)
	cargo test --doc
	$(CARGO_TEST) --all-features
	cargo test --doc --all-features
	$(MAKE) test-c-ffi

# Test each built-in container feature in isolation, in addition to the usual
# default and all-features coverage in `make test`.
test-each-format:
	@set -e; for handler in jpeg png bmff riff svg pdf tiff c2pa gif id3; do \
		$(CARGO_TEST) --no-default-features --features $$handler; \
		cargo test --doc --no-default-features --features $$handler; \
	done

# Test the C bindings with no handlers and with every feature enabled.
test-c-ffi:
	cargo test -p asset-io-c
	cargo test -p asset-io-c --all-features

# Builds and views documentation as docs.rs shows it, with feature badges.
doc:
	RUSTDOCFLAGS="--cfg docsrs" cargo +nightly doc --no-deps --all-features --open

# Fuzzing needs nightly and cargo-fuzz (cargo install cargo-fuzz); see fuzz/README.md.
# FUZZ_TIME is seconds per target.
FUZZ_TIME ?= 60

fuzz-build:
	./fuzz.sh build

fuzz:
	./fuzz.sh all $(FUZZ_TIME)

fuzz-parse:
	./fuzz.sh parse $(FUZZ_TIME)

fuzz-write:
	./fuzz.sh write $(FUZZ_TIME)

fuzz-xmp:
	./fuzz.sh xmp $(FUZZ_TIME)

fuzz-mpf:
	./fuzz.sh mpf $(FUZZ_TIME)

fuzz-conform:
	./fuzz.sh conform $(FUZZ_TIME)

# Replay the saved crash artifacts against the current code.
fuzz-regress:
	@set -e; for t in fuzz_parse fuzz_write fuzz_xmp fuzz_mpf fuzz_conform; do \
		for f in fuzz/artifacts/$$t/*; do \
			if [ -f "$$f" ]; then cargo +nightly fuzz run $$t "$$f"; fi; \
		done; \
	done
