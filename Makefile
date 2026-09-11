.PHONY: build bundle dmg install uninstall test

build:
	cargo build --release --workspace

bundle:
	scripts/bundle.sh

dmg: bundle
	scripts/make-dmg.sh

install:
	scripts/install.sh

uninstall:
	scripts/uninstall.sh

test:
	cargo test --workspace
