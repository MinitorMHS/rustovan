# Production-readiness plan

**Status snapshot: 2026-11-01.** The project supports CPU search and GPU-assisted PBKDF2 across platform-dependent backends. The CPU remains the correctness reference. GPU SLIP-0010, Ed25519, wallet construction, TON hashing, and matching are not implemented; a GPU backend is not a fully GPU-resident vanity search.

The committed main revision passed all nine hosted build/test jobs in [Rust CI run 41292225784](https://github.com/MinitorMHS/rustovan/actions/runs/41292225784), including macOS compile/test coverage and the expanded Windows/Linux target matrix. This is not a runtime test of every GPU or CPU architecture. Subsequent uncommitted source/workflow changes require their own CI run.

## Release gates

### 1. Cryptographic correctness and wallet interoperability

- Keep the CPU pipeline as the reference and retain the published BIP39 TREZOR seed vector.
- Add independently sourced address fixtures and provenance for V1R1 through V4R1, non-default W5 wallet IDs/workchains, and WalletTg.
- Continue cross-checking TON address derivation against an independent SDK; current independent coverage is V4R2 and W5.
- Verify restoration behavior across supported mnemonic/passphrase and network/address settings.

**Status:** Partially verified; legacy fixture provenance and broader independent wallet coverage remain open.

### 2. Search behavior and resource accounting

- Preserve bounded candidate accounting, multiple-match semantics, exact-address validation, cancellation, and CPU verification of GPU-found matches.
- Exercise runtime adapter failures, device loss, allocation pressure, batch recovery, daemon interruption, and multi-GPU scheduling on representative hardware.
- Keep measured Auto/Hybrid routing reports clearly described as estimates that can vary with load and temperature.

**Status:** Core tests and bounded host checks exist. Physical multi-GPU scaling, broader driver failure injection, and production-duration hybrid measurements remain open.

### 3. Secret handling and output

- Retain default secret withholding, explicit secret export, protected file creation, atomic no-overwrite behavior, and best-effort zeroization.
- Complete Windows ACL, reparse-point, partial-write, forced-interruption, and clean-machine package checks.
- Keep sealed output documented as at-rest encryption. It cannot protect an untrusted runner from memory inspection or executable modification.
- Recheck the supported age recipient formats and all optional sealed-output dependencies.

**Status:** Core controls are implemented; failure-path and target-machine acceptance remain open.

### 4. Backend and platform validation

- GPU-assisted search currently accelerates PBKDF2 only; do not describe it as fully GPU-resident.
- CI run 41292225784 passed the committed Windows/Linux tests and build matrix, including Windows ARM64, Linux ARMv7 hard-float, RISC-V64, glibc/musl Linux, and macOS compile/test coverage.
- Retain per-device PBKDF2 parity checks and validate drivers/runtime on physical hardware. Termux Vulkan is correct for the observed small dispatches but very slow; macOS Metal has compile coverage only; CUDA has no current build/runtime acceptance; physical multi-GPU behavior is unverified.
- Verify any uncommitted code changes in CI before release.

**Status:** Cross-target compile/test coverage is broad; hardware runtime coverage is incomplete.

### 5. Dependency, license, and release operations

- Resolve or formally assess the latest RustSec audit findings before calling the dependency scan clean. The latest hosted scan found crypto-bigint 0.5.5 (RUSTSEC-2026-0387) and an unmaintained basic-toml 0.1.10 warning (RUSTSEC-2026-0374); see [the audit run](https://github.com/MinitorMHS/rustovan/actions/runs/41292196456).
- Regenerate the dependency-license CSV from the current lockfile. The checked-in inventory is an October 11 snapshot of 360 packages; the latest audit scanned 450.
- Obtain maintainer license-policy approval and document required notices.
- Test published packages on clean machines and verify release checksums. The current v0.1.0 assets predate the expanded Windows ARM64, ARMv7, RISC-V64, and musl matrix.
- Define supported release policy, signing/provenance expectations, and update/uninstall guidance.

**Status:** Release blocker: security scan is failing and license metadata is stale; current release assets do not yet cover all configured targets.

## Production-ready exit criteria

A release may be described as production-ready only after the gates above have owners and current evidence, the dependency audit is resolved or explicitly reviewed, the license inventory is refreshed and approved, target-device behavior is validated, and package/secret-output acceptance is complete. See [STATUS.md](STATUS.md), [docs/validation.md](docs/validation.md), and [docs/dependency-license-inventory.md](docs/dependency-license-inventory.md).