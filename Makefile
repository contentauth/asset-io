.PHONY: ci check-format fmt check-docs clippy test doc fuzz-build fuzz fuzz-parse fuzz-write fuzz-xmp fuzz-mpf fuzz-conform fuzz-regress

# Formatting follows c2pa-rs: rustfmt.toml uses unstable options, so it needs
# nightly rustfmt (rustup toolchain add nightly).
FMT_MANIFESTS := Cargo.toml asset-io-c/Cargo.toml asset-io-wasm/Cargo.toml fuzz/Cargo.toml

# Run the full CI preflight locally (mirrors .github/workflows/ci.yml).
# Run this before pushing a PR.
ci: check-format check-docs clippy test

check-format:
	@set -e; for m in $(FMT_MANIFESTS); do \
		cargo +nightly fmt --manifest-path $$m -- --check; \
	done

# Auto format code according to standards
fmt:
	@set -e; for m in $(FMT_MANIFESTS); do \
		cargo +nightly fmt --manifest-path $$m; \
	done

check-docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

clippy:
	cargo clippy --all-features --all-targets -- -D warnings
	cargo clippy --no-default-features --all-targets -- -D warnings

test:
	cargo test --no-default-features
	@set -e; for handler in jpeg png bmff riff svg pdf tiff c2pa; do \
		cargo test --no-default-features --features $$handler; \
	done
	cargo test --all-features

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
