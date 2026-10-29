//! Native Windows Direct3D 11 compute backend for the PBKDF2 stage.
//!
//! WGSL is translated to shader model 5.0 HLSL with Naga so this backend runs
//! the same PBKDF2 kernel as the wgpu backends rather than maintaining a second
//! cryptographic implementation.

use anyhow::{Context, Result, anyhow};
use bip39::{Language, Mnemonic};
use std::{
    ffi::c_void,
    ptr,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::Fxc::{
                D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL0, D3DCompile,
            },
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
                D3D11_SRV_DIMENSION_BUFFEREX,
            },
            Direct3D11::{
                D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE,
                D3D11_BIND_UNORDERED_ACCESS, D3D11_BUFFER_DESC, D3D11_BUFFER_UAV,
                D3D11_BUFFER_UAV_FLAG_RAW, D3D11_BUFFEREX_SRV, D3D11_BUFFEREX_SRV_FLAG_RAW,
                D3D11_CPU_ACCESS_READ, D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_FLAG,
                D3D11_MAP_READ_WRITE, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS,
                D3D11_SDK_VERSION, D3D11_SHADER_RESOURCE_VIEW_DESC,
                D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_SUBRESOURCE_DATA,
                D3D11_UAV_DIMENSION_BUFFER, D3D11_UNORDERED_ACCESS_VIEW_DESC,
                D3D11_UNORDERED_ACCESS_VIEW_DESC_0, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
                D3D11CreateDevice, ID3D11Buffer, ID3D11ComputeShader, ID3D11Device,
                ID3D11DeviceContext, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
            },
            Dxgi::Common::DXGI_FORMAT_R32_TYPELESS,
        },
    },
    core::{PCSTR, s},
};
use zeroize::Zeroizing;

pub struct D3d11SeedEngine {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    shader: ID3D11ComputeShader,
    batch_size: usize,
    dispatch_batch_size: usize,
    feature_level: i32,
}

impl D3d11SeedEngine {
    pub fn new(batch_size: usize) -> Result<Self> {
        // D3D11 Dispatch permits at most 65,535 thread groups on each axis;
        // this shader uses 64 threads per group and one candidate per thread.
        // The ByteWidth limit on the 128-byte input buffer is larger, so the
        // dispatch-grid limit is the tighter API ceiling for this kernel.
        const D3D11_MAX_CANDIDATES: usize = 65_535 * 64;
        let device_batch_limit = D3D11_MAX_CANDIDATES;
        let batch_size = if batch_size == 0 {
            device_batch_limit
        } else {
            batch_size.min(device_batch_limit)
        };
        let (device, context, feature_level) = create_hardware_device()?;
        if feature_level < D3D_FEATURE_LEVEL_11_0.0 {
            anyhow::bail!(
                "Direct3D 11 compute requires feature level 11_0 or newer; detected 0x{feature_level:04x}"
            );
        }
        let shader_bytecode = compiled_shader_bytecode()?;
        let mut shader = None;
        unsafe {
            device
                .CreateComputeShader(&shader_bytecode, None, Some(&mut shader))
                .context("D3D11 rejected the compiled PBKDF2 compute shader")?;
        }
        let shader = shader.context("Direct3D 11 returned no compute shader")?;
        Ok(Self {
            device,
            context,
            shader,
            batch_size,
            dispatch_batch_size: batch_size.min(256),
            feature_level,
        })
    }

    pub fn device_name(&self) -> String {
        format!(
            "Direct3D 11 hardware, feature level 0x{:04x}",
            self.feature_level
        )
    }

