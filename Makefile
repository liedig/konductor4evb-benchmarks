.PHONY: help build test check benchmark-publish teardown-publish-benchmark clean

# Left unset, AWS commands use the caller's current credentials: an explicit
# --profile, even "default", overrides AWS_PROFILE and credential environment variables.
PLATFORM_PROFILE_OPTION = $(if $(PLATFORM_PROFILE),--profile $(PLATFORM_PROFILE))
AWS_REGION ?= $(or $(AWS_DEFAULT_REGION),$(shell aws configure get region $(PLATFORM_PROFILE_OPTION) 2>/dev/null),us-east-1)

CORE_STACK ?= konductor-core
BENCH_STACK ?= konductor-publish-benchmark
REPORT_FILE ?= /tmp/publish-benchmark-report.json
# Invocation payload overrides (profile knobs); '{}' reproduces issue #6.
PAYLOAD ?= {}
# Which of the core stack's environments the benchmark domain's bus lives in.
BENCH_ENV ?= dev

BOLD := $(shell tput bold 2>/dev/null)
CYAN := $(shell tput setaf 6 2>/dev/null)
GREEN := $(shell tput setaf 2 2>/dev/null)
YELLOW := $(shell tput setaf 3 2>/dev/null)
RESET := $(shell tput sgr0 2>/dev/null)

.DEFAULT_GOAL := help

help: ## Show this help message
	@printf "\n  $(BOLD)$(CYAN)Konductor publish benchmark$(RESET) — Makefile targets\n\n"
	@awk 'BEGIN {FS = ":.*?## "} /^[a-z-]+:.*## / {printf "    $(GREEN)%-28s$(RESET) %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@printf "\n  Required for benchmark-publish: BENCH_DOMAIN (a pre-existing, ACTIVE domain).\n\n"

build: ## Compile the driver (same build sam build runs)
	cargo lambda build --release --arm64

test: ## Run Rust tests and template checks
	cargo test
	uv run --with pytest --with pyyaml pytest tests

check: ## fmt + clippy gates
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

benchmark-publish: ## Deploy the harness, run the benchmark, write the report to $(REPORT_FILE)
ifndef BENCH_DOMAIN
	$(error BENCH_DOMAIN is required — a pre-existing, ACTIVE domain; register and approve it first)
endif
	@set -euo pipefail; \
	resolve() { aws cloudformation describe-stacks --stack-name $(CORE_STACK) \
		--query "$$1" --output text --region $(AWS_REGION) $(PLATFORM_PROFILE_OPTION); }; \
	API_ENDPOINT=$$(resolve "Stacks[0].Outputs[?OutputKey=='IamApiEndpoint'].OutputValue"); \
	TABLE=$$(resolve "Stacks[0].Outputs[?OutputKey=='AuthorizationBindingsTableName'].OutputValue"); \
	ORG=$$(resolve "Stacks[0].Parameters[?ParameterKey=='Organization'].ParameterValue"); \
	ENV=$(BENCH_ENV); \
	for pair in "IamApiEndpoint:$$API_ENDPOINT" "AuthorizationBindingsTableName:$$TABLE" "Organization:$$ORG" "Environment:$$ENV"; do \
		case "$${pair##*:}" in ""|None) echo "$(YELLOW)$${pair%%:*} missing on stack $(CORE_STACK) — deploy it first$(RESET)"; exit 1;; esac; \
	done; \
	API_ID=$$(printf '%s' "$$API_ENDPOINT" | sed -E 's|https://([^.]+)\..*|\1|'); \
	API_STAGE=$$(printf '%s' "$$API_ENDPOINT" | sed -E 's|.*/([^/]+)$$|\1|'); \
	echo "$(CYAN)Building$(RESET)"; \
	cargo lambda build --release --arm64; \
	sam build; \
	echo "$(CYAN)Deploying $(BENCH_STACK) (domain: $(BENCH_DOMAIN), org: $$ORG, env: $$ENV)$(RESET)"; \
	sam deploy --region $(AWS_REGION) $(PLATFORM_PROFILE_OPTION) \
		--parameter-overrides \
			BenchDomain=$(BENCH_DOMAIN) \
			Organization=$$ORG \
			Environment=$$ENV \
			CoreApiEndpoint=$$API_ENDPOINT \
			CoreApiId=$$API_ID \
			CoreApiStage=$$API_STAGE \
			BindingsTableName=$$TABLE; \
	echo "$(CYAN)Running the benchmark (this takes ~12 minutes with the default profile)$(RESET)"; \
	aws lambda invoke --function-name $(BENCH_STACK) \
		--payload '$(PAYLOAD)' --cli-binary-format raw-in-base64-out \
		--output json \
		--cli-read-timeout 920 \
		--region $(AWS_REGION) $(PLATFORM_PROFILE_OPTION) \
		"$(REPORT_FILE)" > /tmp/publish-benchmark-invoke.json; \
	if grep -q '"FunctionError"' /tmp/publish-benchmark-invoke.json; then \
		echo "$(YELLOW)Run failed:$(RESET)"; cat "$(REPORT_FILE)"; echo; \
		if grep -qE 'does not exist|not ACTIVE' "$(REPORT_FILE)"; then \
			echo "$(YELLOW)The domain is still registering or provisioning — wait for approval/ACTIVE, then re-run: make benchmark-publish$(RESET)"; \
		fi; \
		exit 1; \
	fi; \
	echo "$(GREEN)Report written to $(REPORT_FILE)$(RESET)"; \
	jq '{publish: .publish.latencies, floor: .floor.latencies, reconciliation: .reconciliation, comparisons: .comparisons}' "$(REPORT_FILE)"

teardown-publish-benchmark: ## Delete the benchmark stack (function, role, log group)
	@set -euo pipefail; \
	sam delete --stack-name $(BENCH_STACK) --no-prompts --region $(AWS_REGION) $(PLATFORM_PROFILE_OPTION)

clean: ## Remove build artifacts
	cargo clean
	rm -rf .aws-sam
