# Security policy

## Supported releases

The project has published v0.1.0 but has not established a formal security-support window. Until maintainers publish one, report issues affecting the latest release or the main branch. Development status is not production certification.

## Reporting a vulnerability

Please report vulnerabilities through the repository's private vulnerability reporting feature under **Security → Advisories → Report a vulnerability**. Do not post a suspected vulnerability in a public issue or discussion. If private reporting is unavailable, use the private security contact published by the repository maintainers.

Do not include a real recovery phrase, private key, passphrase, or wallet backup in a report. Use synthetic test material and redact logs that could expose secrets.

Reports should include the affected release or commit, platform and backend, impact, and steps to reproduce with non-secret test data. Coordinate public disclosure with the maintainers so users can receive a fix or mitigation first.

## Scope

The project considers reports involving incorrect wallet derivation or addresses, exposure of recovery phrases or private keys, unsafe secret-file handling, GPU results that can produce an incorrect wallet, and vulnerable dependencies.

## Dependency audit status (2026-11-02)

The latest hosted audit reported crypto-bigint 0.5.5 ([RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387), timing variability in operations intended to be constant-time) and basic-toml 0.1.10 as unmaintained ([RUSTSEC-2026-0374]). The optional `age` dependency is now pinned to an upstream revision that updates the elliptic-curve/HPKE dependency family to crypto-bigint 0.7.10, above the advisory's fixed version. The only remaining workflow exception is RUSTSEC-2026-0374: dependency-tree inspection shows basic-toml is used by age's i18n procedural-macro/build tooling, not the runtime wallet-encryption path. `cargo audit` scans the full lockfile without feature resolution, so the workflow must report that exception even though normal GitHub release binaries omit optional `age` and are unaffected. The sealed-output dependency tree still includes this build-time parser; the exception is narrowly documented and is not a general security clearance. The updated audit has not yet been confirmed by a hosted run.

## Secret-handling review (2026-11-02)

The current source review found these controls in the wallet search path:

- Search output withholds the mnemonic and private key by default. `--show-secrets` is explicit and warns before printing; daemon mode rejects it.
- `--output` writes the address, mnemonic, and private key as **plaintext** to a new file and refuses to replace an existing file. Unix creates the file with mode `0600`; Windows requests an owner-only DACL. The temporary file is created beside the destination and removed on normal error/return paths.
- The optional `sealed-output` build embeds an age recipient supplied through `TON_VANITY_AGE_RECIPIENT` during compilation. The Rust age library accepts classic X25519 recipients and tagged post-quantum hardware recipients beginning `age1tagpq1`; standard software post-quantum `age1pq1` recipients are rejected. It writes only age ciphertext to the protected result file, creates a timestamped `.age` result by default, and rejects `--show-secrets`. The recipient is not accepted at runtime; the corresponding private identity must remain with the operator. The public recipient is not secret and can be extracted from the executable.
- The prompted BIP39 passphrase is not put in command arguments. `SearchConfig`, returned wallet secrets, BIP39 mnemonic indices, derived seeds/keys, normalized Unicode passphrase copies, host-side GPU buffers, and the TON crate's key-pair type use zeroization on drop. GPU backends also clear their temporary device/readback buffers where their APIs permit it. Rust cannot guarantee clearing compiler-created copies or opaque driver allocations.
- Unix daemon state rejects symlinked or untrusted-owner/writable ancestor directories, opens the final directory without following a symlink, and sets daemon state files to mode `0600`; state file opens also reject symlinks. Windows daemon directories receive an explicit owner-only DACL and reject final-component reparse points. The Windows ACL path is covered by a local test; Unix runtime behavior remains for maintainer verification.
- Every additional GPU selected by `--gpu-devices` is checked against the CPU BIP39 reference at the actual configured dispatch size before it receives search work. Devices that fail this check are skipped. This protects against a device-specific bad batch result. Simultaneous multi-GPU scheduling is implemented but has not been verified or runtime-tested on physical multi-GPU hardware.
- Daemon logs do not intentionally contain wallet credentials. Daemon mode rejects secret printing, and without `--output` the found credentials are withheld.

