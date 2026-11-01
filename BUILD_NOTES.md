# Build notes

**Documentation snapshot: 2026-11-01.**

## Build configuration

The default Cargo feature set enables CPU search, wgpu GPU support, OpenCL, Vulkan, and Windows Direct3D 11. The wgpu build includes GL/GLES and the platform-specific Direct3D 12 and Metal backends. Runtime availability depends on the operating system, adapter, driver, and loader. CUDA is opt-in and requires the NVIDIA CUDA Toolkit and nvcc.

Linux cross-target jobs use the cross-openssl feature to build OpenSSL for the target. Android cross builds use android-cross and target Android API 30 or newer. A native Termux build retains the default GPU features, subject to the phone's installed drivers and runtimes.

## Latest hosted CI evidence

The committed main revision b32ca046 passed all nine jobs in [Rust CI run 41292225784](https://github.com/MinitorMHS/rustovan/actions/runs/41292225784). The run includes Windows/Linux tests, macOS compile/test coverage, Windows ARM64, Linux ARMv7, Linux RISC-V64, and Linux musl build jobs.

This CI run validates the committed revision only. Changes made after that revision require their own CI run before release.

## Hardware evidence and limits

See [docs/validation.md](docs/validation.md) for device-specific evidence. Windows Intel HD Graphics 4400 has recorded OpenGL, OpenCL, and DX11 PBKDF2 acceptance. Termux Adreno 618 has recorded OpenGL acceptance and limited Vulkan parity/search checks; Vulkan throughput there is about two candidates per second. macOS CI compiles and tests the Metal configuration, but no Apple GPU runtime result is recorded here. CUDA compilation/runtime and physical multi-GPU scaling remain unverified.

## Windows CUDA

The CUDA build script selects nvcc.exe, MSVC object/library extensions, and the CUDA lib/x64 runtime directory on Windows. It invokes nvcc --lib and tracks CUDA environment variables for rebuilds. Build and runtime acceptance still require an installed CUDA Toolkit and compatible NVIDIA hardware.

## Remaining release checks

Production readiness remains blocked by the current dependency audit findings, stale dependency-license inventory, clean-machine packaging and secret-file failure-path checks, physical Apple Metal/multi-GPU testing, CUDA verification, sustained cross-device performance, and independent provenance for legacy wallet fixtures. See [STATUS.md](STATUS.md) and [RELEASE_PLAN.md](RELEASE_PLAN.md).