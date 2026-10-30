.PHONY: prepare check test build update clean

prepare:
	cargo +nightly fmt
	uv run scripts/blank-lines.py --fix
	cargo clippy --all-targets --fix --allow-dirty --allow-staged

check:
	cargo +nightly fmt --check
	uv run scripts/blank-lines.py
	cargo clippy --all-targets -- -D warnings
	cargo clippy --no-default-features --all-targets -- -D warnings
	uv run scripts/size.py --check

test:
	cargo test
	cargo test --no-default-features
	uv run --no-project python -m doctest scripts/size.py

build:
	cargo build --release

update:
	cargo upgrade --incompatible
	cargo update

clean:
	cargo clean

.PHONY: size

size: # code, docs and tests per part; ARGS="--files", "--map" (each file and what it does), "--check" (the budgets) or "--vs main"
	uv run scripts/size.py $(ARGS)
