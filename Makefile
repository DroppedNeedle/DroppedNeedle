SHELL := /bin/bash

.DEFAULT_GOAL := help

ROOT_DIR     := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
SERVER_DIR   := $(ROOT_DIR)/server
FRONTEND_DIR := $(ROOT_DIR)/frontend

CARGO ?= cargo
PNPM  ?= pnpm

.PHONY: help check \
	server-run server-test server-lint server-fmt server-fmt-check contract-check contract-write \
	frontend-install frontend-browser-install frontend-dev frontend-build frontend-check \
	frontend-lint frontend-format-check frontend-test frontend-test-server frontend-test-client \
	image

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "} {printf "  %-24s %s\n", $$1, $$2}'

check: server-fmt-check server-lint server-test contract-check frontend-check frontend-lint frontend-format-check frontend-test ## Run the local CI gates (not audit, deny or container E2E)

server-run: ## Run the server against ./dev-data (PORT defaults to 8688)
	mkdir -p "$(ROOT_DIR)/dev-data"
	cd "$(SERVER_DIR)" && ROOT_APP_DIR="$(ROOT_DIR)/dev-data" $(CARGO) run --bin droppedneedle

server-test: ## Run the full Rust test suite
	cd "$(SERVER_DIR)" && $(CARGO) test --workspace

server-lint: ## Run clippy with warnings as errors
	cd "$(SERVER_DIR)" && $(CARGO) clippy --workspace --all-targets -- -D warnings

server-fmt: ## Format the Rust code
	cd "$(SERVER_DIR)" && $(CARGO) fmt

server-fmt-check: ## Check Rust formatting
	cd "$(SERVER_DIR)" && $(CARGO) fmt --check

contract-check: ## Fail if the OpenAPI snapshot or generated TypeScript drifted
	"$(SERVER_DIR)/openapi/check.sh"

contract-write: ## Regenerate the OpenAPI snapshot and TypeScript types
	"$(SERVER_DIR)/openapi/check.sh" --write

frontend-install: ## Install frontend dependencies
	cd "$(FRONTEND_DIR)" && $(PNPM) install

frontend-browser-install: ## Install Playwright Chromium for browser tests
	cd "$(FRONTEND_DIR)" && $(PNPM) exec playwright install chromium

frontend-dev: ## Start the Vite dev server
	cd "$(FRONTEND_DIR)" && $(PNPM) run dev

frontend-build: ## Build the frontend
	cd "$(FRONTEND_DIR)" && $(PNPM) run build

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

image: ## Build the container image as droppedneedle:local
	docker build -t droppedneedle:local "$(ROOT_DIR)"
