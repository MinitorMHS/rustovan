# GPU kernels

## `pbkdf2.wgsl`

Production-oriented first-stage compute kernel for BIP39 PBKDF2-HMAC-SHA512 (2048 rounds).
It is backend-neutral WGSL and is dispatched through `wgpu` on Vulkan or OpenGL/GLES.

The kernel was checked against the canonical BIP39 `TREZOR` seed by reproducing its
64-bit arithmetic in a host-side reference test. Runtime GPU validation is still required
on actual hardware before a release can claim cross-vendor hardware support.

## Planned next kernels

- `slip10_ed25519.wgsl`: derive `m/44'/607'/account'` and Ed25519 public keys.
- `ton_state_init.wgsl`: serialize the standard wallet data cells and hash StateInit.
- `match.wgsl`: compare raw account IDs before friendly-address base64/CRC formatting.

The final matcher should operate on the raw 32-byte account ID rather than computing
friendly-address strings for every candidate.