    pub fn dispatch_batch_size(&self) -> usize {
        self.dispatch_batch_size
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

    pub fn self_test(&self) -> Result<()> {
        let mnemonic = Mnemonic::parse_in_normalized(
            Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .context("BIP39 vector parse failed")?;
        for passphrase in ["", "TREZOR", "passphrase-ñ"] {
            let actual = self.derive_batch(std::slice::from_ref(&mnemonic), passphrase)?;
            let expected = Zeroizing::new(mnemonic.to_seed(passphrase));
            if actual.first().is_none_or(|seed| seed[..] != expected[..]) {
                anyhow::bail!("Direct3D 11 PBKDF2 self-test differs from the CPU reference");
            }
        }
        Ok(())
    }

    pub fn acceptance_test(&self) -> Result<()> {
        let base = Mnemonic::parse_in_normalized(
            Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .context("BIP39 vector parse failed")?;
        let mnemonics = [
            base,
            Mnemonic::from_entropy_in(Language::English, &[1u8; 16])?,
            Mnemonic::from_entropy_in(Language::English, &[0x5au8; 16])?,
        ];
        let mut sizes = vec![1, self.batch_size.min(2), self.batch_size.min(256)];
        sizes.sort_unstable();
        sizes.dedup();
        for passphrase in ["", "TREZOR", "passphrase-ñ", "passphrase-n\u{303}"] {
            for size in &sizes {
                let batch = (0..*size)
                    .map(|index| mnemonics[index % mnemonics.len()].clone())
                    .collect::<Vec<_>>();
                let actual = self.derive_batch(&batch, passphrase)?;
                if actual.len() != batch.len()
                    || actual.iter().zip(batch.iter()).any(|(seed, mnemonic)| {
                        let expected = Zeroizing::new(mnemonic.to_seed(passphrase));
                        seed[..] != expected[..]
                    })
                {
                    anyhow::bail!(
                        "Direct3D 11 PBKDF2 parity failed for batch size {size} and passphrase byte length {}",
                        passphrase.len()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn derive_batch(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        self.derive_batch_cancellable(mnemonics, passphrase, None)
    }

    pub fn derive_batch_cancellable(
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
        let normalized = if passphrase.is_ascii() {
            None
        } else {
            Some(crate::derivation::normalize_bip39_passphrase(passphrase))
        };
        let passphrase = normalized
            .as_deref()
            .map_or(passphrase, |normalized| normalized.as_str());
        if passphrase.len() > 99 {
            anyhow::bail!(
                "Direct3D 11 PBKDF2 supports passphrases up to 99 normalized UTF-8 bytes"
            );
        }
        let mut result = Zeroizing::new(Vec::with_capacity(mnemonics.len()));
        for batch in mnemonics.chunks(self.dispatch_batch_size) {
            if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                anyhow::bail!("Direct3D 11 PBKDF2 batch cancelled");
            }
            let seeds = self.derive_batch_single(batch, passphrase)?;
            result.extend(seeds.iter().copied());
        }
        Ok(result)
    }

    fn derive_batch_single(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        let mut passwords = Zeroizing::new(vec![0u32; mnemonics.len() * 32]);
        let mut lengths = Zeroizing::new(vec![0u32; mnemonics.len()]);
        for (index, mnemonic) in mnemonics.iter().enumerate() {
            let phrase = Zeroizing::new(mnemonic.to_string());
            let bytes = phrase.as_bytes();
            if bytes.len() > 128 {
                anyhow::bail!("BIP39 password exceeds 128 bytes");
            }
            lengths[index] = bytes.len() as u32;
            for (offset, byte) in bytes.iter().enumerate() {
                passwords[index * 32 + offset / 4] |= (*byte as u32) << ((offset % 4) * 8);
            }
        }
        let salt = Zeroizing::new(format!("mnemonic{passphrase}"));
        let mut salt_words = Zeroizing::new(vec![0u32; 64]);
        for (offset, byte) in salt.bytes().enumerate() {
            salt_words[offset / 4] |= (byte as u32) << ((offset % 4) * 8);
        }
        let mut params = Zeroizing::new(vec![0u32; 68]);
        params[0] = mnemonics.len() as u32;
        params[1] = salt.len() as u32;
        params[4..68].copy_from_slice(&salt_words);

        let password_bytes = bytemuck::cast_slice(&passwords[..]);
        let length_bytes = bytemuck::cast_slice(&lengths[..]);
        let params_bytes = bytemuck::cast_slice(&params[..]);
        let output_bytes = mnemonics.len() * 16 * size_of::<u32>();
        let output_buffer = create_empty_buffer(
            &self.device,
            output_bytes,
            D3D11_BIND_UNORDERED_ACCESS.0 as u32,
            D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32,
            0,
        )?;
        // CPU-mappable staging buffers must not carry D3D11 misc/raw-view flags.
        let readback_buffer = create_empty_buffer(&self.device, output_bytes, 0, 0, 0)?;
        let mut wipe_guard = D3d11WipeOnDrop::new(
            &self.context,
            &output_buffer,
            &readback_buffer,
            output_bytes,
        );
        let password_buffer = create_buffer(
            &self.device,
            password_bytes,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
            D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32,
            0,
        )?;
        wipe_guard.track(&password_buffer, passwords.len());
        let length_buffer = create_buffer(
            &self.device,
            length_bytes,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
            D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32,
            0,
        )?;
        wipe_guard.track(&length_buffer, lengths.len());
        let params_buffer = create_buffer(
            &self.device,
            params_bytes,
            D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            0,
            0,
        )?;
        wipe_guard.track(&params_buffer, params.len());
        let mut password_view = None;
        let mut length_view = None;
        let mut output_view = None;
        let password_desc = raw_srv_desc(password_bytes.len())?;
        let length_desc = raw_srv_desc(length_bytes.len())?;
        let output_desc = raw_uav_desc(output_bytes)?;
        unsafe {
            self.device.CreateShaderResourceView(
                &password_buffer,
                Some(&password_desc),
                Some(&mut password_view),
            )?;
            self.device.CreateShaderResourceView(
                &length_buffer,
                Some(&length_desc),
                Some(&mut length_view),
            )?;
            self.device.CreateUnorderedAccessView(
                &output_buffer,
                Some(&output_desc),
                Some(&mut output_view),
            )?;
        }
        let password_view = password_view.context("Direct3D 11 returned no password SRV")?;
        let length_view = length_view.context("Direct3D 11 returned no length SRV")?;
        let output_view = output_view.context("Direct3D 11 returned no output UAV")?;

        let result = self.dispatch_and_read(
            mnemonics.len(),
            output_bytes,
            &params_buffer,
            &output_buffer,
            &readback_buffer,
            &password_view,
            &length_view,
            &output_view,
        );
        let wipe = wipe_guard.wipe();
        match (result, wipe) {
            (Ok(seeds), Ok(())) => Ok(seeds),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.context("Direct3D 11 secret-buffer wipe failed")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_and_read(
        &self,
        count: usize,
        output_bytes: usize,
        params_buffer: &ID3D11Buffer,
        output_buffer: &ID3D11Buffer,
        readback_buffer: &ID3D11Buffer,
        password_view: &ID3D11ShaderResourceView,
        length_view: &ID3D11ShaderResourceView,
        output_view: &ID3D11UnorderedAccessView,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        unsafe {
            let resources = [Some(password_view.clone()), Some(length_view.clone())];
            let constants = [Some(params_buffer.clone())];
            let uavs = [Some(output_view.clone())];
            self.context.CSSetShader(Some(&self.shader), None);
            self.context.CSSetShaderResources(0, Some(&resources));
            self.context.CSSetConstantBuffers(2, Some(&constants));
            self.context
                .CSSetUnorderedAccessViews(3, 1, Some(uavs.as_ptr()), None);
            self.context.Dispatch((count as u32).div_ceil(64), 1, 1);
            self.context.CSSetShaderResources(0, Some(&[None, None]));
            self.context.CSSetConstantBuffers(2, Some(&[None]));
            self.context
                .CSSetUnorderedAccessViews(3, 1, Some([None].as_ptr()), None);
            self.context.CopyResource(readback_buffer, output_buffer);

            let mut mapped = Default::default();
            self.context.Map(
                readback_buffer,
                0,
                D3D11_MAP_READ_WRITE,
                0,
                Some(&mut mapped),
            )?;
            if mapped.pData.is_null() {
                self.context.Unmap(readback_buffer, 0);
                anyhow::bail!("Direct3D 11 returned a null PBKDF2 readback pointer");
            }
            let words = std::slice::from_raw_parts(mapped.pData.cast::<u32>(), count * 16);
            let mut result = Zeroizing::new(Vec::with_capacity(count));
            for index in 0..count {
                let mut seed = [0u8; 64];
                for word in 0..16 {
                    seed[word * 4..word * 4 + 4]
                        .copy_from_slice(&words[index * 16 + word].to_be_bytes());
                }
                result.push(seed);
            }
            ptr::write_bytes(mapped.pData, 0, output_bytes);
            self.context.Unmap(readback_buffer, 0);
            Ok(result)
        }
    }
}

struct D3d11WipeOnDrop<'a> {
    context: &'a ID3D11DeviceContext,
    inputs: Vec<(ID3D11Buffer, usize)>,
    output: &'a ID3D11Buffer,
    readback: &'a ID3D11Buffer,
    output_bytes: usize,
    wiped: bool,
}

impl<'a> D3d11WipeOnDrop<'a> {
    fn new(
        context: &'a ID3D11DeviceContext,
        output: &'a ID3D11Buffer,
        readback: &'a ID3D11Buffer,
        output_bytes: usize,
    ) -> Self {
        Self {
            context,
            inputs: Vec::new(),
            output,
            readback,
            output_bytes,
            wiped: false,
        }
    }

    fn track(&mut self, buffer: &ID3D11Buffer, words: usize) {
        self.inputs.push((buffer.clone(), words));
    }

    fn wipe(&mut self) -> Result<()> {
        if self.wiped {
            return Ok(());
        }
        let mut zero_inputs = Vec::with_capacity(self.inputs.len());
        for (buffer, word_count) in &self.inputs {
            let zeros = Zeroizing::new(vec![0u32; *word_count]);
            unsafe {
                self.context.UpdateSubresource(
                    buffer,
                    0,
                    None,
                    zeros.as_ptr().cast::<c_void>(),
                    0,
                    0,
                );
            }
            zero_inputs.push(zeros);
        }
        let zero_output = Zeroizing::new(vec![0u32; self.output_bytes / size_of::<u32>()]);
        unsafe {
            self.context.UpdateSubresource(
                self.output,
                0,
                None,
                zero_output.as_ptr().cast::<c_void>(),
                0,
                0,
            );
            self.context.CopyResource(self.readback, self.output);
            let mut mapped = Default::default();
            self.context
                .Map(self.readback, 0, D3D11_MAP_READ_WRITE, 0, Some(&mut mapped))?;
            if mapped.pData.is_null() {
                self.context.Unmap(self.readback, 0);
                anyhow::bail!("Direct3D 11 returned a null wipe synchronization pointer");
            }
            ptr::write_bytes(mapped.pData, 0, self.output_bytes);
            self.context.Unmap(self.readback, 0);
            self.context.Flush();
        }
        drop(zero_inputs);
        self.wiped = true;
        Ok(())
    }
}

impl Drop for D3d11WipeOnDrop<'_> {
    fn drop(&mut self) {
        if !self.wiped
            && let Err(error) = self.wipe()
        {
            eprintln!("warning: Direct3D 11 GPU buffer wipe failed: {error:#}");
        }
    }
}

pub fn device_info() -> Result<(String, String)> {
    let (_, _, feature_level) = create_hardware_device()?;
    Ok((
        "Direct3D 11 hardware adapter".to_string(),
        format!("0x{feature_level:04x}"),
    ))
}

fn raw_srv_desc(byte_size: usize) -> Result<D3D11_SHADER_RESOURCE_VIEW_DESC> {
    let elements = u32::try_from(byte_size / size_of::<u32>())
        .context("D3D11 raw shader-resource view exceeds its element limit")?;
    if elements == 0 || byte_size % size_of::<u32>() != 0 {
        anyhow::bail!(
            "D3D11 raw shader-resource buffer size must be a nonzero multiple of 4 bytes"
        );
    }
    Ok(D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R32_TYPELESS,
        ViewDimension: D3D11_SRV_DIMENSION_BUFFEREX,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            BufferEx: D3D11_BUFFEREX_SRV {
                FirstElement: 0,
                NumElements: elements,
                Flags: D3D11_BUFFEREX_SRV_FLAG_RAW.0 as u32,
            },
        },
    })
}

fn raw_uav_desc(byte_size: usize) -> Result<D3D11_UNORDERED_ACCESS_VIEW_DESC> {
    let elements = u32::try_from(byte_size / size_of::<u32>())
        .context("D3D11 raw unordered-access view exceeds its element limit")?;
    if elements == 0 || byte_size % size_of::<u32>() != 0 {
        anyhow::bail!(
            "D3D11 raw unordered-access buffer size must be a nonzero multiple of 4 bytes"
        );
    }
    Ok(D3D11_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_R32_TYPELESS,
        ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
        Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
            Buffer: D3D11_BUFFER_UAV {
                FirstElement: 0,
                NumElements: elements,
                Flags: D3D11_BUFFER_UAV_FLAG_RAW.0 as u32,
            },
        },
    })
}

fn create_hardware_device() -> Result<(ID3D11Device, ID3D11DeviceContext, i32)> {
    let levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device = None;
    let mut context = None;
    let mut feature_level = D3D_FEATURE_LEVEL(0);
    unsafe {
        D3D11CreateDevice(
            None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )
        .context("D3D11CreateDevice could not open a hardware compute adapter")?;
    }
    Ok((
        device.context("D3D11CreateDevice returned no device")?,
        context.context("D3D11CreateDevice returned no immediate context")?,
        feature_level.0,
    ))
}

fn wgsl_to_hlsl() -> Result<String> {
    let source = include_str!("../../shaders/pbkdf2.wgsl");
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| anyhow!("PBKDF2 WGSL parse failed: {error}"))?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|error| anyhow!("PBKDF2 WGSL validation failed: {error}"))?;
    let mut options = naga::back::hlsl::Options::default();
    options.shader_model = naga::back::hlsl::ShaderModel::V5_0;
    options.fake_missing_bindings = false;
    for (_, variable) in module.global_variables.iter() {
        if let Some(binding) = &variable.binding {
            options.binding_map.insert(
                *binding,
                naga::back::hlsl::BindTarget {
                    space: 0,
                    register: binding.binding,
                    binding_array_size: None,
                    dynamic_storage_buffer_offsets_index: None,
                    restrict_indexing: true,
                },
            );
        }
    }
    let pipeline = naga::back::hlsl::PipelineOptions {
        entry_point: Some((naga::ShaderStage::Compute, "main".to_string())),
    };
    let mut hlsl = String::new();
    let mut writer = naga::back::hlsl::Writer::new(&mut hlsl, &options, &pipeline);
    writer
        .write(&module, &info, None)
        .map_err(|error| anyhow!("WGSL-to-HLSL translation failed: {error}"))?;
    drop(writer);
    Ok(hlsl)
}

fn compiled_shader_bytecode() -> Result<&'static [u8]> {
    static BYTECODE: OnceLock<std::result::Result<Vec<u8>, String>> = OnceLock::new();
    match BYTECODE.get_or_init(|| {
        wgsl_to_hlsl()
            .and_then(|source| compile_compute_shader(&source))
            .map_err(|error| format!("{error:#}"))
    }) {
        Ok(bytecode) => Ok(bytecode),
        Err(error) => anyhow::bail!("Direct3D 11 PBKDF2 shader compilation failed: {error}"),
    }
}
fn compile_compute_shader(source: &str) -> Result<Vec<u8>> {
    let mut bytecode = None;
    let mut diagnostics = None;
    // Level 0 avoids D3DCompiler's pathological optimization cost on this large
    // iterative PBKDF2 shader, especially on older integrated GPUs.
    let flags = D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL0;
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast::<c_void>(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            s!("main"),
            s!("cs_5_0"),
            flags,
            0,
            &mut bytecode,
            Some(&mut diagnostics),
        )
    };
    if let Err(error) = result {
        let details = diagnostics
            .and_then(|blob| unsafe {
                let pointer = blob.GetBufferPointer();
                (!pointer.is_null()).then(|| {
                    std::ffi::CStr::from_ptr(pointer.cast::<i8>())
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .unwrap_or_default();
        return Err(anyhow!(
            "Direct3D 11 HLSL compilation failed: {error}; {details}"
        ));
    }
    let bytecode = bytecode.context("D3DCompile returned no compute shader bytecode")?;
    let pointer = unsafe { bytecode.GetBufferPointer() };
    let length = unsafe { bytecode.GetBufferSize() };
    if pointer.is_null() || length == 0 {
        anyhow::bail!("D3DCompile returned empty compute shader bytecode");
    }
    Ok(unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) }.to_vec())
}

fn create_buffer(
    device: &ID3D11Device,
    data: &[u8],
    bind_flags: u32,
    misc_flags: u32,
    structure_stride: u32,
) -> Result<ID3D11Buffer> {
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr().cast::<c_void>(),
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    };
    create_buffer_inner(
        device,
        data.len(),
        D3D11_USAGE_DEFAULT,
        bind_flags,
        0,
        misc_flags,
        structure_stride,
        Some(&initial),
    )
}

