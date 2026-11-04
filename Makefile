SHELL := /bin/bash

.DEFAULT_GOAL := help

ROOT_DIR     := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
SERVER_DIR   := $(ROOT_DIR)/server
FRONTEND_DIR := $(ROOT_DIR)/frontend

CARGO ?= cargo
PNPM  ?= pnpm

# Compose file that rebuild recreates the container from. Relative paths resolve from the repo root.
COMPOSE_FILE ?= docker-compose.yml

.PHONY: help setup dev check \
	server-run server-test server-lint library-file-gate server-fmt server-fmt-check contract-check contract-write \
	frontend-install frontend-browser-install frontend-dev frontend-build frontend-check \
	frontend-lint frontend-format-check frontend-test frontend-test-server frontend-test-client \
	image rebuild container-e2e

##@ Run

setup: frontend-install frontend-browser-install ## Install frontend dependencies and the test browser, and copy the frontend dev env
	@if [ ! -e "$(FRONTEND_DIR)/.env.development" ]; then \
		cp "$(FRONTEND_DIR)/env.development.example" "$(FRONTEND_DIR)/.env.development"; \
		echo "Created frontend/.env.development from the example"; \
	fi

dev: ## Run the server and the Vite dev server together; Ctrl-C stops both
	@trap 'trap - INT TERM; kill 0' INT TERM; \
	$(MAKE) --no-print-directory server-run & \
	$(MAKE) --no-print-directory frontend-dev & \
	wait

server-run: ## Run the server against ./dev-data (PORT defaults to 8688)
	mkdir -p "$(ROOT_DIR)/dev-data"
	cd "$(SERVER_DIR)" && ROOT_APP_DIR="$(ROOT_DIR)/dev-data" $(CARGO) run --bin droppedneedle

frontend-dev: ## Start the Vite dev server
	cd "$(FRONTEND_DIR)" && $(PNPM) run dev

frontend-install: ## Install frontend dependencies
	cd "$(FRONTEND_DIR)" && $(PNPM) install

frontend-browser-install: ## Install Playwright Chromium for browser tests
	cd "$(FRONTEND_DIR)" && $(PNPM) exec playwright install chromium

frontend-build: ## Build the frontend
	cd "$(FRONTEND_DIR)" && $(PNPM) run build

##@ Test and check

check: server-fmt-check library-file-gate server-lint server-test contract-check frontend-check frontend-lint frontend-format-check frontend-test ## Run the local CI gates (not audit, deny or container E2E)

server-test: ## Run the full Rust test suite
	cd "$(SERVER_DIR)" && $(CARGO) test --workspace

server-lint: ## Run clippy with warnings as errors
	cd "$(SERVER_DIR)" && $(CARGO) clippy --workspace --all-targets -- -D warnings

server-fmt-check: ## Check Rust formatting
	cd "$(SERVER_DIR)" && $(CARGO) fmt --check

library-file-gate: ## Refuse file changes in the library outside the publisher
	"$(SERVER_DIR)/scripts/library-file-gate.sh"

frontend-check: ## Type-check the frontend (svelte-check)
	cd "$(FRONTEND_DIR)" && $(PNPM) run check

frontend-lint: ## Lint the frontend
	cd "$(FRONTEND_DIR)" && $(PNPM) run lint

frontend-format-check: ## Check frontend formatting
	cd "$(FRONTEND_DIR)" && $(PNPM) run format:check

frontend-test: frontend-test-server frontend-test-client ## Run both vitest projects

frontend-test-server: ## Run the node vitest project (no browser)
	cd "$(FRONTEND_DIR)" && $(PNPM) exec vitest run --project server

frontend-test-client: ## Run the browser vitest project (needs Playwright Chromium)
	cd "$(FRONTEND_DIR)" && $(PNPM) exec vitest run --project client

container-e2e: ## Build a test image, run the container checks and the browser journeys
	"$(SERVER_DIR)/scripts/e2e-container.sh"

##@ Contract

contract-check: ## Fail if the OpenAPI snapshot or generated TypeScript drifted
	"$(SERVER_DIR)/openapi/check.sh" --from-tests

contract-write: ## Regenerate the OpenAPI snapshot and TypeScript types
	"$(SERVER_DIR)/openapi/check.sh" --from-tests --write

##@ Image

image: ## Build the container image as droppedneedle:local
	docker build -t droppedneedle:local "$(ROOT_DIR)"

rebuild: image ## Build droppedneedle:local and recreate the container from COMPOSE_FILE
	cd "$(ROOT_DIR)" && docker compose -f "$(COMPOSE_FILE)" up -d

##@ Other

# A line starting with "##@" opens a group in `make help`; a target's "## " text is its line.
help: ## Show available targets
	@awk 'BEGIN {FS = ":.*## "} /^##@ / {printf "\n%s\n", substr($$0, 5); next} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-24s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

server-fmt: ## Format the Rust code
	cd "$(SERVER_DIR)" && $(CARGO) fmt
