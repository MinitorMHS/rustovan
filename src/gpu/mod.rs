use anyhow::Result;
#[cfg(feature = "gpu")]
use anyhow::anyhow;
#[cfg(feature = "gpu")]
use bip39::Mnemonic;
use std::fmt;
#[cfg(all(feature = "d3d11", target_os = "windows"))]
pub mod d3d11;
#[cfg(feature = "opencl")]
pub mod opencl;
#[cfg(feature = "gpu")]
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[cfg(feature = "gpu")]
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    Auto,
    Cpu,
    Vulkan,
    Dx12,
    Dx11,
    Metal,
    OpenGl,
    OpenCl,
    Cuda,
    Hybrid,
}
impl std::str::FromStr for GpuBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "vulkan" => Ok(Self::Vulkan),
            "dx12" | "d3d12" => Ok(Self::Dx12),
            "dx11" | "d3d11" => Ok(Self::Dx11),
            "metal" => Ok(Self::Metal),
            "opengl" | "gl" => Ok(Self::OpenGl),
            "opencl" | "ocl" => Ok(Self::OpenCl),
            "cuda" => Ok(Self::Cuda),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(format!("unknown backend: {s}")),
        }
    }
}
impl fmt::Display for GpuBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub backend: GpuBackend,
    pub name: String,
    pub vendor: String,
    pub device_type: String,
}

pub fn capability_report() -> String {
    let mut s = String::from("Backend capability report:\n  CPU: enabled\n");
    s.push_str(if cfg!(feature = "gpu") {
        "  wgpu: OpenGL/GLES, Direct3D 12 (Windows), and Metal (Apple) compiled\n"
    } else {
        "  wgpu GPU backends: not compiled\n"
    });
    s.push_str(if cfg!(all(feature = "d3d11", target_os = "windows")) {
        "  Direct3D 11: native compute backend compiled on Windows\n"
    } else {
        "  Direct3D 11: not compiled for this target\n"
    });
    s.push_str(if cfg!(feature = "opencl") {
        "  OpenCL: compiled as a runtime-loaded backend using native 64-bit SHA-512 arithmetic
"
    } else {
        "  OpenCL: not compiled
"
    });
    s.push_str(if cfg!(feature = "vulkan") {
        "  Vulkan: compiled; runtime use requires a compatible Vulkan driver/device\n"
    } else {
        "  Vulkan: not compiled; rebuild with --features vulkan\n"
    });
    s.push_str(if cfg!(feature = "cuda") {
        "  CUDA: compiled with the CUDA Toolkit; runtime use requires a compatible NVIDIA driver/device\n"
    } else {
        "  CUDA: not compiled; rebuild with --features cuda and CUDA Toolkit\n"
    });
    s.push_str(if cfg!(feature = "gpu") {
        "  Auto search: benchmarks CPU, available wgpu/OpenCL GPU backends, and concurrent CPU+GPU throughput.\n"
    } else {
        "  Auto search: CPU only (GPU feature not compiled).\n"
    });
    s.push_str("  GPU stage: BIP39 PBKDF2-HMAC-SHA512; SLIP-0010/Ed25519/TON hashing remain CPU reference stages.\n");
    s
}
pub fn devices() -> Result<Vec<DeviceInfo>> {
    #[allow(unused_mut)]
    let mut out = vec![DeviceInfo {
        backend: GpuBackend::Cpu,
        name: "CPU reference engine".into(),
        vendor: "host".into(),
        device_type: "CPU".into(),
    }];
    #[cfg(feature = "gpu")]
    {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        for adapter in adapters {
            let info = adapter.get_info();
            let backend = match info.backend {
                wgpu::Backend::Vulkan => GpuBackend::Vulkan,
                wgpu::Backend::Gl => GpuBackend::OpenGl,
                wgpu::Backend::Dx12 => GpuBackend::Dx12,
                wgpu::Backend::Metal => GpuBackend::Metal,
                _ => continue,
            };
            out.push(DeviceInfo {
                backend,
                name: info.name,
                vendor: info.vendor.to_string(),
                device_type: format!("{:?}", info.device_type),
            });
        }
    }
    #[cfg(all(feature = "d3d11", target_os = "windows"))]
    if let Ok((name, feature_level)) = d3d11::device_info() {
        out.push(DeviceInfo {
            backend: GpuBackend::Dx11,
            name,
            vendor: "Direct3D 11 hardware adapter".into(),
            device_type: format!("GPU (feature level {feature_level})"),
        });
    }
    #[cfg(feature = "opencl")]
    if let Ok(opencl_devices) = opencl::devices() {
        out.extend(opencl_devices.into_iter().map(|device| DeviceInfo {
            backend: GpuBackend::OpenCl,
            name: device.name,
            vendor: device.vendor,
            device_type: device.device_type,
        }));
    }
    Ok(out)
}