fn create_empty_buffer(
    device: &ID3D11Device,
    byte_width: usize,
    bind_flags: u32,
    misc_flags: u32,
    structure_stride: u32,
) -> Result<ID3D11Buffer> {
    create_buffer_inner(
        device,
        byte_width,
        if bind_flags == 0 {
            D3D11_USAGE_STAGING
        } else {
            D3D11_USAGE_DEFAULT
        },
        bind_flags,
        if bind_flags == 0 {
            (D3D11_CPU_ACCESS_READ.0 | D3D11_CPU_ACCESS_WRITE.0) as u32
        } else {
            0
        },
        misc_flags,
        structure_stride,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_buffer_inner(
    device: &ID3D11Device,
    byte_width: usize,
    usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE,
    bind_flags: u32,
    cpu_access_flags: u32,
    misc_flags: u32,
    structure_stride: u32,
    initial: Option<&D3D11_SUBRESOURCE_DATA>,
) -> Result<ID3D11Buffer> {
    let byte_width = u32::try_from(byte_width).context("D3D11 buffer exceeds 4 GiB")?;
    if byte_width == 0 {
        anyhow::bail!("D3D11 buffer cannot be empty");
    }
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: byte_width,
        Usage: usage,
        BindFlags: bind_flags,
        CPUAccessFlags: cpu_access_flags,
        MiscFlags: misc_flags,
        StructureByteStride: structure_stride,
    };
    let mut buffer = None;
    unsafe {
        device.CreateBuffer(
            &desc,
            initial.map(|data| data as *const _),
            Some(&mut buffer),
        )?;
    }
    buffer.context("Direct3D 11 returned no buffer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgsl_translates_to_shader_model_5_bindings() {
        let hlsl = wgsl_to_hlsl().expect("PBKDF2 kernel translates for D3D11");
        assert!(hlsl.contains("register(t0"));
        assert!(hlsl.contains("register(t1"));
        assert!(hlsl.contains("register(b2"));
        assert!(hlsl.contains("register(u3"));
        assert!(hlsl.contains("numthreads(64, 1, 1)"));
    }
}
