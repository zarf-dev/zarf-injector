# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2021-Present The Zarf Authors

ifeq ($(shell stat --help >/dev/null 2>&1; echo $$?), 0)
  stat_format = "-c%s"
else
  stat_format = "-f%z"
endif

.PHONY: help
help: ## Display this help information
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
	  | sort | awk 'BEGIN {FS = ":.*?## "}; \
	  {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

clean: ## Clean the build directory
	rm -rf target

install-rust: ## Install Rust via rustup
	curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

install-targets: ## Add required Rust targets
	rustup target add x86_64-unknown-linux-musl
	rustup target add aarch64-unknown-linux-musl

install-cross: ## installs cross
	cargo install cross --git https://github.com/cross-rs/cross

setup: install-rust install-targets install-cross ## Install all dependencies

injector: injector-amd injector-arm ## Builds the injector for both platforms

injector-amd: target/x86_64-unknown-linux-musl/release/zarf-injector ## builds the injector for amd64

injector-arm: target/aarch64-unknown-linux-musl/release/zarf-injector ## builds the injector for arm64

check-size: injector ## Validate that both injector binaries are under 1 MiB
	@max_size=1048576; \
	amd_size=$$(stat $(stat_format) target/x86_64-unknown-linux-musl/release/zarf-injector); \
	arm_size=$$(stat $(stat_format) target/aarch64-unknown-linux-musl/release/zarf-injector); \
	echo "Injector sizes: "; \
	echo "AMD64 injector: $${amd_size}b"; \
	echo "ARM64 injector: $${arm_size}b"; \
	if [ $$amd_size -ge $$max_size ] || [ $$arm_size -ge $$max_size ]; then \
		echo "Error: One or both injectors exceed 1 MiB ($$max_size byte) limit"; \
		exit 1; \
	fi


unit-test: ## Run cargo tests
	cargo test 	

target/x86_64-unknown-linux-musl/release/zarf-injector: src/main.rs Cargo.toml
	cross build --target x86_64-unknown-linux-musl --release

target/aarch64-unknown-linux-musl/release/zarf-injector: src/main.rs Cargo.toml
	cross build --target aarch64-unknown-linux-musl --release