pub fn search_backend_name(s: &str) -> &str {
    match s.to_ascii_lowercase().as_str() {
        "auto" => "auto",
        "cpu" => "cpu",
        "vulkan" => "vulkan",
        "dx12" | "d3d12" => "dx12",
        "dx11" | "d3d11" => "dx11",
        "metal" => "metal",
        "opengl" | "gl" => "opengl",
        "opencl" | "ocl" => "opencl",
        "cuda" => "cuda",
        "hybrid" => "hybrid",
        _ => "unknown",
    }
}

pub struct GpuSeedEngineConfig {
    pub backend: GpuBackend,
    pub batch_size: usize,
}

/// Cross-vendor GPU PBKDF2 stage. The rest of the wallet pipeline remains CPU-verified.
#[cfg(feature = "gpu")]
struct GpuErrorScopes {
    internal: Option<wgpu::ErrorScopeGuard>,
    out_of_memory: Option<wgpu::ErrorScopeGuard>,
    validation: Option<wgpu::ErrorScopeGuard>,
}

#[cfg(feature = "gpu")]
impl GpuErrorScopes {
    fn push(device: &wgpu::Device) -> Self {
        Self {
            internal: Some(device.push_error_scope(wgpu::ErrorFilter::Internal)),
            out_of_memory: Some(device.push_error_scope(wgpu::ErrorFilter::OutOfMemory)),
            validation: Some(device.push_error_scope(wgpu::ErrorFilter::Validation)),
        }
    }

    fn finish(mut self, context: &str) -> Result<()> {
        // wgpu requires scopes to be popped in the reverse order they were pushed.
        let validation = self
            .validation
            .take()
            .ok_or_else(|| anyhow!("GPU validation scope was already popped"))?;
        let validation = pollster::block_on(validation.pop());
        let out_of_memory = self
            .out_of_memory
            .take()
            .ok_or_else(|| anyhow!("GPU out-of-memory scope was already popped"))?;
        let out_of_memory = pollster::block_on(out_of_memory.pop());
        let internal = self
            .internal
            .take()
            .ok_or_else(|| anyhow!("GPU internal scope was already popped"))?;
        let internal = pollster::block_on(internal.pop());
        if let Some(error) = validation.or(out_of_memory).or(internal) {
            anyhow::bail!("{context}: {error}");
        }
        Ok(())
    }
}

#[cfg(feature = "gpu")]
impl Drop for GpuErrorScopes {
    fn drop(&mut self) {
        // Early returns still need to empty the wgpu scope stack from its top down.
        if let Some(scope) = self.validation.take() {
            drop(scope.pop());
        }
        if let Some(scope) = self.out_of_memory.take() {
            drop(scope.pop());
        }
        if let Some(scope) = self.internal.take() {
            drop(scope.pop());
        }
    }
}

