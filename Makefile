# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2021-Present The Zarf Authors

.PHONY: help
help: ## Display this help information
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
	  | sort | awk 'BEGIN {FS = ":.*?## "}; \
	  {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

clean: ## Clean the build directory
	rm -rf target

install-cross: ## installs cross
	cargo install cross --git https://github.com/cross-rs/cross

injector: injector-amd injector-arm ## Builds the injector for both platforms

injector-amd: ## builds the injector for amd64
	rustup target add x86_64-unknown-linux-musl
	cross build --target x86_64-unknown-linux-musl --release

injector-arm: ## builds the injector for arm64
	rustup target add aarch64-unknown-linux-musl
	cross build --target aarch64-unknown-linux-musl --release

check-size: ## Validate that both injector binaries are under 1 MiB
	@max_size=1024; \
	amd_size=$$(du -k target/x86_64-unknown-linux-musl/release/zarf-injector | cut -f1); \
	arm_size=$$(du -k target/aarch64-unknown-linux-musl/release/zarf-injector | cut -f1); \
	echo "AMD64 injector: $${amd_size}k"; \
	echo "ARM64 injector: $${arm_size}k"; \
	if [ $$amd_size -ge $$max_size ] || [ $$arm_size -ge $$max_size ]; then \
		echo "Error: One or both injectors exceed 1 MiB (1024k) limit"; \
		exit 1; \
	fi
