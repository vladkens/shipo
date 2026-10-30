.PHONY: prepare check test build update clean

prepare:
	cargo +nightly fmt
	cargo +nightly fmt --manifest-path setup/Cargo.toml
	uv run scripts/blank-lines.py --fix
	cargo clippy --all-targets --fix --allow-dirty --allow-staged
	cargo clippy --manifest-path setup/Cargo.toml --all-targets --fix --allow-dirty --allow-staged

check:
	cargo +nightly fmt --check
	cargo +nightly fmt --check --manifest-path setup/Cargo.toml
	uv run scripts/blank-lines.py
	cargo clippy --all-targets -- -D warnings
	cargo clippy --manifest-path setup/Cargo.toml --all-targets -- -D warnings
	cargo clippy --no-default-features --all-targets -- -D warnings
	uv run scripts/size.py --check

test:
	cargo test
	cargo test --manifest-path setup/Cargo.toml
	cargo test --no-default-features
	uv run --no-project python -m doctest scripts/size.py

build:
	cargo build --release

update:
	cargo upgrade --incompatible
	cargo upgrade --incompatible --manifest-path setup/Cargo.toml
	cargo update
	cargo update --manifest-path setup/Cargo.toml

clean:
	cargo clean
	cargo clean --manifest-path setup/Cargo.toml

.PHONY: size e2e setup

size: # code, docs and tests per part; ARGS="--files", "--map" (each file and what it does), "--check" (the budgets) or "--vs main"
	uv run scripts/size.py $(ARGS)

e2e: # the end-to-end update on this host: the example at 1.0.0 and 2.0.0 packed, 1.0.0 installed and updated
	uv run scripts/e2e-update.py all target/e2e

setup: # the Windows setup stub per arch, setup/target/<triple>/release/shipo-setup.exe: on Windows, elsewhere by cargo-xwin
	RUSTFLAGS="-C target-feature=+crt-static" cargo $(if $(filter Windows_NT,$(OS)),build,xwin build) --manifest-path setup/Cargo.toml --release --target x86_64-pc-windows-msvc --target aarch64-pc-windows-msvc
