# oxidil build orchestration.
#
# Cargo.toml is the SINGLE SOURCE OF TRUTH for the version. Every other
# version reference is generated from it into a gitignored file:
#   - npm/package.json       (from npm/package.tpl.json)
#   - npm/src/version.ts     (consumed by the CLI)
#
# Common targets:
#   make            build the native binary AND the npm package
#   make rust       build only the native release binary
#   make npm        build only the npm package (wasm + TS)
#   make generate   only (re)generate the versioned files
#   make publish    build + npm publish
#   make clean      remove build artifacts and generated files

# Version, extracted from the [package] section of Cargo.toml.
VERSION := $(shell grep -m1 '^version' Cargo.toml | sed -E 's/^version[[:space:]]*=[[:space:]]*"(.*)".*/\1/')

GENERATED := npm/package.json npm/src/version.ts

.PHONY: all rust npm generate clean publish publish-dry print-version

all: rust npm

# --- generated, gitignored version files -----------------------------------

generate: $(GENERATED)

npm/package.json: npm/package.tpl.json Cargo.toml
	sed 's/{{VERSION}}/$(VERSION)/g' npm/package.tpl.json > $@

npm/src/version.ts: Cargo.toml
	printf '// Generated from the version in Cargo.toml by the Makefile.\n// Do not edit; this file is gitignored.\nexport const VERSION = "%s";\n' '$(VERSION)' > $@

# --- builds -----------------------------------------------------------------

# Native release binary at ./target/release/oxidil.
rust:
	cargo build --release

# npm package: wasm-pack build (reads Cargo.toml) + tsc, ready to publish.
npm: generate
	cd npm && npm install && npm run build

# --- publish ----------------------------------------------------------------

publish-dry: npm
	cd npm && npm pack --dry-run

publish: npm
	cd npm && npm publish

# --- misc -------------------------------------------------------------------

print-version:
	@echo $(VERSION)

clean:
	rm -rf target npm/dist npm/wasm npm/node_modules
	rm -f $(GENERATED)
