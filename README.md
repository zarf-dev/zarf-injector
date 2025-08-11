# zarf-injector

> If using VSCode w/ the official Rust extension, make sure to open a new window in the `src/injector` directory to make `rust-analyzer` happy.
>
> ```bash
> code src/injector
> ```

A tiny (<1MiB) binary statically-linked with musl in order to fit as a configmap.

See how it gets used during the [`zarf-init`](https://docs.zarf.dev/commands/zarf_init/) process in the ['init' package reference documentation](https://docs.zarf.dev/ref/init-package/).

## What does it do?

```sh
zarf-injector <SHA256>
```

The `zarf-injector` binary serves 2 purposes during 'init'.

1. It re-assembles a multi-part tarball that was split into multiple ConfigMap entries (located at `./zarf-payload-*`) back into `payload.tar.gz`, then extracts it to the `/zarf-seed` directory. It also checks that the SHA256 hash of the re-assembled tarball matches the first (and only) argument provided to the binary.
2. It runs a pull-only, insecure, HTTP OCI compliant registry server on port 5000 that serves the contents of the `/zarf-seed` directory (which is of the OCI layout format).

This enables a distro-agnostic way to inject real `registry:3` image into a running cluster, thereby enabling air-gapped deployments.

# pre-req

* Install Rust using https://rustup.rs/
* Install cross with `make install-cross`
* Install Docker or Podman and have it running

## Building on Debian-based Systems

Install build-essential
```bash
sudo apt-get update
sudo apt-get install build-essential
```
Then build
```bash
make injector
```

## Building on Apple Silicon

Whichever arch. of `musl` used, add to toolchain
```
rustup toolchain install --force-non-host stable-x86_64-unknown-linux-musl
```
Then build
```
make injector-linux list-sizes
```

This will build into `target/*--unknown-linux-musl/release`

## Checking Binary Size

Due to the ConfigMap size limit (1MiB for binary data), we need to make sure the binary is small enough to fit.

```bash
make check-sizes
```

```bash
AMD64 injector: 1011736b
ARM64 injector: 917512b
```

## Testing your injector

Build your injector by following the steps above then run the following the `test` directory: 

```bash
zarf package create
zarf init --confirm
```