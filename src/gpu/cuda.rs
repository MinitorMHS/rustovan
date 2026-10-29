#![cfg(feature = "cuda")]

use super::GpuBackend;
use anyhow::{Result, anyhow};
use bip39::Mnemonic;
use zeroize::Zeroizing;

unsafe extern "C" {
    fn ton_cuda_device_count() -> i32;
    fn ton_cuda_max_batch_size(out: *mut u64) -> i32;
    fn ton_cuda_pbkdf2(
        passwords: *const u32,
        lengths: *const u32,
        count: u32,
        salt: *const u8,
        salt_len: u32,
        out: *mut u8,
    ) -> i32;
}

pub fn device_count() -> i32 {
    // SAFETY: This function takes no pointers and the CUDA wrapper returns a status/count.
    unsafe { ton_cuda_device_count() }
}

pub fn max_batch_size() -> Result<usize> {
    let mut maximum = 0u64;
    // SAFETY: `maximum` is a valid output pointer for the CUDA wrapper.
    let status = unsafe { ton_cuda_max_batch_size(&mut maximum) };
    if status != 0 {
        anyhow::bail!("CUDA could not determine a safe batch limit (error {status})");
    }
    let maximum = usize::try_from(maximum).unwrap_or(usize::MAX);
    if maximum == 0 {
        anyhow::bail!("CUDA device has no available memory for a PBKDF2 candidate");
    }
    Ok(maximum)
}

pub fn derive_batch(mnemonics: &[Mnemonic], passphrase: &str) -> Result<Zeroizing<Vec<[u8; 64]>>> {
    if mnemonics.is_empty() {
        return Ok(Zeroizing::new(Vec::new()));
    }
    let maximum = max_batch_size()?;
    if mnemonics.len() > maximum {
        anyhow::bail!(
            "CUDA batch exceeds the current free-memory-derived limit of {} candidates",
            maximum
        );
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
        anyhow::bail!("CUDA PBKDF2 supports passphrases up to 99 normalized UTF-8 bytes");
    }

    // 32 u32s/candidate = 128 bytes, enough for a 12-word English mnemonic.
    let mut passwords = Zeroizing::new(vec![0u32; mnemonics.len() * 32]);
    let mut lengths = Zeroizing::new(vec![0u32; mnemonics.len()]);
    for (i, mnemonic) in mnemonics.iter().enumerate() {
        let phrase = Zeroizing::new(mnemonic.to_string());
        let bytes = phrase.as_bytes();
        if bytes.len() > 128 {
            anyhow::bail!("mnemonic exceeds CUDA password buffer");
        }
        lengths[i] = bytes.len() as u32;
        for (j, b) in bytes.iter().enumerate() {
            passwords[i * 32 + j / 4] |= (*b as u32) << ((j % 4) * 8);
        }
    }

    let count =
        u32::try_from(mnemonics.len()).map_err(|_| anyhow!("CUDA batch count is too large"))?;
    let salt_len =
        u32::try_from(salt.len()).map_err(|_| anyhow!("CUDA salt length is too large"))?;
    let mut output = Zeroizing::new(vec![0u8; mnemonics.len() * 64]);
    // SAFETY: All pointers reference live, correctly sized buffers for this call.
    // The batch and salt lengths are bounded, and the CUDA wrapper checks runtime errors.
    let rc = unsafe {
        ton_cuda_pbkdf2(
            passwords.as_ptr(),
            lengths.as_ptr(),
            count,
            salt.as_ptr(),
            salt_len,
            output.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(anyhow!("CUDA PBKDF2 failed with CUDA error code {rc}"));
    }

    Ok(Zeroizing::new(
        output
            .chunks_exact(64)
            .map(|chunk| {
                let mut seed = [0u8; 64];
                seed.copy_from_slice(chunk);
                seed
            })
            .collect(),
    ))
}

pub fn self_test() -> Result<()> {
    if device_count() <= 0 {
        anyhow::bail!("no CUDA device available");
    }
    let mnemonic = Mnemonic::parse_in_normalized(
        bip39::Language::English,
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
    )?;
    for passphrase in ["TREZOR", "passphrase-ñ"] {
        let got_seeds = derive_batch(std::slice::from_ref(&mnemonic), passphrase)?;
        let got = Zeroizing::new(got_seeds[0]);
        let expected = Zeroizing::new(mnemonic.to_seed(passphrase));
        if got[..] != expected[..] {
            anyhow::bail!("CUDA PBKDF2 known-answer test failed");
        }
    }
    Ok(())
}

pub fn backend() -> GpuBackend {
    GpuBackend::Cuda
}
