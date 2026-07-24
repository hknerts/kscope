.PHONY: help build run test lint fmt check audit coverage docker clean install

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

build: ## Build an optimised binary
	cargo build --release --locked

run: ## Run against the current kube context
	cargo run

test: ## Run the test suite
	cargo test --all-features --locked

lint: ## Clippy with warnings denied
	cargo clippy --all-targets --all-features --locked -- -D warnings

fmt: ## Format the workspace
	cargo fmt --all

check: fmt lint test ## Everything CI runs

audit: ## Licence and advisory check
	cargo deny check

docker: ## Build the container image
	docker build -t kscope:dev .

install: ## Install into ~/.cargo/bin
	cargo install --path . --locked

clean:
	cargo clean
