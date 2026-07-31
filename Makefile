override MAX_WASM_BYTES := 120000
PRODUCTION_WASMS := backstop.wasm pool.wasm pool_factory.wasm

.PHONY: default test build wasm-sizes wasm-size-report fmt clean generate-js

default: build

test: build
	cargo test --workspace --tests --locked
	cargo check --manifest-path test-suites/fuzz/Cargo.toml --all-targets --locked

build:
	mkdir -p target/wasm32v1-none/optimized
	stellar contract build --package pool-factory --locked \
		--out-dir target/wasm32v1-none/optimized
	stellar contract build --package backstop --locked \
		--out-dir target/wasm32v1-none/optimized
	stellar contract build --package pool --locked \
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
		--wasm ./target/wasm32v1-none/optimized/pool_factory.wasm --output-dir ./js/js-pool-factory/ \
		--rpc-url http://localhost:8000 --network-passphrase "Standalone Network ; February 2017" --network Standalone
	stellar contract bindings typescript --overwrite \
		--contract-id CBWH54OKUK6U2J2A4J2REJEYB625NEFCHISWXLOPR2D2D6FTN63TJTWN \
		--wasm ./target/wasm32v1-none/optimized/pool.wasm --output-dir ./js/js-pool/ \
		--rpc-url http://localhost:8000 --network-passphrase "Standalone Network ; February 2017" --network Standalone
