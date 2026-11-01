# GPU roadmap

## Implemented

- GPU-assisted PBKDF2-HMAC-SHA512 for BIP39, with CPU parity checks and backend-specific dispatch tuning.
- Shared WGSL PBKDF2 kernel paths for wgpu Vulkan, GL/GLES, Direct3D 12, and Metal; a native Direct3D 11 path; OpenCL; and an opt-in native CUDA implementation.
- GPU backend/device enumeration, explicit selection, Auto/Hybrid route calibration, per-device batch sizing, GPU result processing, and fail-closed checks for invalid GPU results.
- Multi-device selection and PBKDF2 parity gating for supported adapters. Physical multi-GPU throughput/scaling is still unverified.
- CUDA PBKDF2 kernel, device enumeration, Rust FFI, known-answer self-test, and fail-closed explicit selection are implemented. CUDA build and runtime validation remain outstanding.

Implementation status does not mean each backend has been compiled or exercised on every vendor/device. See [docs/validation.md](validation.md) and [STATUS.md](../STATUS.md) for evidence.

## Remaining implementation for a fully GPU-resident search

1. GPU-native BIP39 entropy and checksum generation.
2. GPU-native SLIP-0010 hardened derivation.
3. GPU-native Ed25519 public-key derivation.
4. GPU-native wallet data/state-init construction and TON cell hashing.
5. GPU-native address encoding and matching.
6. Device-side match/result queues to avoid per-candidate host readback.

Until those stages have independent CPU parity, wallet derivation and matching remain CPU work.

## Remaining validation and performance work

- Validate Vulkan, OpenCL, OpenGL/GLES, DX11, DX12, Metal, and CUDA on representative physical devices and drivers.
- Run cross-backend test vectors and verify large dispatches, cancellation, device loss, memory pressure, and thermal stability.
- Measure multi-GPU concurrency and scaling on physical multi-GPU systems.
- Characterize sustained CPU, GPU-only, and Hybrid throughput across representative architectures.

Auto/Hybrid calibration and CPU/GPU scheduling are implemented; wider hardware measurements remain open.