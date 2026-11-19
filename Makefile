override MAX_WASM_BYTES := 120000
PRODUCTION_WASMS := backstop.wasm emitter.wasm pool.wasm pool_factory.wasm
COMET_V1_1_WASM_SHA256 := d735c3395f59510172cf5cf838823a1389b97cfec6bf24e580e9bd77d2b3e687
SOURCE_REPO_META := source_repo=github:BLNT-Protocol/blnt-contracts-v3
MAINNET_FIXTURE ?= $(if $(XDG_CACHE_HOME),$(XDG_CACHE_HOME),$(HOME)/.cache)/blnt-contracts-v3/oracle-outage-v1.json

.PHONY: default test test-mainnet prepare-mainnet-fixture build build-pool-dependency verify-comet-v1.1 verify-source-metadata wasm-sizes wasm-size-report fmt clean generate-js

default: build

build-pool-dependency:
	cargo build --target wasm32v1-none --release --locked --package backstop

test: build
	cargo test --workspace --tests --locked
	cargo check --manifest-path test-suites/fuzz/Cargo.toml --all-targets --locked

# Capture is explicit and read-only. Existing snapshots are never refreshed.
prepare-mainnet-fixture: build
	@mkdir -p "$$(dirname "$(MAINNET_FIXTURE)")"
	@set -eu; \
		if test -f "$(MAINNET_FIXTURE)"; then \
			printf 'Using existing snapshot: %s\n' '$(MAINNET_FIXTURE)'; \
		else \
			BLNT_CAPTURE_OUTPUT="$(MAINNET_FIXTURE)" cargo test --manifest-path test-suites/mainnet/Cargo.toml --locked capture_fixture -- --ignored --nocapture; \
		fi

test-mainnet: build
	BLNT_MAINNET_FIXTURE="$(MAINNET_FIXTURE)" cargo test --manifest-path test-suites/mainnet/Cargo.toml --locked -- --nocapture

verify-comet-v1.1:
	test "$$(shasum -a 256 comet.wasm | cut -d ' ' -f 1)" = "$(COMET_V1_1_WASM_SHA256)"

build: verify-comet-v1.1
	mkdir -p target/wasm32v1-none/optimized
	stellar contract build --package pool-factory --locked \
		--meta $(SOURCE_REPO_META) \
		--out-dir target/wasm32v1-none/optimized
	stellar contract build --package backstop --locked \
		--meta $(SOURCE_REPO_META) \
		--out-dir target/wasm32v1-none/optimized
	stellar contract build --package emitter --locked \
		--meta $(SOURCE_REPO_META) \
		--out-dir target/wasm32v1-none/optimized
	stellar contract build --package pool --locked \
		--meta $(SOURCE_REPO_META) \
		--out-dir target/wasm32v1-none/optimized
	@for wasm in $(PRODUCTION_WASMS); do \
		size="$$(wc -c < "target/wasm32v1-none/optimized/$$wasm")"; \
		test "$$size" -le "$(MAX_WASM_BYTES)" || \
			{ printf 'error: optimized %s exceeds %s-byte deployment guard\n' \
				"$$wasm" "$(MAX_WASM_BYTES)" >&2; exit 1; }; \
	done
	cd target/wasm32v1-none/optimized/ && \
		for i in $(PRODUCTION_WASMS) ; do \
			ls -l "$$i"; \
		done
	$(MAKE) --no-print-directory verify-source-metadata

verify-source-metadata:
	@for wasm in $(PRODUCTION_WASMS); do \
		stellar contract info meta \
			--wasm "target/wasm32v1-none/optimized/$$wasm" 2>/dev/null | \
			grep -Fq "source_repo: github:BLNT-Protocol/blnt-contracts-v3" || \
			{ printf 'error: missing source_repo metadata in %s\n' "$$wasm" >&2; exit 1; }; \
	done

wasm-sizes: build
	@$(MAKE) --no-print-directory wasm-size-report

wasm-size-report:
	@MAX_WASM_BYTES="$(MAX_WASM_BYTES)" \
		./scripts/wasm-size-report.sh

fmt:
	cargo fmt --all

clean:
	cargo clean

generate-js:
	stellar contract bindings typescript --overwrite \
		--contract-id CBWH54OKUK6U2J2A4J2REJEYB625NEFCHISWXLOPR2D2D6FTN63TJTWN \
		--wasm ./target/wasm32v1-none/optimized/backstop.wasm --output-dir ./js/js-backstop/ \
		--rpc-url http://localhost:8000 --network-passphrase "Standalone Network ; February 2017" --network Standalone
	stellar contract bindings typescript --overwrite \
		--contract-id CBWH54OKUK6U2J2A4J2REJEYB625NEFCHISWXLOPR2D2D6FTN63TJTWN \
		--wasm ./target/wasm32v1-none/optimized/emitter.wasm --output-dir ./js/js-emitter/ \
		--rpc-url http://localhost:8000 --network-passphrase "Standalone Network ; February 2017" --network Standalone
	stellar contract bindings typescript --overwrite \
		--contract-id CBWH54OKUK6U2J2A4J2REJEYB625NEFCHISWXLOPR2D2D6FTN63TJTWN \
		--wasm ./target/wasm32v1-none/optimized/pool_factory.wasm --output-dir ./js/js-pool-factory/ \
		--rpc-url http://localhost:8000 --network-passphrase "Standalone Network ; February 2017" --network Standalone
	stellar contract bindings typescript --overwrite \
		--contract-id CBWH54OKUK6U2J2A4J2REJEYB625NEFCHISWXLOPR2D2D6FTN63TJTWN \
		--wasm ./target/wasm32v1-none/optimized/pool.wasm --output-dir ./js/js-pool/ \
		--rpc-url http://localhost:8000 --network-passphrase "Standalone Network ; February 2017" --network Standalone
