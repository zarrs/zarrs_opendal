TOOLCHAIN ?= nightly

all: build

build:
	cargo +$(TOOLCHAIN) build

test:
	cargo +$(TOOLCHAIN) test
	cargo +$(TOOLCHAIN) test --examples

doc: RUSTDOCFLAGS="-D warnings --cfg docsrs"
doc:
	cargo +$(TOOLCHAIN) doc -Z unstable-options -Z rustdoc-scrape-examples --no-deps

clippy:
	cargo +$(TOOLCHAIN) clippy -- -D warnings

check: build test clippy doc
	cargo +$(TOOLCHAIN) fmt --all -- --check
	cargo +$(TOOLCHAIN) check

coverage_install:
	cargo install cargo-llvm-cov --locked

coverage_report:
	cargo +$(TOOLCHAIN) llvm-cov --doctests --html

coverage_file:
	cargo +$(TOOLCHAIN) llvm-cov --doctests --lcov --output-path lcov.info

fmt:
	cargo +$(TOOLCHAIN) fmt

clean:
	cargo clean

.PHONY: all build test doc clippy check fmt clean
