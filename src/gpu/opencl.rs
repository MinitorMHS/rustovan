use anyhow::{Context, Result, anyhow};
use bip39::{Language, Mnemonic};
use libloading::Library;
use std::{
    ffi::{CString, c_char, c_void},
    mem::size_of,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
use zeroize::Zeroizing;

type ClInt = i32;
type ClUint = u32;
type ClUlong = u64;
type ClBitfield = u64;
type ClPlatform = *mut c_void;
type ClDevice = *mut c_void;
type ClContext = *mut c_void;
type ClQueue = *mut c_void;
type ClProgram = *mut c_void;
type ClKernel = *mut c_void;
type ClMem = *mut c_void;
type ClEvent = *mut c_void;

type GetPlatformIds = unsafe extern "system" fn(ClUint, *mut ClPlatform, *mut ClUint) -> ClInt;
type GetPlatformInfo =
    unsafe extern "system" fn(ClPlatform, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type GetDeviceIds =
    unsafe extern "system" fn(ClPlatform, ClUlong, ClUint, *mut ClDevice, *mut ClUint) -> ClInt;
type GetDeviceInfo =
    unsafe extern "system" fn(ClDevice, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type CreateContext = unsafe extern "system" fn(
    *const isize,
    ClUint,
    *const ClDevice,
    Option<unsafe extern "system" fn(*const c_char, *const c_void, usize, *mut c_void)>,
    *mut c_void,
    *mut ClInt,
) -> ClContext;
type CreateCommandQueue =
    unsafe extern "system" fn(ClContext, ClDevice, ClBitfield, *mut ClInt) -> ClQueue;
type CreateProgramWithSource = unsafe extern "system" fn(
    ClContext,
    ClUint,
    *const *const c_char,
    *const usize,
    *mut ClInt,
) -> ClProgram;
type BuildProgram = unsafe extern "system" fn(
    ClProgram,
    ClUint,
    *const ClDevice,
    *const c_char,
    Option<unsafe extern "system" fn(ClProgram, *mut c_void)>,
    *mut c_void,
) -> ClInt;
type GetProgramBuildInfo =
    unsafe extern "system" fn(ClProgram, ClDevice, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type CreateKernel = unsafe extern "system" fn(ClProgram, *const c_char, *mut ClInt) -> ClKernel;
type CreateBuffer =
    unsafe extern "system" fn(ClContext, ClBitfield, usize, *mut c_void, *mut ClInt) -> ClMem;
type SetKernelArg = unsafe extern "system" fn(ClKernel, ClUint, usize, *const c_void) -> ClInt;
type EnqueueWriteBuffer = unsafe extern "system" fn(
    ClQueue,
    ClMem,
    ClUint,
    usize,
    usize,
    *const c_void,
    ClUint,
    *const ClEvent,
    *mut ClEvent,
) -> ClInt;
type EnqueueReadBuffer = unsafe extern "system" fn(
    ClQueue,
    ClMem,
    ClUint,
    usize,
    usize,
    *mut c_void,
    ClUint,
    *const ClEvent,
    *mut ClEvent,
) -> ClInt;
type EnqueueFillBuffer = unsafe extern "system" fn(
    ClQueue,
    ClMem,
    *const c_void,
    usize,
    usize,
    usize,
    ClUint,
    *const ClEvent,
    *mut ClEvent,
) -> ClInt;
type EnqueueNdRangeKernel = unsafe extern "system" fn(
    ClQueue,
    ClKernel,
    ClUint,
    *const usize,
    *const usize,
    *const usize,
    ClUint,
    *const ClEvent,
    *mut ClEvent,
) -> ClInt;
type GetEventInfo =
    unsafe extern "system" fn(ClEvent, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type GetEventProfilingInfo =
    unsafe extern "system" fn(ClEvent, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type Flush = unsafe extern "system" fn(ClQueue) -> ClInt;
type Finish = unsafe extern "system" fn(ClQueue) -> ClInt;
type ReleaseEvent = unsafe extern "system" fn(ClEvent) -> ClInt;
type ReleaseMemObject = unsafe extern "system" fn(ClMem) -> ClInt;
type ReleaseKernel = unsafe extern "system" fn(ClKernel) -> ClInt;
type ReleaseProgram = unsafe extern "system" fn(ClProgram) -> ClInt;
type ReleaseQueue = unsafe extern "system" fn(ClQueue) -> ClInt;
type ReleaseContext = unsafe extern "system" fn(ClContext) -> ClInt;

struct Api {
    _library: Library,
    get_platform_ids: GetPlatformIds,
    get_platform_info: GetPlatformInfo,
    get_device_ids: GetDeviceIds,
    get_device_info: GetDeviceInfo,
    create_context: CreateContext,
    create_command_queue: CreateCommandQueue,
    create_program_with_source: CreateProgramWithSource,
    build_program: BuildProgram,
    get_program_build_info: GetProgramBuildInfo,
    create_kernel: CreateKernel,
    create_buffer: CreateBuffer,
    set_kernel_arg: SetKernelArg,
    enqueue_write_buffer: EnqueueWriteBuffer,
    enqueue_read_buffer: EnqueueReadBuffer,
    enqueue_fill_buffer: EnqueueFillBuffer,
    enqueue_ndrange_kernel: EnqueueNdRangeKernel,
    get_event_info: GetEventInfo,
    get_event_profiling_info: GetEventProfilingInfo,
    flush: Flush,
    finish: Finish,
    release_event: ReleaseEvent,
    release_mem_object: ReleaseMemObject,
    release_kernel: ReleaseKernel,
    release_program: ReleaseProgram,
    release_queue: ReleaseQueue,
    release_context: ReleaseContext,
}

impl Api {
    fn load() -> Result<Self> {
        #[cfg(target_os = "windows")]
        let names: &[&str] = &["OpenCL.dll"];
        #[cfg(target_os = "macos")]
        let names: &[&str] = &["/System/Library/Frameworks/OpenCL.framework/OpenCL"];
        #[cfg(all(unix, not(target_os = "macos")))]
        let names: &[&str] = &["libOpenCL.so.1", "libOpenCL.so"];
        #[cfg(not(any(target_os = "windows", unix)))]
        let names: &[&str] = &[];

        let mut library = None;
        let mut last_error = None;
        for name in names {
            // The system OpenCL loader is an optional runtime dependency.
            match unsafe { Library::new(name) } {
                Ok(lib) => {
                    library = Some(lib);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let library = library.ok_or_else(|| {
            anyhow!(
                "OpenCL loader unavailable{}",
                last_error.map(|e| format!(": {e}")).unwrap_or_default()
            )
        })?;

        macro_rules! load {
            ($symbol:literal, $ty:ty) => {{
                // SAFETY: These names and function signatures follow the Khronos OpenCL C API.
                unsafe {
                    *library
                        .get::<$ty>(concat!($symbol, "\0").as_bytes())
                        .with_context(|| concat!("missing OpenCL symbol ", $symbol))?
                }
            }};
        }

        Ok(Self {
            get_platform_ids: load!("clGetPlatformIDs", GetPlatformIds),
            get_platform_info: load!("clGetPlatformInfo", GetPlatformInfo),
            get_device_ids: load!("clGetDeviceIDs", GetDeviceIds),
            get_device_info: load!("clGetDeviceInfo", GetDeviceInfo),
            create_context: load!("clCreateContext", CreateContext),
            create_command_queue: load!("clCreateCommandQueue", CreateCommandQueue),
            create_program_with_source: load!("clCreateProgramWithSource", CreateProgramWithSource),
            build_program: load!("clBuildProgram", BuildProgram),
            get_program_build_info: load!("clGetProgramBuildInfo", GetProgramBuildInfo),
            create_kernel: load!("clCreateKernel", CreateKernel),
            create_buffer: load!("clCreateBuffer", CreateBuffer),
            set_kernel_arg: load!("clSetKernelArg", SetKernelArg),
            enqueue_write_buffer: load!("clEnqueueWriteBuffer", EnqueueWriteBuffer),
            enqueue_read_buffer: load!("clEnqueueReadBuffer", EnqueueReadBuffer),
            enqueue_fill_buffer: load!("clEnqueueFillBuffer", EnqueueFillBuffer),
            enqueue_ndrange_kernel: load!("clEnqueueNDRangeKernel", EnqueueNdRangeKernel),
            get_event_info: load!("clGetEventInfo", GetEventInfo),
            get_event_profiling_info: load!("clGetEventProfilingInfo", GetEventProfilingInfo),
            flush: load!("clFlush", Flush),
            finish: load!("clFinish", Finish),
            release_event: load!("clReleaseEvent", ReleaseEvent),
            release_mem_object: load!("clReleaseMemObject", ReleaseMemObject),
            release_kernel: load!("clReleaseKernel", ReleaseKernel),
            release_program: load!("clReleaseProgram", ReleaseProgram),
            release_queue: load!("clReleaseCommandQueue", ReleaseQueue),
            release_context: load!("clReleaseContext", ReleaseContext),
            _library: library,
        })
    }

    fn check(code: ClInt, operation: &str) -> Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(anyhow!("{operation} failed with OpenCL error {code}"))
        }
    }

    fn platforms(&self) -> Result<Vec<ClPlatform>> {
        let mut count = 0;
        let code = unsafe { (self.get_platform_ids)(0, ptr::null_mut(), &mut count) };
        if code == -1001 {
            return Ok(Vec::new());
        }
        Self::check(code, "clGetPlatformIDs(count)")?;
        let mut platforms = vec![ptr::null_mut(); count as usize];
        Self::check(
            unsafe { (self.get_platform_ids)(count, platforms.as_mut_ptr(), &mut count) },
            "clGetPlatformIDs",
        )?;
        platforms.truncate(count as usize);
        Ok(platforms)
    }

    fn info_string(
        &self,
        call: impl Fn(usize, *mut c_void, *mut usize) -> ClInt,
        what: &str,
    ) -> Result<String> {
        let mut size = 0usize;
        Self::check(call(0, ptr::null_mut(), &mut size), what)?;
        let mut bytes = vec![0u8; size];
        Self::check(call(size, bytes.as_mut_ptr().cast(), ptr::null_mut()), what)?;
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn info_u64(&self, device: ClDevice, parameter: ClUint, what: &str) -> Result<u64> {
        let mut value = 0u64;
        Self::check(
            unsafe {
                (self.get_device_info)(
                    device,
                    parameter,
                    size_of::<u64>(),
                    (&mut value as *mut u64).cast(),
                    ptr::null_mut(),
                )
            },
            what,
        )?;
        Ok(value)
    }

    fn devices(&self, gpu_only: bool) -> Result<Vec<RawDevice>> {
        const CL_DEVICE_TYPE_ALL: ClUlong = !0;
        const CL_DEVICE_TYPE_GPU: ClUlong = 1 << 2;
        const CL_DEVICE_NAME: ClUint = 0x102B;
        const CL_DEVICE_VENDOR: ClUint = 0x102C;
        const CL_DEVICE_MAX_MEM_ALLOC_SIZE: ClUint = 0x1010;
        const CL_DEVICE_GLOBAL_MEM_SIZE: ClUint = 0x101F;
        let mut result = Vec::new();
        for platform in self.platforms()? {
            let platform_name = self.info_string(
                |size, value, returned| unsafe {
                    (self.get_platform_info)(platform, 0x0902, size, value, returned)
                },
                "clGetPlatformInfo(name)",
            )?;
            let mut count = 0;
            let kind = if gpu_only {
                CL_DEVICE_TYPE_GPU
            } else {
                CL_DEVICE_TYPE_ALL
            };
            let code =
                unsafe { (self.get_device_ids)(platform, kind, 0, ptr::null_mut(), &mut count) };
            if code == -1 {
                continue;
            }
            Self::check(code, "clGetDeviceIDs(count)")?;
            let mut devices = vec![ptr::null_mut(); count as usize];
            Self::check(
                unsafe {
                    (self.get_device_ids)(platform, kind, count, devices.as_mut_ptr(), &mut count)
                },
                "clGetDeviceIDs",
            )?;
            devices.truncate(count as usize);
            for device in devices {
                let name = self.info_string(
                    |size, value, returned| unsafe {
                        (self.get_device_info)(device, CL_DEVICE_NAME, size, value, returned)
                    },
                    "clGetDeviceInfo(name)",
                )?;
                let vendor = self.info_string(
                    |size, value, returned| unsafe {
                        (self.get_device_info)(device, CL_DEVICE_VENDOR, size, value, returned)
                    },
                    "clGetDeviceInfo(vendor)",
                )?;
                let max_allocation_size = self.info_u64(
                    device,
                    CL_DEVICE_MAX_MEM_ALLOC_SIZE,
                    "clGetDeviceInfo(max allocation size)",
                )?;
                let global_memory_size = self.info_u64(
                    device,
                    CL_DEVICE_GLOBAL_MEM_SIZE,
                    "clGetDeviceInfo(global memory size)",
                )?;
                result.push(RawDevice {
                    device,
                    platform_name: platform_name.clone(),
                    name,
                    vendor,
                    max_allocation_size,
                    global_memory_size,
                });
            }
        }
        Ok(result)
    }
}

struct RawDevice {
    device: ClDevice,
    platform_name: String,
    name: String,
    vendor: String,
    max_allocation_size: u64,
    global_memory_size: u64,
}

fn opencl_candidate_limit(max_allocation_size: u64, global_memory_size: u64) -> usize {
    // The largest per-candidate allocation is the 128-byte mnemonic buffer.
    // Across all simultaneously live buffers, each candidate needs 196 bytes.
    // OpenCL reports these limits per device, so each adapter gets its own cap.
    let by_allocation = usize::try_from(max_allocation_size / 128).unwrap_or(usize::MAX);
    let by_total_memory = if global_memory_size == 0 {
        usize::MAX
    } else {
        usize::try_from(global_memory_size / 196).unwrap_or(usize::MAX)
    };
    by_allocation
        .min(by_total_memory)
        .min(u32::MAX as usize)
        .max(1)
}

#[derive(Debug, Clone)]
pub struct OpenClDeviceInfo {
    pub name: String,
    pub vendor: String,
    pub device_type: String,
}

pub fn devices() -> Result<Vec<OpenClDeviceInfo>> {
    let api = Api::load()?;
    Ok(api
        .devices(true)?
        .into_iter()
        .map(|device| OpenClDeviceInfo {
            name: device.name,
            vendor: device.vendor,
            device_type: "GPU (OpenCL)".into(),
        })
        .collect())
}

pub fn device_count() -> Result<usize> {
    Ok(Api::load()?.devices(true)?.len())
}

pub struct OpenClSeedEngine {
    api: Api,
    context: ClContext,
    queue: ClQueue,
    program: ClProgram,
    kernel: ClKernel,
    name: String,
    batch_size: usize,
    dispatch_batch_size: usize,
    last_kernel_micros: AtomicU64,
}

// OpenCL handles are process-wide opaque objects whose API permits calls from
// any host thread. Each engine is moved to exactly one search worker after
// initialization, so its queue and kernel are never concurrently accessed.
unsafe impl Send for OpenClSeedEngine {}

impl OpenClSeedEngine {
    pub fn new(batch_size: usize) -> Result<Self> {
        Self::new_for_device(batch_size, 0)
    }

    pub fn new_for_device(batch_size: usize, device_index: usize) -> Result<Self> {
        let api = Api::load()?;
        let devices = api.devices(true)?;
        let selected = devices
            .into_iter()
            .nth(device_index)
            .ok_or_else(|| anyhow!("no OpenCL GPU device at index {device_index}"))?;
        let device_batch_limit =
            opencl_candidate_limit(selected.max_allocation_size, selected.global_memory_size);
        let batch_size = if batch_size == 0 {
            device_batch_limit
        } else {
            batch_size.min(device_batch_limit)
        };
        let mut context = ptr::null_mut();
        let mut queue = ptr::null_mut();
        let mut program = ptr::null_mut();
        let mut kernel = ptr::null_mut();

        let initialized = (|| -> Result<()> {
            let mut code = 0;
            context = unsafe {
                (api.create_context)(
                    ptr::null(),
                    1,
                    &selected.device,
                    None,
                    ptr::null_mut(),
                    &mut code,
                )
            };
            Api::check(code, "clCreateContext")?;
            if context.is_null() {
                anyhow::bail!("clCreateContext returned a null context");
            }
            queue = unsafe {
                (api.create_command_queue)(
                    context,
                    selected.device,
                    CL_QUEUE_PROFILING_ENABLE,
                    &mut code,
                )
            };
            Api::check(code, "clCreateCommandQueue")?;
            if queue.is_null() {
                anyhow::bail!("clCreateCommandQueue returned a null queue");
            }

            let source = CString::new(include_str!("../../shaders/pbkdf2_opencl.cl"))?;
            let source_ptr = source.as_ptr();
            let source_len = source.as_bytes().len();
            program = unsafe {
                (api.create_program_with_source)(context, 1, &source_ptr, &source_len, &mut code)
            };
            Api::check(code, "clCreateProgramWithSource")?;
            if program.is_null() {
                anyhow::bail!("clCreateProgramWithSource returned a null program");
            }
            let build_options = CString::new("-cl-std=CL1.2")?;
            let build_code = unsafe {
                (api.build_program)(
                    program,
                    1,
                    &selected.device,
                    build_options.as_ptr(),
                    None,
                    ptr::null_mut(),
                )
            };
            if build_code != 0 {
                let log = Self::build_log(&api, program, selected.device);
                anyhow::bail!("OpenCL PBKDF2 kernel compilation failed ({build_code}): {log}");
            }
            let kernel_name = CString::new("pbkdf2_sha512")?;
            kernel = unsafe { (api.create_kernel)(program, kernel_name.as_ptr(), &mut code) };
            Api::check(code, "clCreateKernel")?;
            if kernel.is_null() {
                anyhow::bail!("clCreateKernel returned a null kernel");
            }
            Ok(())
        })();

        if let Err(error) = initialized {
            unsafe {
                if !kernel.is_null() {
                    (api.release_kernel)(kernel);
                }
                if !program.is_null() {
                    (api.release_program)(program);
                }
                if !queue.is_null() {
                    (api.release_queue)(queue);
                }
                if !context.is_null() {
                    (api.release_context)(context);
                }
            }
            return Err(error);
        }

        eprintln!(
            "opencl_device={} platform={}",
            selected.name, selected.platform_name
        );
        Ok(Self {
            api,
            context,
            queue,
            program,
            kernel,
            name: selected.name,
            batch_size,
            dispatch_batch_size: batch_size.min(256),
            last_kernel_micros: AtomicU64::new(0),
        })
    }

    fn build_log(api: &Api, program: ClProgram, device: ClDevice) -> String {
        let mut size = 0usize;
        let code = unsafe {
            (api.get_program_build_info)(program, device, 0x1183, 0, ptr::null_mut(), &mut size)
        };
        if code != 0 || size == 0 {
            return format!("could not retrieve OpenCL build log (error {code})");
        }
        let mut bytes = vec![0u8; size];
        let code = unsafe {
            (api.get_program_build_info)(
                program,
                device,
                0x1183,
                size,
                bytes.as_mut_ptr().cast(),
                ptr::null_mut(),
            )
        };
        if code != 0 {
            return format!("could not retrieve OpenCL build log (error {code})");
        }
        String::from_utf8_lossy(&bytes)
            .trim_end_matches('\0')
            .to_string()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn max_batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn set_dispatch_batch_size(&mut self, size: usize) -> Result<()> {
        if size == 0 || size > self.batch_size {
            anyhow::bail!(
                "OpenCL dispatch size must be between 1 and {}",
                self.batch_size
            );
        }
        self.dispatch_batch_size = size;
        Ok(())
    }

    pub fn dispatch_batch_size(&self) -> usize {
        self.dispatch_batch_size
    }

    pub fn last_kernel_micros(&self) -> Option<u64> {
        let micros = self.last_kernel_micros.load(Ordering::Acquire);
        (micros > 0).then_some(micros)
    }

    pub fn self_test(&self) -> Result<()> {
        let mnemonic = Mnemonic::parse_in_normalized(
            Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .map_err(|error| anyhow!("BIP39 vector parse failed: {error}"))?;
        for passphrase in ["TREZOR", "passphrase-ñ"] {
            let actual =
                Zeroizing::new(self.derive_batch(std::slice::from_ref(&mnemonic), passphrase)?);
            let expected = Zeroizing::new(mnemonic.to_seed(passphrase));
            if actual.first().is_none_or(|seed| seed[..] != expected[..]) {
                anyhow::bail!("OpenCL PBKDF2 parity self-test failed");
            }
        }
        Ok(())
    }

    pub fn acceptance_test(&self) -> Result<()> {
        let mnemonic = Mnemonic::parse_in_normalized(
            Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .map_err(|error| anyhow!("BIP39 vector parse failed: {error}"))?;
        let batch = vec![mnemonic; self.batch_size.clamp(1, 4)];
        for passphrase in ["", "TREZOR", "passphrase-ñ"] {
            let actual = Zeroizing::new(self.derive_batch(&batch, passphrase)?);
            let expected = Zeroizing::new(
                batch
                    .iter()
                    .map(|mnemonic| mnemonic.to_seed(passphrase))
                    .collect::<Vec<_>>(),
            );
            if actual.len() != expected.len()
                || actual
                    .iter()
                    .zip(expected.iter())
                    .any(|(seed, expected)| seed[..] != expected[..])
            {
                anyhow::bail!("OpenCL PBKDF2 acceptance parity failed");
            }
        }
        Ok(())
    }

    pub fn derive_batch(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        self.derive_batch_cancellable(mnemonics, passphrase, &AtomicBool::new(false))
    }

    pub fn derive_batch_cancellable(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
        cancellation: &AtomicBool,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        if mnemonics.len() > self.batch_size {
            anyhow::bail!(
                "OpenCL batch too large: {} > {}",
                mnemonics.len(),
                self.batch_size
            );
        }
        if mnemonics.len() > self.dispatch_batch_size {
            anyhow::bail!(
                "OpenCL dispatch too large: {} > {}",
                mnemonics.len(),
                self.dispatch_batch_size
            );
        }
        if mnemonics.is_empty() {
            return Ok(Zeroizing::new(Vec::new()));
        }
        let normalized = if passphrase.is_ascii() {
            None
        } else {
            Some(Zeroizing::new(
                crate::derivation::normalize_bip39_passphrase(passphrase),
            ))
        };
        let passphrase = normalized
            .as_ref()
            .map_or(passphrase, |value| value.as_str());
        let salt = Zeroizing::new(format!("mnemonic{passphrase}"));
        if salt.len() > 107 {
            anyhow::bail!("OpenCL PBKDF2 supports normalized passphrases up to 99 bytes");
        }
        let mut passwords = Zeroizing::new(vec![0u8; mnemonics.len() * 128]);
        let mut lengths = Zeroizing::new(vec![0u32; mnemonics.len()]);
        for (index, mnemonic) in mnemonics.iter().enumerate() {
            let phrase = Zeroizing::new(mnemonic.to_string());
            let bytes = phrase.as_bytes();
            if bytes.len() > 128 {
                anyhow::bail!("BIP39 password exceeds 128 bytes");
            }
            lengths[index] = bytes.len() as u32;
            passwords[index * 128..index * 128 + bytes.len()].copy_from_slice(bytes);
        }
        let mut output = Zeroizing::new(vec![0u8; mnemonics.len() * 64]);
        let mut buffers = SecureBuffers {
            api: &self.api,
            queue: self.queue,
            entries: Vec::with_capacity(4),
            wiped: false,
        };
        let password_buf =
            buffers.create(&self.api, self.context, CL_MEM_READ_ONLY, passwords.len())?;
        let lengths_buf = buffers.create(
            &self.api,
            self.context,
            CL_MEM_READ_ONLY,
            lengths.len() * size_of::<u32>(),
        )?;
        let salt_buf = buffers.create(&self.api, self.context, CL_MEM_READ_ONLY, salt.len())?;
        let output_buf =
            buffers.create(&self.api, self.context, CL_MEM_WRITE_ONLY, output.len())?;
        write_buffer(
            &self.api,
            self.queue,
            password_buf,
            bytemuck::cast_slice(&passwords),
        )?;
        write_buffer(
            &self.api,
            self.queue,
            lengths_buf,
            bytemuck::cast_slice(&lengths),
        )?;
        write_buffer(&self.api, self.queue, salt_buf, salt.as_bytes())?;

        set_arg(&self.api, self.kernel, 0, &password_buf)?;
        set_arg(&self.api, self.kernel, 1, &lengths_buf)?;
        set_arg(&self.api, self.kernel, 2, &salt_buf)?;
        let salt_len = salt.len() as u32;
        let count = mnemonics.len() as u32;
        set_arg(&self.api, self.kernel, 3, &salt_len)?;
        set_arg(&self.api, self.kernel, 4, &count)?;
        set_arg(&self.api, self.kernel, 5, &output_buf)?;
        let global = mnemonics.len();
        let mut event = ptr::null_mut();
        Api::check(
            unsafe {
                (self.api.enqueue_ndrange_kernel)(
                    self.queue,
                    self.kernel,
                    1,
                    ptr::null(),
                    &global,
                    ptr::null(),
                    0,
                    ptr::null(),
                    &mut event,
                )
            },
            "clEnqueueNDRangeKernel",
        )?;
        Api::check(unsafe { (self.api.flush)(self.queue) }, "clFlush")?;
        let mut cancelled = false;
        loop {
            cancelled |= cancellation.load(Ordering::Acquire);
            let mut status = 1i32;
            let status_code = unsafe {
                (self.api.get_event_info)(
                    event,
                    0x11D3,
                    size_of::<i32>(),
                    (&mut status as *mut i32).cast(),
                    ptr::null_mut(),
                )
            };
            if status_code != 0 {
                unsafe {
                    (self.api.release_event)(event);
                }
                Api::check(status_code, "clGetEventInfo")?;
            }
            if status == 0 {
                break;
            }
            if status < 0 {
                unsafe {
                    (self.api.release_event)(event);
                }
                anyhow::bail!("OpenCL kernel completed with event status {status}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut kernel_start = 0u64;
        let mut kernel_end = 0u64;
        let profile_start_code = unsafe {
            (self.api.get_event_profiling_info)(
                event,
                CL_PROFILING_COMMAND_START,
                size_of::<u64>(),
                (&mut kernel_start as *mut u64).cast(),
                ptr::null_mut(),
            )
        };
        let profile_end_code = unsafe {
            (self.api.get_event_profiling_info)(
                event,
                CL_PROFILING_COMMAND_END,
                size_of::<u64>(),
                (&mut kernel_end as *mut u64).cast(),
                ptr::null_mut(),
            )
        };
        if profile_start_code == 0 && profile_end_code == 0 && kernel_end >= kernel_start {
            self.last_kernel_micros
                .store((kernel_end - kernel_start) / 1000, Ordering::Release);
        } else {
            self.last_kernel_micros.store(0, Ordering::Release);
        }
        unsafe {
            (self.api.release_event)(event);
        }
        if cancelled || cancellation.load(Ordering::Acquire) {
            anyhow::bail!("OpenCL PBKDF2 batch cancelled");
        }
        Api::check(
            unsafe {
                (self.api.enqueue_read_buffer)(
                    self.queue,
                    output_buf,
                    1,
                    0,
                    output.len(),
                    output.as_mut_ptr().cast(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                )
            },
            "clEnqueueReadBuffer",
        )?;
        let (chunks, remainder) = output.as_chunks::<64>();
        if !remainder.is_empty() {
            buffers.wipe()?;
            anyhow::bail!(
                "OpenCL PBKDF2 returned a byte count that is not divisible by the seed size"
            );
        }
        let mut seeds = Zeroizing::new(Vec::with_capacity(mnemonics.len()));
        for bytes in chunks {
            let mut seed = [0u8; 64];
            seed.copy_from_slice(bytes);
            seeds.push(seed);
        }
        buffers.wipe()?;
        drop(buffers);
        Ok(seeds)
    }
}

const CL_QUEUE_PROFILING_ENABLE: ClBitfield = 1 << 1;
const CL_PROFILING_COMMAND_START: ClUint = 0x1282;
const CL_PROFILING_COMMAND_END: ClUint = 0x1283;
const CL_MEM_READ_ONLY: ClBitfield = 1 << 2;
const CL_MEM_WRITE_ONLY: ClBitfield = 1 << 1;

struct SecureBuffers<'a> {
    api: &'a Api,
    queue: ClQueue,
    entries: Vec<(ClMem, usize)>,
    wiped: bool,
}

impl SecureBuffers<'_> {
    fn create(
        &mut self,
        api: &Api,
        context: ClContext,
        flags: ClBitfield,
        size: usize,
    ) -> Result<ClMem> {
        let mut code = 0;
        let buffer =
            unsafe { (api.create_buffer)(context, flags, size, ptr::null_mut(), &mut code) };
        Api::check(code, "clCreateBuffer")?;
        if buffer.is_null() {
            anyhow::bail!("clCreateBuffer returned a null memory object");
        }
        self.entries.push((buffer, size));
        Ok(buffer)
    }

    fn wipe(&mut self) -> Result<()> {
        if self.wiped {
            return Ok(());
        }
        let zero = 0u8;
        let mut first_error = None;
        for (buffer, size) in &self.entries {
            let code = unsafe {
                (self.api.enqueue_fill_buffer)(
                    self.queue,
                    *buffer,
                    (&zero as *const u8).cast(),
                    1,
                    0,
                    *size,
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                )
            };
            if code != 0 && first_error.is_none() {
                first_error = Some(anyhow!(
                    "clEnqueueFillBuffer(secret wipe) failed with OpenCL error {code}"
                ));
            }
        }
        let finish_code = unsafe { (self.api.finish)(self.queue) };
        if finish_code != 0 && first_error.is_none() {
            first_error = Some(anyhow!(
                "clFinish(secret wipe) failed with OpenCL error {finish_code}"
            ));
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        self.wiped = true;
        Ok(())
    }
}

impl Drop for SecureBuffers<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.wipe() {
            eprintln!("warning: {error:#}");
        }
        unsafe {
            for (buffer, _) in self.entries.drain(..) {
                let _ = (self.api.release_mem_object)(buffer);
            }
        }
    }
}

fn write_buffer(api: &Api, queue: ClQueue, buffer: ClMem, bytes: &[u8]) -> Result<()> {
    Api::check(
        unsafe {
            (api.enqueue_write_buffer)(
                queue,
                buffer,
                0,
                0,
                bytes.len(),
                bytes.as_ptr().cast(),
                0,
                ptr::null(),
                ptr::null_mut(),
            )
        },
        "clEnqueueWriteBuffer",
    )
}

fn set_arg<T>(api: &Api, kernel: ClKernel, index: ClUint, value: &T) -> Result<()> {
    Api::check(
        unsafe { (api.set_kernel_arg)(kernel, index, size_of::<T>(), (value as *const T).cast()) },
        "clSetKernelArg",
    )
}

impl Drop for OpenClSeedEngine {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.api.finish)(self.queue);
            let _ = (self.api.release_kernel)(self.kernel);
            let _ = (self.api.release_program)(self.program);
            let _ = (self.api.release_queue)(self.queue);
            let _ = (self.api.release_context)(self.context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::opencl_candidate_limit;

    #[test]
    fn opencl_candidate_limit_uses_device_allocation_and_total_memory() {
        assert_eq!(opencl_candidate_limit(128_000, 196_000), 1_000);
        assert_eq!(opencl_candidate_limit(256_000, 98_000), 500);
        assert_eq!(opencl_candidate_limit(u64::MAX, 0), u32::MAX as usize);
    }
}