/// Wait for queued GPU work while tolerating finite OpenGL fence-wait slices.
///
/// The wgpu OpenGL backend caps a single native fence wait to roughly 2.1 seconds,
/// even when an indefinite wait is requested. A timeout from one poll therefore
/// means that this wait slice elapsed; it does not mean that the submission
/// failed. Keep polling until it completes, with a bounded overall wait so a
/// genuinely stalled device cannot hang the process.
#[cfg(feature = "gpu")]
fn wait_for_gpu_work(device: &wgpu::Device, context: &str) -> Result<()> {
    const MAX_WAIT: Duration = Duration::from_secs(120);

    let started = Instant::now();
    let mut reported_retry = false;
    loop {
        match device.poll(wgpu::PollType::wait_indefinitely()) {
            Ok(_) => return Ok(()),
            Err(wgpu::PollError::Timeout) if started.elapsed() < MAX_WAIT => {
                if !reported_retry {
                    eprintln!(
                        "GPU wait slice elapsed while {context}; continuing to wait for the submitted work"
                    );
                    reported_retry = true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(wgpu::PollError::Timeout) => {
                anyhow::bail!("{context}: GPU work did not complete within {MAX_WAIT:?}");
            }
            Err(error) => return Err(anyhow!("{context}: {error}")),
        }
    }
}
#[cfg(feature = "gpu")]
struct WipeReadbackOnDrop<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    buffer: &'a wgpu::Buffer,
    secret_buffers: [&'a wgpu::Buffer; 4],
    mapped: bool,
    wiped: bool,
}

#[cfg(feature = "gpu")]
impl WipeReadbackOnDrop<'_> {
    fn schedule_readback_wipe(&mut self) -> Result<()> {
        if self.wiped {
            return Ok(());
        }
        if self.mapped {
            self.buffer.unmap();
            self.mapped = false;
        }
        // Inputs and the GPU seed buffer were cleared in the PBKDF2 submission.
        // Queue the readback wipe without waiting: queue order guarantees it runs
        // before later GPU work, while avoiding a second fence on every batch.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pbkdf2-readback-wipe"),
            });
        encoder.clear_buffer(self.buffer, 0, None);
        self.queue.submit(Some(encoder.finish()));
        self.wiped = true;
        Ok(())
    }

    fn wipe(&mut self) -> Result<()> {
        if self.wiped {
            return Ok(());
        }
        if self.mapped {
            self.buffer.unmap();
            self.mapped = false;
        }
        let error_scopes = GpuErrorScopes::push(self.device);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pbkdf2-readback-wipe"),
            });
        encoder.clear_buffer(self.buffer, 0, None);
        for buffer in self.secret_buffers {
            encoder.clear_buffer(buffer, 0, None);
        }
        self.queue.submit(Some(encoder.finish()));
        let poll_result = wait_for_gpu_work(self.device, "waiting for GPU PBKDF2 buffer wipe");
        let scope_result = error_scopes.finish("GPU PBKDF2 buffer wipe failed");
        poll_result?;
        scope_result?;
        self.wiped = true;
        Ok(())
    }
}

#[cfg(feature = "gpu")]
impl Drop for WipeReadbackOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.wipe() {
            eprintln!("warning: {error:#}");
        }
    }
}
#[cfg(feature = "gpu")]
pub struct GpuSeedEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    backend: GpuBackend,
    adapter_description: String,
    workgroup_size: u32,
    batch_size: usize,
    dispatch_batch_size: usize,
}

#[cfg(feature = "gpu")]
impl GpuSeedEngine {
    pub fn new(cfg: &GpuSeedEngineConfig) -> Result<Self> {
        Self::new_for_device(cfg, 0)
    }

    pub fn available_adapter_count(backend: GpuBackend) -> Result<usize> {
        let backends = Self::wgpu_backends(backend)?;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        Ok(pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .filter(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu)
            .count())
    }

