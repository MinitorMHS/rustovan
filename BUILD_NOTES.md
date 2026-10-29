# Build notes

The Windows 10 workspace now has a successful default-feature compiler check and unit-test run. The dependency graph is locked in Cargo.lock for reproducible builds.

Commands used:

    cargo check --locked
    cargo test --locked

The default build includes CPU search plus wgpu Vulkan/OpenGL-GLES support. The Intel HD 4400 OpenGL PBKDF2 self-test and a one-candidate W5 GPU-assisted search passed on this host. CUDA feature compilation remains unverified.

## Windows CUDA

The CUDA build script now selects nvcc.exe, MSVC object/library extensions, and the CUDA lib/x64 runtime directory for Windows. It invokes nvcc --lib to package the compiled object and watches CUDA environment variables for rebuilds. This path still needs a build against an installed CUDA Toolkit and Visual Studio C++ Build Tools.

## Remaining runtime checks

See [docs/validation.md](docs/validation.md) for CPU known-answer fixtures, GPU self-tests, and hardware release validation. Source-level shader fixes alone do not prove driver/runtime correctness.

A Windows CI workflow is defined in .github/workflows/rust.yml. It has not been executed by GitHub Actions from this workspace.
