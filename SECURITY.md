# Security policy

## Supported releases

Once releases are published, only the latest published release is supported for security fixes. This project has not established a supported-release history yet; confirm this policy before the first production release.

## Reporting a vulnerability

Please report vulnerabilities through the repository's private vulnerability reporting feature under **Security → Advisories → Report a vulnerability**. Do not post a suspected vulnerability in a public issue or discussion. If private reporting is unavailable, use the private security contact published by the repository maintainers.

Do not include a real recovery phrase, private key, passphrase, or wallet backup in a report. Use synthetic test material and redact logs that could expose secrets.

Reports should include the affected release or commit, platform and backend, impact, and steps to reproduce with non-secret test data. Coordinate public disclosure with the maintainers so users can receive a fix or mitigation first.

## Scope

The project considers reports involving incorrect wallet derivation or addresses, exposure of recovery phrases or private keys, unsafe secret-file handling, GPU results that can produce an incorrect wallet, and vulnerable dependencies.

## Secret-handling review (2026-10-22)

The current source review found these controls in the wallet search path:

- Search output withholds the mnemonic and private key by default. `--show-secrets` is explicit and warns before printing; daemon mode rejects it.
- `--output` writes the address, mnemonic, and private key as **plaintext** to a new file and refuses to replace an existing file. Unix creates the file with mode `0600`; Windows requests an owner-only DACL. The temporary file is created beside the destination and removed on normal error/return paths.
- The optional `sealed-output` build embeds an age X25519 recipient public key supplied through `TON_VANITY_AGE_RECIPIENT` during compilation. It writes only age ciphertext to the protected result file, creates a timestamped `.age` result by default, and rejects `--show-secrets`. The recipient is not accepted as a runtime option; the corresponding private identity must remain with the operator. The public recipient is not secret and can be extracted from the executable.
- The prompted BIP39 passphrase is not put in command arguments. `SearchConfig`, returned wallet secrets, BIP39 mnemonic indices, derived seeds/keys, normalized Unicode passphrase copies, host-side GPU buffers, and the TON crate's key-pair type use zeroization on drop. GPU backends also clear their temporary device/readback buffers where their APIs permit it. Rust cannot guarantee clearing compiler-created copies or opaque driver allocations.
- Every additional GPU selected by `--gpu-devices` is checked against the CPU BIP39 reference at the actual configured dispatch size before it receives search work. Devices that fail this check are skipped. This protects against a device-specific bad batch result; simultaneous multi-GPU operation has not yet been exercised on physical multi-GPU hardware.
- Daemon logs do not intentionally contain wallet credentials. Daemon mode rejects secret printing, and without `--output` the found credentials are withheld.

These are best-effort process-level controls, not a guarantee that secrets cannot be recovered from a compromised machine. In ordinary builds, `--show-secrets` exposes credentials to the terminal and any shell/session capture, and `--output` leaves a readable plaintext backup at the chosen path. In sealed-output builds, encryption protects the result file from readers who lack the embedded recipient's private identity, but it cannot stop the recipient of the binary from modifying/replacing it, attaching a debugger, inspecting process memory, or capturing output before encryption. The mode is suitable for accidental disclosure and at-rest protection when the runner is trusted; it is not a secure remote-computation protocol against a malicious runner. Zeroization does not lock memory or control operating-system paging, crash dumps, debugger access, malware, display capture, or opaque GPU-driver/device copies. Flash storage also cannot be reliably sanitized by deleting a file.

The code audit did not run a fresh RustSec advisory database check: `cargo audit --locked` could not run because `cargo-audit` is not installed in this environment. The previous successful scan recorded in `STATUS.md` is dated 2026-10-10; dependency status may have changed and is not confirmed by this review. The sealed-output option adds the optional Rust `age` crate and its transitive dependencies, which need inclusion in the next license and advisory review. Hardware-specific GPU clearing behavior and simultaneous physical multi-GPU operation also need verification on the target machines.