These are best-effort process-level controls, not a guarantee that secrets cannot be recovered from a compromised machine. In ordinary builds, `--show-secrets` exposes credentials to the terminal and any shell/session capture, and `--output` leaves a readable plaintext backup at the chosen path. In sealed-output builds, encryption protects the result file from readers who lack the embedded recipient's private identity, but it cannot stop the recipient of the binary from modifying/replacing it, attaching a debugger, inspecting process memory, or capturing output before encryption. The mode is suitable for accidental disclosure and at-rest protection when the runner is trusted; it is not a secure remote-computation protocol against a malicious runner. Zeroization does not lock memory or control operating-system paging, crash dumps, debugger access, malware, display capture, or opaque GPU-driver/device copies. Flash storage also cannot be reliably sanitized by deleting a file.

## Cryptographic review (2026-11-02)

A source-level review found the intended derivation path to be internally consistent: 128-bit OS-seeded entropy is converted to a 12-word English BIP39 mnemonic; the BIP39 seed uses the library's 2048-round PBKDF2-HMAC-SHA512 implementation with NFKD passphrase handling; the TON key uses hardened SLIP-0010 Ed25519 path `m/44'/607'/account'`; wallet contract/address construction is delegated to the Rust TON crate except for the fixed WalletTg trampoline, whose BOC hash is checked at runtime; friendly addresses encode bounceability/testnet flags and CRC16. The GPU accelerates PBKDF2 only. A returned GPU candidate is checked against CPU-derived BIP39 material before the wallet is returned. This review does not prove driver correctness, every candidate generated by opaque GPU hardware, or interoperability for every legacy wallet version; device acceptance and fixture provenance are documented validation limits rather than claims of cryptographic assurance.

The derivation buffers and wallet-secret containers use zeroization, and the TON dependency's current `KeyPair` type zeroizes its owned secret-key array on drop. This remains best-effort: compiler/library copies, crash dumps, debuggers, and GPU-driver allocations are outside the application's guarantee. The code has not been reviewed by an external security auditor.

## Release signatures

The release workflow is configured to create and verify a detached OpenPGP signature over `SHA256SUMS.txt` before publishing assets. It only releases from `main` and reads the signing key after the `release-signing` GitHub Environment's protection rules pass. Keep only a dedicated signing subkey in Actions, protect its passphrase as an environment secret, and publish the full public-key fingerprint through a separate trusted channel. The signature authenticates the checksum manifest and its listed assets when the fingerprint is independently trusted; it does not independently prove source-to-binary reproducibility or protect against a compromised repository, workflow, or Actions runner. The installer currently verifies the release checksum but does not automatically verify the OpenPGP signature, so users must verify the signature and fingerprint manually.

For an external audit, freeze a commit and provide the auditor with the exact default and `sealed-output` feature sets, dependency lockfile, supported release workflows, and synthetic known-answer vectors. Request a manual review of BIP39 entropy and NFKD/PBKDF2, SLIP-0010 path/account handling, every wallet contract and WalletTg BOC, address tags/CRC, GPU parity and failure behavior, secret lifetime/zeroization, output-file and daemon paths, and release supply chain. Ask for a severity-ranked written report, reproducible findings using synthetic secrets only, and a follow-up review after fixes. Use an auditor with Rust and TON wallet/Ed25519 experience; never send real mnemonics, passphrases, private keys, or live wallet files.

A fresh local RustSec scan could not be run in this Windows workspace: `cargo-audit` is absent and crates.io DNS prevented installing it. The updated GitHub audit workflow is ready to check the revised lockfile after the changes are pushed. Windows daemon-directory ACLs were tested locally against their actual DACLs; Unix daemon runtime behavior remains for maintainer verification. The OpenPGP workflow configuration has not been exercised with a signing key and awaits the maintainer's key/environment verification. GPU drivers and devices remain outside the application's ability to prove complete memory erasure or protect against a compromised host.
