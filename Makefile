.PHONY: build test lint

build:
	cargo build --all-targets

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings
	cargo fmt --check
