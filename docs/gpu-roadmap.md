# GPU roadmap

## Completed in this step

- Native CUDA `.cu` backend.
- CUDA runtime device enumeration.
- CUDA PBKDF2-HMAC-SHA512 (2048 rounds, 64-byte output).
- Rust FFI and explicit backend selection.
- CUDA known-answer self-test using the BIP39 `TREZOR` vector.
- Fail-closed behavior when CUDA is explicitly requested but unavailable.

## Remaining for full GPU vanity search

1. GPU-native BIP39 entropy/checksum generation.
2. GPU-native SLIP-0010 hardened derivation.
3. GPU-native Ed25519 public-key derivation.
4. GPU-native TON wallet data/state-init construction.
5. GPU-native TON cell hashing.
6. GPU-native address encoding/matching.
7. Device-side match/result queue to avoid per-candidate readback.
8. CUDA/Vulkan/OpenGL cross-backend test vectors.
9. Throughput calibration and hybrid CPU/GPU scheduling.
10. Hardware benchmarking on NVIDIA, AMD and Intel devices.

The CPU implementation remains the correctness oracle until all GPU stages match it byte-for-byte.
