# Remote test hosts (override on the command line, e.g. `make test-linux LINUX=me@host`).
MACOS ?= admin@192.168.64.32
LINUX ?= admin@192.168.64.33
REMOTE_DIR ?= tdu
CARGO_REMOTE ?= ~/.cargo/bin/cargo

PREFIX ?= $(HOME)/.local

.PHONY: all build release test check fmt lint clean install run snapshot \
        test-remote test-macos test-linux

all: build

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

# Formatting check + clippy + tests: what should pass before committing.
check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

fmt:
	cargo fmt

lint:
	cargo clippy --all-targets -- -D warnings

run:
	cargo run --release -- $(ARGS)

snapshot:
	cargo run --example snapshot

install: release
	install -d $(PREFIX)/bin
	install -m 755 target/release/tdu $(PREFIX)/bin/tdu

clean:
	cargo clean

# Build and test the committed HEAD on the macOS and Linux VMs.
# Uncommitted changes are not included.
test-remote: test-macos test-linux

test-macos:
	$(call remote_test,$(MACOS))

test-linux:
	$(call remote_test,$(LINUX))

define remote_test
	@echo "===== $(1)"
	git archive --format=tar HEAD | ssh $(1) '\
		rm -rf ~/$(REMOTE_DIR) && mkdir ~/$(REMOTE_DIR) && tar -x -C ~/$(REMOTE_DIR) && \
		cd ~/$(REMOTE_DIR) && $(CARGO_REMOTE) build --release && $(CARGO_REMOTE) test'
endef
