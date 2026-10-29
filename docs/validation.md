# Validation strategy

## CPU reference pipeline

The CPU path is the correctness reference:

1. Generate 128 bits of operating-system-random entropy and a checksummed 12-word English BIP39 mnemonic.
2. Derive the BIP39 seed with PBKDF2-HMAC-SHA512 (2048 rounds).
3. Derive SLIP-0010 Ed25519 at m/44'/607'/account'.
4. Derive the Ed25519 public key.
5. Construct the selected TON wallet contract through the Rust TON SDK.
6. Derive the account hash and encode the requested friendly-address flags.
7. Apply validated prefix/suffix/exact matching rules.

The BIP39 seed test uses the published TREZOR reference vector. Rust unit tests also hold fixed private/public-key vectors for account indices 0, 1, 42, 32767, and 2147483647. Their source provenance has not yet been fully recorded, so those tests are regression evidence and are not counted as an independent derivation audit.

The deterministic address matrix covers all ten wallet versions and checks mainnet/testnet tags, bounceability tags, and CRC. A separate Rust cross-check tool using the tonutils SDK independently recomputes V4R2 and W5 addresses from a fixed public key in mainnet/testnet and bounceable/non-bounceable modes:

    cargo run --manifest-path tools/rust-ton-crosscheck/Cargo.toml --release --locked

The Rust cross-check is also run by CI. Independent wallet-address fixtures for V1R1 through V4R1, plus a documented source for each fixture, remain a release requirement. Existing TON address fixtures for V3R1, V3R2, V4R2, and W5 remain regression checks until their source provenance is recorded.

## Unit and compile checks

    cargo fmt --all -- --check
    cargo fmt --manifest-path tools/rust-ton-crosscheck/Cargo.toml -- --check
    cargo test --locked
    cargo run --manifest-path tools/rust-ton-crosscheck/Cargo.toml --release --locked
    cargo check --locked
    cargo check --no-default-features --features cpu --locked
    cargo build --release --locked

The CLI installs a Ctrl+C handler and shares a Rust cancellation token with CPU, GPU, and hybrid search. CPU workers stop between candidates; GPU work stops after the current batch. Ctrl+C was exercised during CPU (debug), OpenGL (debug/release), and hybrid (release) searches on the HD 4400 host; each returned `search cancelled by user`.

Rust tests verify concurrent GPU batch reservations, final partial batches, and refund/re-reservation after simulated batch failure; one-candidate OpenGL and hybrid exact searches plus CPU-only auto fallback were exercised. A bounded hybrid runtime search crossed multiple 8-candidate batches and stopped cleanly at a 100-candidate budget. A no-limit hybrid OpenGL search using wallet v5r1, suffix sds, account 2, and gpu-batch 1000 matched after 1,910,163 candidates. One finite OpenGL fence-wait slice was logged and retried; the search completed through OpenGL without CPU fallback. GPU polling retries transient timeout slices with a 120-second per-wait guard; a genuinely stalled GPU beyond that limit and injected runtime GPU-error fallback remain unverified. --exact must always include --max-candidates because an exact random wallet address is computationally infeasible to find. Exact inputs must be valid 48-character friendly addresses with a supported tag and valid CRC16; tests cover byte-exact matching plus invalid CRC/tag rejection, while --case-sensitive controls only prefix/suffix matching.

## GPU validation

GPU currently owns only PBKDF2. It must pass the standard BIP39 TREZOR vector:

- mnemonic: abandon repeated eleven times followed by about
- passphrase: TREZOR
- expected seed: c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04

Run the selected backend self-test:

    cargo run --release --locked -- gpu-self-test --backend vulkan
    cargo run --release --locked -- gpu-self-test --backend opengl
    cargo run --release --locked --features cuda -- gpu-self-test --backend cuda

The gpu-self-test command compares GPU output with the CPU reference at batch sizes 1, 2, and the configured maximum, using three distinct mnemonics per batch, blank/TREZOR, Unicode and normalization-equivalent passphrases, and the maximum 99-byte normalized passphrase, plus over-limit rejection. Search also performs a quick known-answer check before processing candidates. A candidate that matches a vanity pattern must have its BIP39 seed recomputed from the mnemonic on the CPU and compared before returning any wallet material.

Windows 10 verification on 2026-10-14: cargo run --release --locked -- gpu-self-test --backend opengl passed. The Unicode and normalization-equivalent GPU seeds matched the CPU BIP39 reference.

OpenGL/auto GPU batches receive ASCII passphrases directly. For Unicode passphrases, the CPU applies BIP39 NFKD normalization and passes the resulting UTF-8 salt to the GPU; PBKDF2 remains on the GPU. The salt limit is 107 bytes total, including the fixed eight-byte mnemonic prefix. A passphrase over 99 normalized UTF-8 bytes uses CPU search for that run. Hybrid mode retains a CPU candidate leg alongside GPU batches.

## W5 wallet ID

The W5R1 implementation uses TON's client-context wallet ID:

context = 1-bit marker | workchain:int8 | wallet_version:uint8 | subwallet:uint15

wallet_id = network_global_id XOR context

Unit checks cover default mainnet/testnet values and reject subwallets above the supported range. The Rust cross-check independently validates default W5 addresses in both network modes and both bounceability settings. Non-default W5 wallet IDs and workchains still need independent fixtures.

## Hardware and release validation

The Intel HD 4400 OpenGL PBKDF2 acceptance matrix passed on this Windows 10 host in the optimized build. Vulkan SDK `vulkaninfo --summary` reports no driver/ICD, and Intel lists Vulkan as unavailable for HD Graphics 4400 on Windows; CUDA still lacks a compatible toolkit/device here. Repeat the matrix on another supported Windows GPU/vendor before making a broader hardware claim. Record OS, driver, device, backend selected, batch size, self-test result, and CPU/GPU comparison result.

Windows secret output is assembled in a protected sibling temporary file and committed atomically without replacing an existing destination. A synthetic writer unit test now covers successful commit, refusing to overwrite an existing file, and temporary-file cleanup. It does not verify the current writer ACL on Windows, partial-write failures, reparse-point behavior, forced termination/recovery, or clean-machine package acceptance; these remain release gates along with dependency/security review and hosted CI execution.
