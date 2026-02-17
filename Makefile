# Reaper — Development Makefile
#
# Primary entry point for all build, test, and CI workflows.
# Run `make help` to see available targets.

LINUX_TARGET := x86_64-unknown-linux-gnu
DOCKER_IMAGE := reaper-dev
COVERAGE_VOL := reaper-cargo-cache

.PHONY: help build test fmt clippy check-linux coverage ci clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

build: ## Build debug binaries (macOS native)
	cargo build

build-release: ## Build release binaries (macOS native)
	cargo build --release

# ---------------------------------------------------------------------------
# Quality checks
# ---------------------------------------------------------------------------

fmt: ## Check formatting (fails if unformatted)
	cargo fmt -- --check

clippy: ## Run clippy for macOS target
	cargo clippy -- -D warnings

check-linux: ## Cross-check clippy for Linux target (catches cfg(linux) issues)
	cargo clippy --target $(LINUX_TARGET) --all-targets -- -D warnings

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

test: ## Run all unit and integration tests (macOS native)
	cargo test --verbose

test-unit: ## Run only unit tests for reaper-runtime
	cargo test --bin reaper-runtime --verbose

# ---------------------------------------------------------------------------
# Coverage (Docker — mirrors CI exactly)
# ---------------------------------------------------------------------------

coverage: ## Run tarpaulin in Docker (same as CI, with caching)
	@echo "==> Building Docker image $(DOCKER_IMAGE)..."
	docker build -t $(DOCKER_IMAGE) -f scripts/Dockerfile.coverage .
	@echo "==> Ensuring cargo cache volume $(COVERAGE_VOL)..."
	docker volume create $(COVERAGE_VOL) 2>/dev/null || true
	@echo "==> Running tarpaulin (Linux)..."
	docker run --rm \
		--cap-add=SYS_PTRACE \
		--security-opt seccomp=unconfined \
		-v "$$(pwd)":/usr/src/reaper \
		-v $(COVERAGE_VOL):/usr/local/cargo/registry \
		-w /usr/src/reaper \
		-e CARGO_TERM_COLOR=always \
		-e RUST_BACKTRACE=1 \
		$(DOCKER_IMAGE) \
		cargo tarpaulin
	@echo "==> Coverage report: cobertura.xml"

# ---------------------------------------------------------------------------
# CI — run everything GitHub Actions runs (except kind integration)
# ---------------------------------------------------------------------------

ci: fmt clippy check-linux test coverage ## Full CI-equivalent check (format + clippy + linux check + tests + coverage)
	@echo ""
	@echo "All CI checks passed."

# ---------------------------------------------------------------------------
# Integration tests (requires Docker + kind)
# ---------------------------------------------------------------------------

integration: ## Run full K8s integration tests (kind cluster)
	./scripts/run-integration-tests.sh

integration-quick: ## Run K8s integration tests, skip cargo tests
	./scripts/run-integration-tests.sh --skip-cargo

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

clean: ## Remove build artifacts and coverage files
	cargo clean
	rm -f cobertura.xml

clean-all: clean ## Remove everything including Docker cache volumes
	docker volume rm $(COVERAGE_VOL) 2>/dev/null || true
