# Native CUDA backend

This directory contains the NVIDIA-specific CUDA implementation of the BIP39 PBKDF2-HMAC-SHA512 stage.

## Build

The Rust `cuda` feature invokes `nvcc` from `build.rs` and links the resulting static CUDA object. CUDA Toolkit 13.x is the current development target. NVIDIA documents `nvcc` as the compiler for `.cu` device code and the CUDA runtime as the host API. See the official CUDA Programming Guide.

Example:

```bash
cargo run --release --features cuda -- --backend cuda --prefix UQ
```

The backend is optional. Builds without `--features cuda` do not require CUDA.

## Scope of this stage

CUDA currently accelerates BIP39 PBKDF2-HMAC-SHA512. SLIP-0010, Ed25519 and TON StateInit hashing remain CPU verified until their CUDA kernels are added and cross-checked against the Rust reference implementation.

The Rust layer must fail closed on CUDA initialization/runtime errors; it must never silently substitute CPU work for an explicitly requested CUDA backend.
