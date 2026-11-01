# Native CUDA backend

This directory contains the NVIDIA-specific CUDA implementation of the BIP39 PBKDF2-HMAC-SHA512 stage.

## Build requirements

The Rust cuda feature invokes nvcc from build.rs and links the resulting static CUDA object. The documented development target is CUDA Toolkit 13.x; confirm the installed Toolkit and driver compatibility before building. The backend is optional. Builds that omit the cuda feature do not require nvcc or CUDA runtime libraries.

On Windows the build script selects nvcc.exe, MSVC object/library formats, and the CUDA lib/x64 runtime directory. The compiler packages the CUDA object into a static library.

## Run

    cargo run --release --features cuda -- --backend cuda --prefix UQ

An explicitly requested CUDA backend must fail with a useful error if its compiler, runtime, device, or kernel is unavailable. It must not silently substitute CPU work.

## Scope and validation

CUDA currently accelerates BIP39 PBKDF2-HMAC-SHA512. SLIP-0010, Ed25519, TON wallet construction, StateInit hashing, and vanity matching remain CPU stages. The latest hosted CI matrix does not build the opt-in CUDA feature. No CUDA Toolkit build or NVIDIA runtime acceptance is recorded as of 2026-11-01; see [STATUS.md](../STATUS.md) before relying on this backend.