BACKSTOP_MAX_WASM_BYTES := 120000

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
	@test "$$(wc -c < target/wasm32v1-none/optimized/backstop.wasm)" \
		-le "$(BACKSTOP_MAX_WASM_BYTES)" || \
		(printf 'error: optimized backstop exceeds %s-byte deployment guard\n' \
			"$(BACKSTOP_MAX_WASM_BYTES)" >&2; exit 1)
	stellar contract build --package pool --locked \
		--out-dir target/wasm32v1-none/optimized
	stellar contract build --package blend-v3-backstop-valuation --locked \
		--out-dir target/wasm32v1-none/optimized
	cd target/wasm32v1-none/optimized/ && \
		for i in *.wasm ; do \
			ls -l "$$i"; \
		done

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
