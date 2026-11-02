# Production-readiness plan

**Status snapshot: 2026-11-01.** The project supports CPU search and GPU-assisted PBKDF2 across platform-dependent backends. The CPU remains the correctness reference. GPU SLIP-0010, Ed25519, wallet construction, TON hashing, and matching are not implemented; a GPU backend is not a fully GPU-resident vanity search.

The committed main revision passed all nine hosted build/test jobs in [Rust CI run 41292225784](https://github.com/MinitorMHS/rustovan/actions/runs/41292225784), including macOS compile/test coverage and the expanded Windows/Linux target matrix. This is not a runtime test of every GPU or CPU architecture. Subsequent uncommitted source/workflow changes require their own CI run.

## Release gates

### 1. Cryptographic correctness and wallet interoperability

- Keep the CPU pipeline as the reference and retain the published BIP39 TREZOR seed vector.
- Add independently sourced address fixtures and provenance for V1R1 through V4R1, non-default W5 wallet IDs/workchains, and WalletTg.
- Continue cross-checking TON address derivation against an independent SDK; current independent coverage is V4R2 and W5.
- The maintainer reports successfully restoring a generated wallet in a wallet app. Record the app/version and wallet/network/passphrase combination before claiming broader interoperability; test the remaining combinations separately.

**Status:** At least one restoration is maintainer-verified; the exact app/version and configuration were not recorded. Legacy fixture provenance and broader independent wallet coverage remain open.

### 2. Search behavior and resource accounting

- Preserve bounded candidate accounting, multiple-match semantics, exact-address validation, cancellation, and CPU verification of GPU-found matches.
- Exercise runtime adapter failures, device loss, allocation pressure, batch recovery, and daemon interruption. Multi-GPU scheduling is implemented but remains unverified until it is run on representative physical multi-GPU hardware.
- Keep measured Auto/Hybrid routing reports clearly described as estimates that can vary with load and temperature.

**Status:** Core tests and bounded host checks exist. Physical multi-GPU scaling, broader driver failure injection, and production-duration hybrid measurements remain open.

### 3. Secret handling and output

- Retain default secret withholding, explicit secret export, protected file creation, atomic no-overwrite behavior, and best-effort zeroization.
- Windows daemon directories now receive an owner-only DACL through no-follow handles; a local regression test checks the actual ACLs and final-component reparse-point rejection. Partial-write failure cleanup is unit-tested. Abrupt termination can leave an owner-protected staging file because OS termination bypasses cleanup; deletion cannot guarantee flash erasure. Verify Unix daemon directory/file controls on a native Unix host and complete clean-machine package checks.
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

- The default GitHub release build omits optional `age`. The sealed-output dependency is pinned to an upstream revision using crypto-bigint 0.7.10, which includes the fix for RUSTSEC-2026-0387. The workflow retains one narrow `--ignore` for RUSTSEC-2026-0374 (unmaintained basic-toml), which is used by age's i18n procedural-macro/build tooling and not the runtime encryption path. `cargo audit` scans the complete lockfile without feature resolution; confirm the updated hosted audit and review the exception before distributing a sealed-output build.
- Regenerate the dependency-license CSV from the current lockfile. The checked-in inventory is an October 11 snapshot of 360 packages; the latest audit scanned 450.
- Obtain maintainer license-policy approval and document required notices.
- Test published packages on clean machines and verify release checksums. The current v0.1.0 assets predate the expanded Windows ARM64, ARMv7, RISC-V64, and musl matrix.
- The release workflow signs `SHA256SUMS.txt` with a detached OpenPGP signature and verifies the result before publishing. To enable it, create the GitHub Environment named `release-signing` under **Settings → Environments**, then add these environment values:
  - Secret `GPG_PRIVATE_KEY`: armored export of the dedicated signing subkey, including its secret material and the public key.
  - Secret `GPG_PASSPHRASE`: the non-empty passphrase protecting that secret key.
  - Variable `GPG_FINGERPRINT`: the full fingerprint of the primary public key (40 or 64 hexadecimal characters, depending on key type).
  Configure the environment to require approval and permit deployments only from the protected release branch. Keep the primary key offline and store only a dedicated signing subkey in Actions. The workflow fails before publishing if the values are missing, malformed, or do not match; it signs and verifies the checksum manifest before uploading assets. Publish the full public-key fingerprint independently and distribute the public key through a separate trusted channel. GitHub artifact attestations are an additional provenance layer when repository visibility/plan supports them; private repositories require GitHub Enterprise Cloud for attestations.
- Define supported release policy and update/uninstall guidance.

**Status:** The signing workflow is configured but not yet exercised because no key/fingerprint has been supplied. The updated dependency audit has not yet passed a post-change hosted run; the only remaining exception is documented above. The stale license inventory remains open. Current release assets do not yet cover all configured targets.

## Production-ready exit criteria

A release may be described as production-ready only after the gates above have owners and current evidence, the dependency audit is resolved or explicitly reviewed, the license inventory is refreshed and approved, target-device behavior is validated, and package/secret-output acceptance is complete. See [STATUS.md](STATUS.md), [docs/validation.md](docs/validation.md), and [docs/dependency-license-inventory.md](docs/dependency-license-inventory.md).