    fn wgpu_backends(backend: GpuBackend) -> Result<wgpu::Backends> {
        if backend == GpuBackend::Vulkan && !cfg!(feature = "vulkan") {
            anyhow::bail!("Vulkan is not compiled in this release");
        }
        if backend == GpuBackend::Dx12 && !cfg!(target_os = "windows") {
            anyhow::bail!("Direct3D 12 is only available on Windows");
        }
        if backend == GpuBackend::Dx11 {
            anyhow::bail!("Direct3D 11 is initialized by the native DX11 engine");
        }
        if backend == GpuBackend::Metal && !cfg!(target_vendor = "apple") {
            anyhow::bail!("Metal is only available on Apple platforms");
        }
        let backends = match backend {
            GpuBackend::Vulkan => wgpu::Backends::VULKAN,
            GpuBackend::Dx12 => wgpu::Backends::DX12,
            GpuBackend::Metal => wgpu::Backends::METAL,
            GpuBackend::OpenGl => wgpu::Backends::GL,
            GpuBackend::Auto | GpuBackend::Hybrid => wgpu::Backends::GL,
            _ => wgpu::Backends::empty(),
        };
        if backends.is_empty() {
            anyhow::bail!("unsupported GPU backend for wgpu: {backend}");
        }
        Ok(backends)
    }

