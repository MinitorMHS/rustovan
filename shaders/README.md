# GPU kernels

## pbkdf2.wgsl

This backend-neutral WGSL compute kernel implements the BIP39 PBKDF2-HMAC-SHA512 stage with 2048 rounds. The wgpu path targets Vulkan, Direct3D 12, Metal, and GL/GLES where enabled. Windows Direct3D 11 translates the shared WGSL through Naga to HLSL. OpenCL and CUDA use separate backend implementations.

The kernel is checked against the canonical BIP39 TREZOR seed and CPU reference outputs. Recorded runtime acceptance includes OpenGL/OpenCL/DX11 on Intel HD Graphics 4400 and OpenGL on Android Adreno 618. Termux Vulkan passes known-answer parity and very small wallet searches but is extremely slow; macOS Metal has compile/test CI coverage without a physical runtime result. CUDA remains unverified. See [docs/validation.md](../docs/validation.md) for the evidence and limits.

## Planned device-side stages

- GPU-native BIP39 entropy/checksum generation.
- slip10_ed25519.wgsl for hardened derivation and public-key generation.
- ton_state_init.wgsl for wallet data cells and StateInit hashing.
- A matcher over raw 32-byte account IDs and a device-side result queue.

Wallet derivation, TON hashing/address construction, and matching remain CPU stages. These kernels are roadmap items, not implemented production features.