#[cfg(feature = "cuda")]
unsafe extern "C" {
    pub fn ton_cuda_device_count() -> i32;
    pub fn ton_cuda_pbkdf2(passwords: *const u32, lengths: *const u32, count: u32, salt: *const u8, salt_len: u32, out: *mut u8) -> i32;
}
