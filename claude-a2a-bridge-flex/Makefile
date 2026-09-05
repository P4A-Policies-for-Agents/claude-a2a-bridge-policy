export PDK_COMPATIBILITY_VERSION = 1.4.0
TARGET                	:= wasm32-wasip1
TARGET_DIR            	:= target/$(TARGET)/release
BUILD_POLICY_DIR        := target
CARGO_ANYPOINT        	:= cargo-anypoint
CRATE_NAME		        = $(shell cargo anypoint get-name)
DEFINITION_GAV          = $(shell cargo anypoint get-policy-definition-gav)
DEFINITION_NAMESPACE    := default
MIN_FLEX_VERSION        := 1.9.3


ifeq ($(OS), Windows_NT)
    SHELL = powershell.exe
    .SHELLFLAGS = -NoProfile -ExecutionPolicy Bypass -Command
	POLICY_REF_NAME = $(shell $$env:PDK_COMPATIBILITY_VERSION="$(PDK_COMPATIBILITY_VERSION)"; cargo anypoint get-policy-implementation-name)
else
	POLICY_REF_NAME = $(shell export PDK_COMPATIBILITY_VERSION=$(PDK_COMPATIBILITY_VERSION); cargo anypoint get-policy-implementation-name)
endif

.PHONY: setup
setup: install-cargo-anypoint ## Setup Cargo Anypoint to build

.PHONY: build-asset-files
build-asset-files:  ## Retrieves the definition from exchange
	@anypoint-cli-v4 pdk fetch-policy --gav=$(DEFINITION_GAV)
	@# Note: src/generated/config.rs is hand-maintained because the GCL schema uses
	@# a nested object property and DataWeave selectors which the current
	@# `cargo anypoint config-gen` cannot prettify. The hand-rolled struct preserves
	@# the schema and is checked in. Do NOT regenerate over the nested object.
	@if [ ! -f src/generated/config.rs ]; then \
		echo "Error: src/generated/config.rs is missing. Restore from version control."; \
		exit 1; \
	fi

.PHONY: build
build: ## Builds the policy implementation
	@cargo build --target $(TARGET) --release
	@cargo anypoint build-policy --target=$(BUILD_POLICY_DIR) --wasm=$(TARGET_DIR)/$(CRATE_NAME).wasm --min-runtime-version=$(MIN_FLEX_VERSION)
	@anypoint-cli-v4 pdk install-policy --target $(TARGET_DIR) --path $(BUILD_POLICY_DIR)/$(CRATE_NAME)/implementation/
	@echo $(POLICY_REF_NAME) > target/policy-ref-name.txt

.PHONY: run
run: build  ## Run the policy in local flex
	@anypoint-cli-v4 pdk patch-gcl -f playground/config/api.yaml -p "spec.policies[0].policyRef.name" -v "$(POLICY_REF_NAME)"
	@anypoint-cli-v4 pdk patch-gcl -f playground/config/api.yaml -p "spec.policies[0].policyRef.namespace" -v "$(DEFINITION_NAMESPACE)"
	@anypoint-cli-v4 pdk install-policy --target ./playground/config/custom-policies --path $(BUILD_POLICY_DIR)/$(CRATE_NAME)/implementation/
	-docker compose -f ./playground/docker-compose.yaml down
	docker compose -f ./playground/docker-compose.yaml up

.PHONY: test
test: build ## Run all tests (unit + integration)
	@cargo test -- --nocapture

.PHONY: test-unit
test-unit: ## Run unit tests only (no Docker required)
	@cargo test --lib -- --nocapture

.PHONY: test-one
test-one: build ## Run a single test by name (usage: make test-one TEST=test_name)
	@cargo test -- --nocapture $(TEST)

.PHONY: publish
publish: build-asset-files build ## Publish a development version of the policy
	@anypoint-cli-v4 pdk policy-wasm publish --wasm=$(TARGET_DIR)/$(CRATE_NAME).wasm  --target=$(BUILD_POLICY_DIR)/$(CRATE_NAME) --dev

.PHONY: release
release: build-asset-files  build ## Publish a release version
	@anypoint-cli-v4 pdk policy-wasm publish --wasm=$(TARGET_DIR)/$(CRATE_NAME).wasm  --target=$(BUILD_POLICY_DIR)/$(CRATE_NAME)

.PHONY: install-cargo-anypoint
install-cargo-anypoint:
	@cargo install cargo-anypoint@1.10.0

.PHONY: show-policy-ref-name
show-policy-ref-name:
	@echo $(POLICY_REF_NAME)

ifneq ($(OS), Windows_NT)
all: help

.PHONY: help
help: ## Shows this help
	@echo 'Usage: make <target>'
	@echo ''
	@echo 'Available targets are:'
	@echo ''
	@grep -Eh '^\w[^:]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-6s\033[0m %s\n", $$1, $$2}' \
		| sort
endif
