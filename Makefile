.PHONY: build bundle install uninstall test

build:
	cargo build --release --workspace

bundle:
	scripts/bundle.sh

install:
	scripts/install.sh

uninstall:
	scripts/uninstall.sh

test:
	cargo test --workspace
