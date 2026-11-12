CARGO ?= cargo

.PHONY: build test lint fmt check run clean

build:
	$(CARGO) build --release

test:
	$(CARGO) test

lint:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt --all

check: fmt lint test

run:
	$(CARGO) run -- check

clean:
	$(CARGO) clean