    pub fn new_for_device(cfg: &GpuSeedEngineConfig, device_index: usize) -> Result<Self> {
        let backends = Self::wgpu_backends(cfg.backend)?;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .filter(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu)
            .nth(device_index)
            .ok_or_else(|| anyhow!("no compatible GPU adapter at device index {device_index}"))?;
        let info = adapter.get_info();
        let adapter_description = format!(
            "{} vendor=0x{:04x} type={:?} driver={} {}",
            info.name, info.vendor, info.device_type, info.driver, info.driver_info
        );
        if info.device_type == wgpu::DeviceType::Cpu {
            anyhow::bail!(
                "selected adapter '{}' is a software CPU renderer, not a GPU",
                info.name
            );
        }
        if matches!(cfg.backend, GpuBackend::Vulkan) && info.backend != wgpu::Backend::Vulkan {
            anyhow::bail!("requested Vulkan but selected {:?}", info.backend);
        }
        if matches!(cfg.backend, GpuBackend::OpenGl) && info.backend != wgpu::Backend::Gl {
            anyhow::bail!("requested OpenGL/GLES but selected {:?}", info.backend);
        }
        // Adreno Vulkan has returned incorrect PBKDF2 output for larger
        // candidate batches. Use one invocation per workgroup; the tuner still
        // validates each candidate batch size before search uses it.
        let workgroup_size = if cfg.backend == GpuBackend::Vulkan
            && info.name.to_ascii_lowercase().contains("adreno")
        {
            1
        } else {
            64
        };
        let adapter_limits = adapter.limits();
        let mut required_limits = wgpu::Limits::downlevel_defaults();
        required_limits.max_storage_buffer_binding_size =
            adapter_limits.max_storage_buffer_binding_size;
        required_limits.max_buffer_size = adapter_limits.max_buffer_size;
        required_limits.max_compute_workgroups_per_dimension =
            adapter_limits.max_compute_workgroups_per_dimension;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("ton-vanity-gpu"),
                required_features: wgpu::Features::empty(),
                // The PBKDF2 shader uses three storage buffers per compute
                // stage. Requesting wgpu's full defaults asks for eight,
                // excluding mobile GLES/Vulkan adapters capped at four.
                required_limits,
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            }))?;
        let limits = device.limits();
        // One candidate occupies 128 bytes in the mnemonic buffer, 4 bytes in
        // the lengths buffer, and 64 bytes in the output buffer. Derive the
        // dispatch ceiling from the limits actually granted by wgpu, instead
        // of imposing a global candidate-count ceiling on every adapter.
        let binding_capacity = (limits.max_storage_buffer_binding_size as usize) / 128;
        let workgroup_capacity = (limits.max_compute_workgroups_per_dimension as usize)
            .saturating_mul(workgroup_size as usize);
        let device_batch_limit = binding_capacity
            .min(workgroup_capacity)
            .min(u32::MAX as usize)
            .max(1);
        let batch_size = if cfg.batch_size == 0 {
            device_batch_limit
        } else {
            cfg.batch_size.min(device_batch_limit)
        };
        let setup_errors = GpuErrorScopes::push(&device);
        let shader_source = include_str!("../../shaders/pbkdf2.wgsl").replace(
            "@compute @workgroup_size(64)",
            &format!("@compute @workgroup_size({workgroup_size})"),
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bip39-pbkdf2"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pbkdf2-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pbkdf2-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pbkdf2-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        wait_for_gpu_work(&device, "waiting for GPU pipeline initialization")?;
        setup_errors.finish("GPU pipeline initialization failed")?;
        Ok(Self {
            device,
            queue,
            pipeline,
            layout,
            backend: match info.backend {
                wgpu::Backend::Vulkan => GpuBackend::Vulkan,
                wgpu::Backend::Gl => GpuBackend::OpenGl,
                wgpu::Backend::Dx12 => GpuBackend::Dx12,
                wgpu::Backend::Metal => GpuBackend::Metal,
                _ => cfg.backend,
            },
            adapter_description,
            workgroup_size,
            batch_size,
            dispatch_batch_size: batch_size.min(256),
        })
    }

    pub fn backend(&self) -> GpuBackend {
        self.backend
    }

    pub fn adapter_description(&self) -> &str {
        &self.adapter_description
    }

    pub fn max_batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn set_dispatch_batch_size(&mut self, size: usize) -> Result<()> {
        if size == 0 || size > self.batch_size {
            anyhow::bail!(
                "GPU dispatch size must be between 1 and {}",
                self.batch_size
            );
        }
        self.dispatch_batch_size = size;
        Ok(())
    }

    pub fn dispatch_batch_size(&self) -> usize {
        self.dispatch_batch_size
    }

    /// Quick known-answer test used during automatic GPU backend startup.
    pub fn self_test(&self) -> Result<()> {
        let mnemonic = bip39::Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .map_err(|e| anyhow!("BIP39 vector parse failed: {e}"))?;
        for passphrase in ["TREZOR", "passphrase-ñ"] {
            let gpu_seeds =
                Zeroizing::new(self.derive_batch(std::slice::from_ref(&mnemonic), passphrase)?);
            let gpu_seed = gpu_seeds
                .first()
                .ok_or_else(|| anyhow!("GPU PBKDF2 self-test returned no seed"))?;
            let gpu = Zeroizing::new(*gpu_seed);
            let cpu = Zeroizing::new(mnemonic.to_seed(passphrase));
            if gpu[..] != cpu[..] {
                anyhow::bail!("GPU PBKDF2 self-test failed: GPU seed differs from CPU reference");
            }
        }
        Ok(())
    }

    /// Hardware acceptance matrix for batch sizes and supported BIP39 passphrases.
    pub fn acceptance_test(&self) -> Result<()> {
        let mnemonic = bip39::Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .map_err(|e| anyhow!("BIP39 vector parse failed: {e}"))?;
        let mut mnemonics = vec![mnemonic.clone()];
        for entropy in [[1u8; 16], [0x5a; 16]] {
            mnemonics.push(
                bip39::Mnemonic::from_entropy_in(bip39::Language::English, &entropy)
                    .map_err(|e| anyhow!("BIP39 acceptance-vector creation failed: {e}"))?,
            );
        }
        let maximum_passphrase = "x".repeat(99);
        // Keep the former double-escaped test input as an explicitly ASCII 19-byte
        // case, while testing decomposed Unicode with an actual U+0303 character.
        let ascii_escape_text = "passphrase-n\\u{303}";
        let passphrases = [
            "",
            "TREZOR",
            "passphrase-ñ",
            "passphrase-n\u{303}",
            ascii_escape_text,
            maximum_passphrase.as_str(),
        ];
        // Acceptance tests exercise representative and multi-candidate paths;
        // `batch_size` can now reflect a very large adapter limit.
        let mut batch_sizes = vec![1, self.batch_size.min(2), self.batch_size.min(256)];
        batch_sizes.sort_unstable();
        batch_sizes.dedup();

        for passphrase in passphrases {
            for &batch_size in &batch_sizes {
                let batch = (0..batch_size)
                    .map(|index| mnemonics[index % mnemonics.len()].clone())
                    .collect::<Vec<_>>();
                let expected = Zeroizing::new(
                    batch
                        .iter()
                        .map(|mnemonic| mnemonic.to_seed(passphrase))
                        .collect::<Vec<_>>(),
                );
                let gpu_seeds = Zeroizing::new(self.derive_batch(&batch, passphrase)?);
                let mismatch_index = gpu_seeds
                    .iter()
                    .zip(expected.iter())
                    .position(|(actual, expected)| actual[..] != expected[..]);
                if gpu_seeds.len() != batch_size || mismatch_index.is_some() {
                    anyhow::bail!(
                        "GPU PBKDF2 parity failed for batch size {batch_size}, passphrase byte length {}, candidate {} (returned {} seeds)",
                        passphrase.len(),
                        mismatch_index
                            .map_or_else(|| "count-check".to_string(), |index| index.to_string()),
                        gpu_seeds.len(),
                    );
                }
            }
        }

        let over_limit_passphrase = "x".repeat(100);
        if self
            .derive_batch(std::slice::from_ref(&mnemonic), &over_limit_passphrase)
            .is_ok()
        {
            anyhow::bail!("GPU PBKDF2 accepted a passphrase beyond its supported salt limit");
        }
        Ok(())
    }

    pub fn derive_batch(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        self.derive_batch_chunked(mnemonics, passphrase, None)
    }

    /// Derive a large batch in bounded GPU submissions, checking cancellation between them.
    pub fn derive_batch_cancellable(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
        cancellation: &AtomicBool,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        self.derive_batch_chunked(mnemonics, passphrase, Some(cancellation))
    }

    fn derive_batch_chunked(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
        cancellation: Option<&AtomicBool>,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        if mnemonics.is_empty() {
            return Ok(Zeroizing::new(Vec::new()));
        }
        if mnemonics.len() > self.batch_size {
            anyhow::bail!(
                "GPU batch too large: {} > {}",
                mnemonics.len(),
                self.batch_size
            );
        }

        let mut seeds = Zeroizing::new(Vec::with_capacity(mnemonics.len()));
        for batch in mnemonics.chunks(self.dispatch_batch_size) {
            if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                anyhow::bail!("GPU PBKDF2 batch cancelled");
            }
            let batch_seeds = self.derive_batch_single(batch, passphrase)?;
            seeds.extend(batch_seeds.iter().copied());
        }
        Ok(seeds)
    }

    fn derive_batch_single(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        if mnemonics.is_empty() {
            return Ok(Zeroizing::new(Vec::new()));
        }
        if mnemonics.len() > self.batch_size {
            return Err(anyhow!(
                "GPU batch too large: {} > {}",
                mnemonics.len(),
                self.batch_size
            ));
        }
        let normalized_passphrase = if passphrase.is_ascii() {
            None
        } else {
            Some(crate::derivation::normalize_bip39_passphrase(passphrase))
        };
        let passphrase = normalized_passphrase
            .as_ref()
            .map_or(passphrase, |normalized| normalized.as_str());
        let salt = Zeroizing::new(format!("mnemonic{passphrase}"));
        if salt.len() > 107 {
            anyhow::bail!("GPU PBKDF2 supports passphrases up to 99 normalized UTF-8 bytes");
        }
        let mut passwords = Zeroizing::new(vec![0u32; mnemonics.len() * 32]);
        let mut lengths = Zeroizing::new(vec![0u32; mnemonics.len()]);
        for (i, m) in mnemonics.iter().enumerate() {
            let phrase = Zeroizing::new(m.to_string());
            let bytes = phrase.as_bytes();
            if bytes.len() > 128 {
                return Err(anyhow!("BIP39 password exceeds 128 bytes"));
            }
            lengths[i] = bytes.len() as u32;
            for (j, b) in bytes.iter().enumerate() {
                passwords[i * 32 + j / 4] |= (*b as u32) << ((j % 4) * 8);
            }
        }
        let mut salt_words = Zeroizing::new(vec![0u32; 64]);
        for (j, b) in salt.bytes().enumerate() {
            salt_words[j / 4] |= (b as u32) << ((j % 4) * 8);
        }
        let mut params = Zeroizing::new(vec![0u32; 68]);
        params[0] = mnemonics.len() as u32;
        params[1] = salt.len() as u32;
        params[4..68].copy_from_slice(&salt_words);
        let error_scopes = GpuErrorScopes::push(&self.device);
        // Do not use DeviceExt::create_buffer_init here: it maps the just-created
        // buffer and panics if the backend/device has become invalid. Upload via
        // the queue instead, so wgpu's error scopes can report device loss rather
        // than unwinding through the search process.
        let password_buf = create_upload_buffer(
            &self.device,
            &self.queue,
            "pbkdf2-passwords",
            bytemuck::cast_slice(&passwords[..]),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let lengths_buf = create_upload_buffer(
            &self.device,
            &self.queue,
            "pbkdf2-lengths",
            bytemuck::cast_slice(&lengths[..]),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let params_buf = create_upload_buffer(
            &self.device,
            &self.queue,
            "pbkdf2-params",
            bytemuck::cast_slice(&params[..]),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let out_size = (mnemonics.len() * 16 * 4) as u64;
        let out_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pbkdf2-output"),
            size: out_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pbkdf2-readback"),
            size: out_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut _readback_wiper = WipeReadbackOnDrop {
            device: &self.device,
            queue: &self.queue,
            buffer: &staging,
            secret_buffers: [&password_buf, &lengths_buf, &params_buf, &out_buf],
            mapped: false,
            wiped: false,
        };
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pbkdf2-bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: password_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: lengths_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: out_buf.as_entire_binding(),
                },
            ],
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pbkdf2-encoder"),
            });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("pbkdf2"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((mnemonics.len() as u32).div_ceil(self.workgroup_size), 1, 1);
        }
        enc.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, out_size);
        // Clear mnemonic, passphrase, and seed buffers before releasing device memory.
        enc.clear_buffer(&password_buf, 0, None);
        enc.clear_buffer(&lengths_buf, 0, None);
        enc.clear_buffer(&params_buf, 0, None);
        enc.clear_buffer(&out_buf, 0, None);
        self.queue.submit(Some(enc.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        // Track a pending map so the drop guard can cancel it on an early error.
        _readback_wiper.mapped = true;
        wait_for_gpu_work(&self.device, "waiting for GPU PBKDF2")?;
        rx.recv()
            .map_err(|_| anyhow!("GPU map callback dropped"))??;
        error_scopes.finish("GPU PBKDF2 batch failed")?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("failed to read GPU output buffer: {e}"))?;
        let words: &[u32] = bytemuck::cast_slice(&data);
        let mut result = Zeroizing::new(Vec::with_capacity(mnemonics.len()));
        for i in 0..mnemonics.len() {
            let mut seed = [0u8; 64];
            for j in 0..16 {
                seed[j * 4..j * 4 + 4].copy_from_slice(&words[i * 16 + j].to_be_bytes());
            }
            result.push(seed);
        }
        drop(data);
        staging.unmap();
        _readback_wiper.mapped = false;
        _readback_wiper.schedule_readback_wipe()?;
        Ok(result)
    }
}

#[cfg(feature = "gpu")]
fn create_upload_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: contents.len() as u64,
        usage,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, contents);
    buffer
}

#[cfg(not(feature = "gpu"))]
pub struct GpuSeedEngine;

#[cfg(not(feature = "gpu"))]
impl GpuSeedEngine {
    pub fn new(_: &GpuSeedEngineConfig) -> Result<Self> {
        anyhow::bail!("GPU feature not compiled")
    }
}

#[cfg(feature = "cuda")]
pub mod cuda;

#[cfg(feature = "cuda")]
pub fn cuda_self_test() -> Result<()> {
    cuda::self_test()
}
