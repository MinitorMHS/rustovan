use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rand::TryRngCore;
use std::{
    collections::VecDeque,
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ton_vanity_rust::{
    gpu::{GpuBackend, capability_report, devices, search_backend_name},
    matcher::{MatchMode, Matcher, normalize_friendly_prefixes},
    search::{
        FoundWallet, SearchConfig, SearchProgress, expected_search_candidates,
        format_duration_dhms, search_auto, search_cpu, search_gpu, search_hybrid,
    },
    wallet::{Network, WalletVersion},
};
use zeroize::Zeroizing;

#[derive(Parser, Clone)]
#[command(name = "rustovan", version, about = "TON vanity wallet generator")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(
        long,
        global = true,
        default_value = "v5r1",
        help = "Wallet contract: v1r1-v5r1/w5 or wallettg"
    )]
    wallet: String,
    #[arg(
        long,
        global = true,
        default_value = "auto",
        help = "Auto benchmarks CPU-only, GPU-only, and concurrent CPU+GPU routes and selects the fastest measured; Hybrid benchmarks and runs the fastest concurrent CPU+GPU configuration"
    )]
    backend: String,
    #[arg(long, global = true, allow_hyphen_values = true)]
    #[arg(
        help = "Address prefix to match; bare text is placed after the selected TON header; repeat for multiple criteria"
    )]
    prefix: Vec<String>,
    #[arg(long, global = true, allow_hyphen_values = true)]
    #[arg(help = "Address suffix to match; repeat for multiple criteria")]
    suffix: Vec<String>,
    #[arg(long, global = true)]
    #[arg(help = "Exact 48-character TON friendly address; repeat for multiple criteria")]
    exact: Vec<String>,
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "all",
        help = "How supplied match criteria combine: all requires every criterion; any stops at the first criterion that matches"
    )]
    match_mode: MatchMode,
    #[arg(
        long,
        global = true,
        help = "Case-sensitive prefix/suffix matching; exact addresses are always byte-exact"
    )]
    case_sensitive: bool,
    #[arg(long, global = true)]
    bounceable: bool,
    #[arg(long, global = true)]
    testnet: bool,
    #[arg(
        long,
        global = true,
        default_value_t = 0,
        help = "CPU search workers (0 = all available logical threads); Hybrid keeps this full CPU pool and independently tunes CPU workers that process GPU results"
    )]
    threads: usize,
    #[arg(
        long,
        global = true,
        default_value_t = 1,
        value_name = "COUNT",
        help = "Maximum number of compatible physical GPU devices to use in parallel (Vulkan, Direct3D 12, Metal, OpenGL/GLES, and OpenCL)"
    )]
    gpu_devices: usize,
    #[arg(long, global = true, default_value_t = 0)]
    account: u32,
    #[arg(
        long,
        global = true,
        help = "Prompt for a hidden BIP39 passphrase instead of putting it in command history or process arguments"
    )]
    prompt_passphrase: bool,
    #[arg(
        long,
        global = true,
        default_value_t = 0,
        help = "GPU dispatch candidate cap (default 0 = derive from device limits); dispatch is calibrated per device and targets calls below 1.5 seconds"
    )]
    gpu_batch: usize,
    #[arg(
        long,
        global = true,
        value_name = "COUNT",
        help = "Stop after this many generated candidates; required for --exact"
    )]
    max_candidates: Option<u64>,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Create a new file containing the mnemonic and private key; existing files are never overwritten"
    )]
    output: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Print the mnemonic and private key to stdout (exposes secrets in terminal and logs)"
    )]
    show_secrets: bool,
    #[arg(long, hide = true)]
    daemon_worker: bool,
    #[arg(long, hide = true)]
    daemon_state_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    daemon_passphrase_stdin: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    ListWallets,
    Devices,
    Capabilities,
    #[command(
        about = "Benchmark GPU backends (or the selected --backend) and dispatch sizes, then report the fastest route"
    )]
    Benchmark,
    #[command(about = "Show available logical CPU threads and the default search worker count")]
    Threads,
    GpuSelfTest {
        #[arg(
            long,
            default_value = "auto",
            help = "Backend used for the GPU PBKDF2 self-test"
        )]
        backend: String,
    },
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand, Clone, Copy, Debug)]
enum DaemonAction {
    Start,
    Status,
    Stop,
}

#[cfg(unix)]
fn create_secret_file(path: &Path) -> std::io::Result<File> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(windows)]
fn path_to_wide(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if path_wide[..path_wide.len() - 1].contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "secret output path contains a NUL",
        ));
    }
    Ok(path_wide)
}
#[cfg(windows)]
fn create_secret_file(path: &Path) -> std::io::Result<File> {
    use std::{os::windows::io::FromRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{INVALID_HANDLE_VALUE, LocalFree},
        Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        },
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE},
    };

    let path_wide = path_to_wide(path)?;

    let sddl = "D:P(A;;FA;;;OW)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor = ptr::null_mut();
    let mut descriptor_len = 0;
    // SAFETY: The SDDL is a valid NUL-terminated UTF-16 string and the output pointers are writable.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            &mut descriptor_len,
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: All pointers are valid for this call; CREATE_NEW prevents following or replacing an existing file.
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_WRITE,
            FILE_SHARE_NONE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    let create_error = if handle == INVALID_HANDLE_VALUE {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: The security descriptor was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(descriptor) };
    if let Some(error) = create_error {
        return Err(error);
    }
    // SAFETY: CreateFileW returned a valid, uniquely owned handle that is now transferred to File.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(not(any(unix, windows)))]
fn create_secret_file(path: &Path) -> std::io::Result<File> {
    use std::fs::OpenOptions;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).open(path)
}
struct SecretTempFile(PathBuf);

impl Drop for SecretTempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_secret_temp_file(target: &Path) -> std::io::Result<(SecretTempFile, File)> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if target.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "secret output path must name a file",
        ));
    }

    for _ in 0..32 {
        let mut random = [0u8; 16];
        rand::rng().try_fill_bytes(&mut random).map_err(|error| {
            std::io::Error::other(format!("secure random source failed: {error}"))
        })?;
        let mut temp_name = std::ffi::OsString::from(".ton-vanity-");
        temp_name.push(format!("{}.tmp", hex::encode(random)));
        let temp_path = parent.join(temp_name);
        match create_secret_file(&temp_path) {
            Ok(file) => return Ok((SecretTempFile(temp_path), file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary secret file",
    ))
}
#[cfg(windows)]
fn commit_secret_file(temp_path: &Path, target: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::MoveFileW;

    let temp_wide = path_to_wide(temp_path)?;
    let target_wide = path_to_wide(target)?;
    // SAFETY: Both arguments are valid, NUL-terminated UTF-16 paths. MoveFileW
    // does not replace an existing destination and both paths share a directory.
    let moved = unsafe { MoveFileW(temp_wide.as_ptr(), target_wide.as_ptr()) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "android")]
fn commit_secret_file(temp_path: &Path, target: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let temp_path = CString::new(temp_path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "temporary secret-file path contains a NUL",
        )
    })?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "secret output path contains a NUL",
        )
    })?;

    // Android may deny hard links even between files owned by the same app.
    // renameat2 with RENAME_NOREPLACE keeps the same atomic, no-overwrite
    // guarantee without relying on hard-link permission.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            temp_path.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_uint,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(not(windows), not(target_os = "android")))]
fn commit_secret_file(temp_path: &Path, target: &Path) -> std::io::Result<()> {
    // A same-directory hard link is an atomic no-replace commit. If the target
    // exists, the operation fails without changing it.
    fs::hard_link(temp_path, target)
}

fn serialize_wallet_secrets(found: &[(Option<String>, FoundWallet)]) -> Result<Zeroizing<String>> {
    use std::fmt::Write as FmtWrite;
    let mut contents = Zeroizing::new(String::new());
    if found.len() == 1 {
        let (criterion, wallet) = &found[0];
        if let Some(criterion) = criterion {
            writeln!(contents, "matched_criterion={criterion}")?;
        }
        writeln!(contents, "address={}", wallet.address)?;
        writeln!(contents, "mnemonic={}", wallet.mnemonic)?;
        writeln!(contents, "private_key={}", wallet.private_key_hex)?;
    } else {
        for (index, (criterion, wallet)) in found.iter().enumerate() {
            writeln!(contents, "wallet_result={}", index + 1)?;
            if let Some(criterion) = criterion {
                writeln!(contents, "matched_criterion={criterion}")?;
            }
            writeln!(contents, "address={}", wallet.address)?;
            writeln!(contents, "mnemonic={}", wallet.mnemonic)?;
            writeln!(contents, "private_key={}", wallet.private_key_hex)?;
        }
    }
    Ok(contents)
}

fn write_protected_file(path: &Path, contents: &[u8]) -> Result<()> {
    let (temp, mut file) = create_secret_temp_file(path).with_context(|| {
        format!(
            "could not create a protected temporary output file beside {}",
            path.display()
        )
    })?;
    if let Err(error) = file.write_all(contents).and_then(|_| file.sync_all()) {
        drop(file);
        return Err(error).with_context(|| {
            format!(
                "could not finish writing temporary output file for {}",
                path.display()
            )
        });
    }
    drop(file);

    commit_secret_file(&temp.0, path).with_context(|| {
        format!(
            "could not commit output file {} (it may already exist)",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(feature = "sealed-output")]
enum EmbeddedAgeRecipient {
    X25519(age::x25519::Recipient),
    TaggedPq(age::tagpq::Recipient),
}

#[cfg(feature = "sealed-output")]
impl EmbeddedAgeRecipient {
    fn kind(&self) -> &'static str {
        match self {
            Self::X25519(_) => "X25519",
            Self::TaggedPq(_) => "tagged-ML-KEM-768+P-256",
        }
    }
}

#[cfg(feature = "sealed-output")]
fn encrypt_wallet_contents(
    contents: &[u8],
    recipient: &EmbeddedAgeRecipient,
) -> Result<Zeroizing<Vec<u8>>> {
    let encrypted = match recipient {
        EmbeddedAgeRecipient::X25519(recipient) => age::encrypt(recipient, contents),
        EmbeddedAgeRecipient::TaggedPq(recipient) => age::encrypt(recipient, contents),
    }
    .context("could not encrypt wallet output with age")?;
    Ok(Zeroizing::new(encrypted))
}

#[cfg(feature = "sealed-output")]
fn parse_age_recipient(value: &str) -> Result<EmbeddedAgeRecipient> {
    if value.starts_with("age1pq1") {
        anyhow::bail!(
            "this Rust age library does not support standard age1pq1 software recipients yet; use an age1tagpq1 hardware recipient or a classic age1 X25519 recipient"
        );
    }
    if value.starts_with("age1tagpq1") {
        return value
            .parse::<age::tagpq::Recipient>()
            .map(EmbeddedAgeRecipient::TaggedPq)
            .map_err(|error| {
                anyhow::anyhow!("embedded age post-quantum recipient is invalid: {error}")
            });
    }
    value
        .parse::<age::x25519::Recipient>()
        .map(EmbeddedAgeRecipient::X25519)
        .map_err(|error| anyhow::anyhow!("embedded age X25519 recipient is invalid: {error}"))
}

#[cfg(feature = "sealed-output")]
fn embedded_age_recipient() -> Result<EmbeddedAgeRecipient> {
    parse_age_recipient(env!("TON_VANITY_AGE_RECIPIENT"))
}

fn write_secret_file(path: &Path, found: &[(Option<String>, FoundWallet)]) -> Result<()> {
    #[cfg(feature = "sealed-output")]
    {
        let recipient = embedded_age_recipient()?;
        write_secret_file_to_recipient(path, found, &recipient)
    }
    #[cfg(not(feature = "sealed-output"))]
    {
        let contents = serialize_wallet_secrets(found)?;
        write_protected_file(path, contents.as_bytes())
    }
}

#[cfg(feature = "sealed-output")]
fn write_secret_file_to_recipient(
    path: &Path,
    found: &[(Option<String>, FoundWallet)],
    recipient: &EmbeddedAgeRecipient,
) -> Result<()> {
    let contents = serialize_wallet_secrets(found)?;
    let ciphertext = encrypt_wallet_contents(contents.as_bytes(), recipient)?;
    write_protected_file(path, &ciphertext)
}

#[cfg(feature = "sealed-output")]
fn harden_sealed_process() {
    #[cfg(unix)]
    let core_dumps_disabled = {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: setrlimit reads the initialized rlimit value for this process.
        unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0 }
    };

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let process_nondumpable = {
        // SAFETY: PR_SET_DUMPABLE takes scalar arguments and applies only to this process.
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0 }
    };

    #[cfg(unix)]
    eprintln!(
        "sealed_memory_hardening=core_limit_zero:{}{}",
        core_dumps_disabled,
        {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                format!(" process_nondumpable:{process_nondumpable}")
            }
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            {
                String::new()
            }
        }
    );

    #[cfg(windows)]
    eprintln!(
        "sealed_memory_hardening=zeroization_only; Windows cannot prevent an administrator or same-user debugger from inspecting this process"
    );
}

#[cfg(not(feature = "sealed-output"))]
fn harden_sealed_process() {}

fn effective_search_backend(requested: GpuBackend) -> Result<GpuBackend> {
    #[cfg(all(feature = "sealed-output", not(feature = "sealed-output-gpu")))]
    {
        match requested {
            GpuBackend::Cpu => Ok(GpuBackend::Cpu),
            GpuBackend::Auto | GpuBackend::Hybrid => {
                eprintln!(
                    "sealed_execution=CPU; this build keeps mnemonic and passphrase data off GPU devices and drivers"
                );
                Ok(GpuBackend::Cpu)
            }
            _ => anyhow::bail!(
                "this sealed-output build is CPU-only; rebuild with --features sealed-output-gpu to opt into exposing mnemonic/passphrase data to the GPU and its driver"
            ),
        }
    }
    #[cfg(any(not(feature = "sealed-output"), feature = "sealed-output-gpu"))]
    {
        #[cfg(all(feature = "sealed-output", feature = "sealed-output-gpu"))]
        eprintln!(
            "warning: sealed-output-gpu is enabled; GPU devices and drivers receive mnemonic/passphrase-derived data and may retain opaque copies"
        );
        Ok(requested)
    }
}

#[cfg(all(feature = "sealed-output", not(feature = "sealed-output-gpu")))]
fn enforce_benchmark_security_policy() -> Result<()> {
    anyhow::bail!(
        "GPU benchmarking is disabled in this CPU-only sealed-output build; a prompted benchmark passphrase would otherwise be sent to a GPU. Rebuild with --features sealed-output-gpu only if you accept that exposure"
    )
}

#[cfg(all(feature = "sealed-output", feature = "sealed-output-gpu"))]
fn enforce_benchmark_security_policy() -> Result<()> {
    eprintln!(
        "warning: sealed-output-gpu benchmark may send the prompted passphrase and test mnemonic to GPU devices and drivers"
    );
    Ok(())
}

#[cfg(not(feature = "sealed-output"))]
fn enforce_benchmark_security_policy() -> Result<()> {
    Ok(())
}

fn preflight_secret_output_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => anyhow::bail!("secret output path {} already exists", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("could not inspect secret output path {}", path.display())
            });
        }
    }

    let (temporary, file) = create_secret_temp_file(path).with_context(|| {
        format!(
            "daemon cannot create protected secret output beside {}",
            path.display()
        )
    })?;
    drop(file);
    drop(temporary);
    Ok(())
}
fn format_elapsed(duration: Duration) -> String {
    format_duration_dhms(duration)
}

fn spawn_progress_reporter(
    progress: Arc<SearchProgress>,
    stop: Arc<AtomicBool>,
    expected_candidates: u64,
) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        if !std::io::stderr().is_terminal() {
            return 0;
        }
        let mut samples = VecDeque::<(Instant, u64, u64, u64)>::with_capacity(12);
        let mut last_line_width = 0usize;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if stop.load(Ordering::Acquire) {
                break;
            }

            let now = Instant::now();
            let Some(work_started) = progress.search_work_started_at() else {
                continue;
            };
            let checked = progress.checked_addresses();
            let (cpu_candidates, gpu_candidates) = progress.pbkdf2_candidates_by_source();
            if samples.is_empty() {
                samples.push_back((work_started, 0, 0, 0));
            }
            samples.push_back((now, checked, cpu_candidates, gpu_candidates));
            while samples.len() > 2
                && now.duration_since(samples.front().expect("sample exists").0)
                    > Duration::from_secs(2)
            {
                samples.pop_front();
            }

            let (window_start, window_count, window_cpu, window_gpu) = samples
                .front()
                .copied()
                .unwrap_or((now, checked, cpu_candidates, gpu_candidates));
            let window_elapsed = now.duration_since(window_start).as_secs_f64().max(0.001);
            let recent_rate = checked.saturating_sub(window_count) as f64 / window_elapsed;
            let cpu_rate = cpu_candidates.saturating_sub(window_cpu) as f64 / window_elapsed;
            let gpu_rate = gpu_candidates.saturating_sub(window_gpu) as f64 / window_elapsed;
            let elapsed = now.duration_since(work_started);
            let average_rate = checked as f64 / elapsed.as_secs_f64().max(0.001);
            let eta = if expected_candidates == u64::MAX {
                "unknown".to_string()
            } else if checked == 0 || average_rate <= 0.0 {
                "pending".to_string()
            } else {
                let remaining = expected_candidates.saturating_sub(checked);
                let seconds = remaining as f64 / average_rate;
                if seconds.is_finite() && seconds < u64::MAX as f64 {
                    format_elapsed(Duration::from_secs_f64(seconds))
                } else {
                    "very long".to_string()
                }
            };
            let dispatch_elapsed = progress.current_gpu_dispatch_elapsed();
            let dispatch_candidates = progress.current_gpu_dispatch_candidates();
            let last_dispatch = progress.last_gpu_dispatch();
            let result_batches = progress.current_gpu_result_batches_pending();
            let gpu_status = dispatch_elapsed.map_or_else(
                || {
                    if result_batches > 0 {
                        format!("{result_batches} result batch(es) on CPU")
                    } else {
                        last_dispatch.map_or_else(
                            || "idle".to_string(),
                            |(count, micros)| {
                                format!("last {count} {:.0}ms", micros as f64 / 1000.0)
                            },
                        )
                    }
                },
                |dispatch_elapsed| {
                    if result_batches > 0 {
                        format!(
                            "{dispatch_candidates} pending {:.1}s; {result_batches} result batch(es)",
                            dispatch_elapsed.as_secs_f64()
                        )
                    } else {
                        format!(
                            "{dispatch_candidates} pending {:.1}s",
                            dispatch_elapsed.as_secs_f64()
                        )
                    }
                },
            );
            let elapsed = format_elapsed(elapsed);
            let line = format_live_status(LiveStatus {
                checked,
                recent_rate,
                average_rate,
                cpu_candidates,
                cpu_rate,
                gpu_candidates,
                gpu_rate,
                elapsed: &elapsed,
                eta: &eta,
                gpu_status: &gpu_status,
                max_width: terminal_columns().saturating_sub(1).max(1),
            });
            render_live_status(&line, &mut last_line_width);
        }
        last_line_width
    })
}

fn compact_quantity(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "?".into();
    }
    for (threshold, suffix) in [
        (1_000_000_000_000.0, 'T'),
        (1_000_000_000.0, 'G'),
        (1_000_000.0, 'M'),
        (1_000.0, 'k'),
    ] {
        if value >= threshold {
            return format!("{:.1}{suffix}", value / threshold);
        }
    }
    format!("{value:.0}")
}

fn compact_duration(value: &str) -> &str {
    value.split_once('.').map_or(value, |(whole, _)| whole)
}

struct LiveStatus<'a> {
    checked: u64,
    recent_rate: f64,
    average_rate: f64,
    cpu_candidates: u64,
    cpu_rate: f64,
    gpu_candidates: u64,
    gpu_rate: f64,
    elapsed: &'a str,
    eta: &'a str,
    gpu_status: &'a str,
    max_width: usize,
}

fn format_live_status(status: LiveStatus<'_>) -> String {
    let LiveStatus {
        checked,
        recent_rate,
        average_rate,
        cpu_candidates,
        cpu_rate,
        gpu_candidates,
        gpu_rate,
        elapsed,
        eta,
        gpu_status,
        max_width,
    } = status;
    let full = format!(
        "{checked} checked | {recent_rate:.0}/s (avg {average_rate:.0}) | CPU {cpu_candidates} ({cpu_rate:.0}/s) + GPU {gpu_candidates} ({gpu_rate:.0}/s) | {elapsed} | ETA {eta} | GPU {gpu_status}"
    );
    if full.len() <= max_width {
        return full;
    }

    let elapsed = compact_duration(elapsed);
    let eta = compact_duration(eta);
    let compact = format!(
        "{} checked|{}/s|CPU{}/{} GPU{}/{}|{}|ETA {}",
        compact_quantity(checked as f64),
        compact_quantity(recent_rate),
        compact_quantity(cpu_candidates as f64),
        compact_quantity(cpu_rate),
        compact_quantity(gpu_candidates as f64),
        compact_quantity(gpu_rate),
        elapsed,
        eta,
    );
    if compact.len() <= max_width {
        return compact;
    }

    let minimal = format!(
        "{}|{}/s|ETA {}",
        compact_quantity(checked as f64),
        compact_quantity(recent_rate),
        eta,
    );
    minimal.chars().take(max_width).collect()
}

fn terminal_columns() -> usize {
    #[cfg(windows)]
    if let Some(columns) = windows_terminal_columns() {
        return columns;
    }
    #[cfg(unix)]
    if let Some(columns) = unix_terminal_columns() {
        return columns;
    }
    env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|columns| *columns > 0)
        .unwrap_or(80)
}

#[cfg(windows)]
fn windows_terminal_columns() -> Option<usize> {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE,
    };

    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if handle.is_null() || GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        usize::try_from(info.srWindow.Right - info.srWindow.Left + 1).ok()
    }
}

#[cfg(unix)]
fn unix_terminal_columns() -> Option<usize> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    let result = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    let size = unsafe { size.assume_init() };
    (size.ws_col > 0).then_some(size.ws_col as usize)
}

fn render_live_status(line: &str, last_line_width: &mut usize) {
    let padding = last_line_width.saturating_sub(line.len());
    *last_line_width = line.len();
    eprint!("\r{line}{}", " ".repeat(padding));
    let _ = std::io::stderr().flush();
}

fn clear_progress_line(width: usize) {
    if width == 0 || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("\r{}\r", " ".repeat(width));
    let _ = std::io::stderr().flush();
}
fn daemon_state_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("ton-vanity-rust")
            .join("daemon");
    }
    #[cfg(unix)]
    {
        if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(state_home)
                .join("ton-vanity-rust")
                .join("daemon");
        }
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("ton-vanity-rust")
                .join("daemon");
        }
    }
    env::temp_dir().join("ton-vanity-rust").join("daemon")
}

fn daemon_file(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

fn resolve_daemon_output_path(path: &Path, executable: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let has_explicit_parent = path
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());
    if has_explicit_parent {
        return Ok(env::current_dir()?.join(path));
    }

    let executable_dir = executable
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("could not locate the executable directory for daemon output")?;
    Ok(executable_dir.join(path))
}

fn selected_output_path(requested: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(path) = requested {
        return Ok(Some(path.to_path_buf()));
    }
    #[cfg(feature = "sealed-output")]
    {
        let _ = embedded_age_recipient()?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_nanos();
        return Ok(Some(
            env::current_dir()?.join(format!("ton-vanity-sealed-{stamp}.age")),
        ));
    }
    #[cfg(not(feature = "sealed-output"))]
    Ok(None)
}

fn remove_output_arguments(args: &mut Vec<OsString>) {
    let mut index = 0;
    while index < args.len() {
        if args[index] == OsStr::new("--output") {
            args.remove(index);
            if index < args.len() {
                args.remove(index);
            }
        } else if args[index]
            .to_str()
            .is_some_and(|argument| argument.starts_with("--output="))
        {
            args.remove(index);
        } else {
            index += 1;
        }
    }
}

fn write_daemon_text(path: &Path, text: &str) -> Result<()> {
    use std::fs::OpenOptions;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn read_daemon_text(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn daemon_has_fresh_heartbeat(dir: &Path) -> bool {
    let Some(heartbeat) = read_daemon_text(&daemon_file(dir, "heartbeat"))
        .and_then(|value| value.parse::<u128>().ok())
    else {
        return false;
    };
    epoch_millis().saturating_sub(heartbeat) < 10_000
}

fn daemon_status(dir: &Path) -> String {
    let has_lock = daemon_file(dir, "daemon.lock").exists();
    let stored_state =
        read_daemon_text(&daemon_file(dir, "state")).unwrap_or_else(|| "not started".to_string());
    if ["completed", "stopped", "failed"].contains(&stored_state.as_str()) {
        stored_state
    } else if has_lock && daemon_has_fresh_heartbeat(dir) {
        if stored_state == "stopping" {
            "stopping".to_string()
        } else {
            "running".to_string()
        }
    } else if has_lock {
        "stale".to_string()
    } else {
        stored_state
    }
}
fn ensure_daemon_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("could not create daemon state directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn daemon_passphrase_from_stdin() -> Result<Zeroizing<String>> {
    let mut stdin = std::io::stdin().lock();
    let mut length_bytes = [0u8; 8];
    stdin.read_exact(&mut length_bytes)?;
    let length = u64::from_le_bytes(length_bytes);
    if length > 1_048_576 {
        anyhow::bail!("daemon passphrase input exceeds the 1 MiB safety limit");
    }
    let mut bytes = Zeroizing::new(vec![0u8; length as usize]);
    stdin.read_exact(&mut bytes)?;
    let value = String::from_utf8(std::mem::take(&mut *bytes))
        .context("daemon passphrase input was not valid UTF-8")?;
    Ok(Zeroizing::new(value))
}

struct DaemonLaunchLock {
    path: PathBuf,
    active: bool,
}

impl DaemonLaunchLock {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn transfer_to_worker(&mut self) {
        self.active = false;
    }
}

impl Drop for DaemonLaunchLock {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_file(&self.path);
            if let Some(dir) = self.path.parent() {
                let _ = write_daemon_text(&daemon_file(dir, "state"), "failed");
            }
        }
    }
}
struct DaemonWorkerGuard {
    state_dir: PathBuf,
    cancellation: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    heartbeat_thread: Option<std::thread::JoinHandle<()>>,
    completed: bool,
}

impl DaemonWorkerGuard {
    fn start(state_dir: PathBuf, cancellation: Arc<AtomicBool>) -> Result<Self> {
        write_daemon_text(
            &daemon_file(&state_dir, "pid"),
            &std::process::id().to_string(),
        )?;
        write_daemon_text(&daemon_file(&state_dir, "state"), "running")?;
        write_daemon_text(
            &daemon_file(&state_dir, "heartbeat"),
            &epoch_millis().to_string(),
        )?;

        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let worker_cancellation = Arc::clone(&cancellation);
        let worker_dir = state_dir.clone();
        let heartbeat_thread = std::thread::Builder::new()
            .name("vanity-daemon-heartbeat".to_string())
            .spawn(move || {
                let mut ticks = 0u8;
                while !worker_finished.load(Ordering::Acquire) {
                    if daemon_file(&worker_dir, "stop.request").exists() {
                        worker_cancellation.store(true, Ordering::Release);
                        let _ = write_daemon_text(&daemon_file(&worker_dir, "state"), "stopping");
                    }
                    if ticks == 0 {
                        let _ = write_daemon_text(
                            &daemon_file(&worker_dir, "heartbeat"),
                            &epoch_millis().to_string(),
                        );
                    }
                    ticks = (ticks + 1) % 5;
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
            .context("could not start daemon heartbeat worker")?;

        Ok(Self {
            state_dir,
            cancellation,
            finished,
            heartbeat_thread: Some(heartbeat_thread),
            completed: false,
        })
    }

    fn mark_complete(&mut self) -> Result<()> {
        write_daemon_text(&daemon_file(&self.state_dir, "state"), "completed")?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for DaemonWorkerGuard {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(thread) = self.heartbeat_thread.take() {
            let _ = thread.join();
        }
        if !self.completed {
            let state = if self.cancellation.load(Ordering::Acquire)
                || daemon_file(&self.state_dir, "stop.request").exists()
            {
                "stopped"
            } else {
                "failed"
            };
            let _ = write_daemon_text(&daemon_file(&self.state_dir, "state"), state);
        }
        let _ = fs::remove_file(daemon_file(&self.state_dir, "daemon.lock"));
        let _ = fs::remove_file(daemon_file(&self.state_dir, "stop.request"));
    }
}

fn run_daemon_command(action: DaemonAction, cli: &Cli) -> Result<()> {
    let dir = daemon_state_dir();
    match action {
        DaemonAction::Status => {
            println!("daemon_state={}", daemon_status(&dir));
            if let Some(pid) = read_daemon_text(&daemon_file(&dir, "pid")) {
                println!("daemon_pid={pid}");
            }
            println!("daemon_log={}", daemon_file(&dir, "daemon.log").display());
            let output_path = read_daemon_text(&daemon_file(&dir, "output.path"));
            if let Some(path) = output_path.filter(|path| !path.is_empty() && path != "none") {
                println!("daemon_output={path}");
            } else {
                println!("daemon_output=none (wallet secrets will be withheld)");
            }
            Ok(())
        }
        DaemonAction::Stop => {
            if !daemon_file(&dir, "daemon.lock").exists() || daemon_status(&dir) != "running" {
                println!("daemon_state={}", daemon_status(&dir));
                return Ok(());
            }
            write_daemon_text(&daemon_file(&dir, "state"), "stopping")?;
            write_daemon_text(&daemon_file(&dir, "stop.request"), "stop")?;
            println!("daemon_state=stop requested");
            Ok(())
        }
        DaemonAction::Start => {
            if cli.daemon_worker {
                anyhow::bail!("daemon worker mode is internal and cannot start another daemon");
            }
            if cli.show_secrets {
                anyhow::bail!(
                    "daemon mode does not allow --show-secrets because output is captured in a log"
                );
            }
            if cli.prefix.is_empty() && cli.suffix.is_empty() && cli.exact.is_empty() {
                anyhow::bail!("daemon start requires --prefix, --suffix, or --exact");
            }

            let passphrase = if cli.prompt_passphrase {
                Zeroizing::new(rpassword::prompt_password(
                    "BIP39 passphrase (leave blank for none): ",
                )?)
            } else {
                Zeroizing::new(String::new())
            };
            let wallet = WalletVersion::parse(&cli.wallet)?;
            let backend = cli
                .backend
                .parse::<GpuBackend>()
                .map_err(anyhow::Error::msg)?;
            let network = if cli.testnet {
                Network::Testnet
            } else {
                Network::Mainnet
            };
            let header = ton_vanity_rust::address::friendly_address(
                0,
                [0; 32],
                cli.bounceable,
                network == Network::Testnet,
            );
            let matcher = Matcher {
                prefix: normalize_friendly_prefixes(cli.prefix.clone(), &header[..2]),
                suffix: cli.suffix.clone(),
                exact: cli.exact.clone(),
                mode: cli.match_mode,
                case_sensitive: cli.case_sensitive,
            };
            matcher.validate()?;
            let validation = SearchConfig {
                wallet,
                network,
                bounceable: cli.bounceable,
                matcher,
                threads: cli.threads,
                gpu_devices: cli.gpu_devices,
                account: cli.account,
                passphrase: Zeroizing::new(passphrase.to_string()),
                gpu_backend: backend,
                gpu_batch: cli.gpu_batch,
                max_candidates: cli.max_candidates,
                cancellation: Arc::new(AtomicBool::new(false)),
            };
            validation.validate()?;

            let executable = env::current_exe().context("could not locate current executable")?;
            let requested_output = selected_output_path(cli.output.as_deref())?;
            let output_path = requested_output
                .as_deref()
                .map(|path| resolve_daemon_output_path(path, &executable))
                .transpose()?;

            ensure_daemon_dir(&dir)?;
            let lock_path = daemon_file(&dir, "daemon.lock");
            if lock_path.exists() {
                let state = daemon_status(&dir);
                if state == "running" || state == "stopping" {
                    anyhow::bail!(
                        "a vanity search daemon is already running; use daemon status or daemon stop"
                    );
                }
                fs::remove_file(&lock_path)?;
            }
            let mut lock_options = std::fs::OpenOptions::new();
            lock_options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                lock_options.mode(0o600);
            }
            let mut lock = lock_options
                .open(&lock_path)
                .context("could not acquire daemon lock; another start may be in progress")?;
            lock.write_all(b"starting")?;
            drop(lock);
            let mut launch_lock = DaemonLaunchLock::new(lock_path.clone());
            if let Some(path) = output_path.as_deref() {
                preflight_secret_output_target(path)?;
            }
            let output_path_text = output_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string());
            write_daemon_text(&daemon_file(&dir, "output.path"), &output_path_text)?;
            write_daemon_text(&daemon_file(&dir, "state"), "starting")?;
            write_daemon_text(&daemon_file(&dir, "heartbeat"), &epoch_millis().to_string())?;
            let _ = fs::remove_file(daemon_file(&dir, "stop.request"));

            let mut args = env::args_os().skip(1).collect::<Vec<_>>();
            let command_index = args
                .windows(2)
                .position(|pair| pair[0] == "daemon" && pair[1] == "start")
                .context("could not locate daemon start command in process arguments")?;
            args.drain(command_index..command_index + 2);
            args.retain(|argument| argument != "--prompt-passphrase");
            remove_output_arguments(&mut args);
            if let Some(path) = output_path.as_deref() {
                args.push(OsString::from("--output"));
                args.push(path.as_os_str().to_os_string());
            }
            args.push(OsString::from("--daemon-worker"));
            args.push(OsString::from("--daemon-state-dir"));
            args.push(dir.as_os_str().to_os_string());
            if cli.prompt_passphrase {
                args.push(OsString::from("--daemon-passphrase-stdin"));
            }

            let log_path = daemon_file(&dir, "daemon.log");
            match fs::remove_file(&log_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("could not clear old daemon log {}", log_path.display())
                    });
                }
            }
            let log = create_secret_file(&log_path).with_context(|| {
                format!("could not create private daemon log {}", log_path.display())
            })?;
            let mut child_command = ProcessCommand::new(executable);
            child_command.args(args);
            child_command.stdout(Stdio::from(log.try_clone()?));
            child_command.stderr(Stdio::from(log));
            child_command.stdin(if cli.prompt_passphrase {
                Stdio::piped()
            } else {
                Stdio::null()
            });

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                // SAFETY: setsid only changes the spawned child's session; no parent state is touched.
                unsafe {
                    child_command.pre_exec(|| {
                        // SAFETY: setsid has no pointer arguments and returns an OS error on failure.
                        if libc::setsid() == -1 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                child_command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
            }

            let mut child = child_command
                .spawn()
                .context("could not start background search daemon")?;
            launch_lock.transfer_to_worker();
            write_daemon_text(&daemon_file(&dir, "pid"), &child.id().to_string())?;

            if cli.prompt_passphrase {
                let send_result = (|| -> std::io::Result<()> {
                    let mut stdin = child.stdin.take().ok_or_else(|| {
                        std::io::Error::other("daemon worker stdin was not available")
                    })?;
                    let bytes = passphrase.as_bytes();
                    stdin.write_all(&(bytes.len() as u64).to_le_bytes())?;
                    stdin.write_all(bytes)?;
                    stdin.flush()
                })();
                if let Err(error) = send_result {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_file(&lock_path);
                    let _ = write_daemon_text(&daemon_file(&dir, "state"), "failed");
                    return Err(error).context("could not send passphrase to daemon worker");
                }
            }
            println!("daemon_state=starting");
            println!("daemon_pid={}", child.id());
            println!("daemon_log={}", log_path.display());
            if let Some(path) = output_path.as_deref() {
                println!("daemon_output={}", path.display());
            } else {
                eprintln!(
                    "warning: daemon started without --output; a found wallet's mnemonic and private key will be withheld and cannot be recovered from the daemon log. Set --output <file> to save them."
                );
            }
            Ok(())
        }
    }
}
fn main() -> Result<()> {
    harden_sealed_process();
    let c = Cli::parse();
    if let Some(cmd) = c.command.clone() {
        match cmd {
            Command::ListWallets => {
                for wallet in WalletVersion::ALL {
                    println!("{}", wallet.name());
                }
                return Ok(());
            }
            Command::Devices => {
                for device in devices()? {
                    println!(
                        "{:?}: {} [{}] {}",
                        device.backend, device.name, device.vendor, device.device_type
                    );
                }
                return Ok(());
            }
            Command::Capabilities => {
                print!("{}", capability_report());
                return Ok(());
            }
            Command::Threads => {
                match std::thread::available_parallelism() {
                    Ok(count) => println!("available_logical_threads={}", count.get()),
                    Err(error) => println!("available_logical_threads=unknown ({error})"),
                }
                println!("default_search_threads={}", rayon::current_num_threads());
                println!("max_search_threads=256");
                println!(
                    "note=logical processors available to this process; not physical core count"
                );
                return Ok(());
            }
            Command::Benchmark => {
                enforce_benchmark_security_policy()?;
                let requested_backend = c
                    .backend
                    .parse::<GpuBackend>()
                    .map_err(anyhow::Error::msg)?;
                let only_backend = match requested_backend {
                    GpuBackend::Auto => None,
                    backend => Some(backend),
                };
                let wallet = WalletVersion::parse(&c.wallet)?;
                let passphrase = if c.prompt_passphrase {
                    Zeroizing::new(rpassword::prompt_password(
                        "BIP39 passphrase for GPU benchmark (leave blank for none): ",
                    )?)
                } else {
                    Zeroizing::new(String::new())
                };
                let cfg = SearchConfig {
                    wallet,
                    network: if c.testnet {
                        Network::Testnet
                    } else {
                        Network::Mainnet
                    },
                    bounceable: c.bounceable,
                    matcher: Matcher {
                        // SearchConfig validation requires a matcher; the benchmark never matches addresses.
                        prefix: vec!["A".into()],
                        suffix: vec![],
                        exact: vec![],
                        mode: MatchMode::All,
                        case_sensitive: false,
                    },
                    threads: c.threads,
                    gpu_devices: c.gpu_devices,
                    account: c.account,
                    passphrase: Zeroizing::new(passphrase.to_string()),
                    gpu_backend: requested_backend,
                    gpu_batch: c.gpu_batch,
                    max_candidates: None,
                    cancellation: Arc::new(AtomicBool::new(false)),
                };
                cfg.validate()?;
                return ton_vanity_rust::search::benchmark_gpu_backends(&cfg, only_backend);
            }
            Command::Daemon { action } => return run_daemon_command(action, &c),
            Command::GpuSelfTest { backend } => {
                let _requested = backend.parse::<GpuBackend>().map_err(anyhow::Error::msg)?;
                #[cfg(all(feature = "d3d11", target_os = "windows"))]
                if _requested == GpuBackend::Dx11 {
                    let engine = ton_vanity_rust::gpu::d3d11::D3d11SeedEngine::new(64)?;
                    engine.acceptance_test()?;
                    println!("Direct3D 11 PBKDF2 parity: PASS ({})", engine.device_name());
                    return Ok(());
                }
                #[cfg(not(all(feature = "d3d11", target_os = "windows")))]
                if _requested == GpuBackend::Dx11 {
                    anyhow::bail!(
                        "Direct3D 11 is only available on Windows with the d3d11 feature"
                    );
                }
                #[cfg(feature = "opencl")]
                if _requested == GpuBackend::OpenCl {
                    let engine = ton_vanity_rust::gpu::opencl::OpenClSeedEngine::new(64)?;
                    engine.acceptance_test()?;
                    println!("OpenCL PBKDF2 parity: PASS ({})", engine.name());
                    return Ok(());
                }
                #[cfg(not(feature = "opencl"))]
                if _requested == GpuBackend::OpenCl {
                    anyhow::bail!("OpenCL backend not compiled; rebuild with --features opencl");
                }
                #[cfg(feature = "cuda")]
                if _requested == GpuBackend::Cuda {
                    ton_vanity_rust::gpu::cuda_self_test()?;
                    println!("CUDA PBKDF2 self-test: PASS");
                    return Ok(());
                }
                #[cfg(feature = "gpu")]
                {
                    let engine = ton_vanity_rust::gpu::GpuSeedEngine::new(
                        &ton_vanity_rust::gpu::GpuSeedEngineConfig {
                            backend: _requested,
                            batch_size: 64,
                        },
                    )?;
                    engine.acceptance_test()?;
                    println!("GPU PBKDF2 self-test: PASS ({:?})", engine.backend());
                    return Ok(());
                }
                #[cfg(not(feature = "gpu"))]
                anyhow::bail!("GPU feature not compiled");
            }
        }
    }

    #[cfg(feature = "sealed-output")]
    if c.show_secrets {
        anyhow::bail!(
            "--show-secrets is disabled in sealed-output builds; results are encrypted to the recipient embedded at build time"
        );
    }

    let wallet = WalletVersion::parse(&c.wallet)?;
    let requested_backend = c
        .backend
        .parse::<GpuBackend>()
        .map_err(anyhow::Error::msg)?;
    let backend = effective_search_backend(requested_backend)?;
    let network = if c.testnet {
        Network::Testnet
    } else {
        Network::Mainnet
    };
    let header = ton_vanity_rust::address::friendly_address(
        0,
        [0; 32],
        c.bounceable,
        network == Network::Testnet,
    );
    let matcher = Matcher {
        prefix: normalize_friendly_prefixes(c.prefix, &header[..2]),
        suffix: c.suffix,
        exact: c.exact,
        mode: c.match_mode,
        case_sensitive: c.case_sensitive,
    };
    matcher.validate()?;
    let passphrase = if c.daemon_passphrase_stdin {
        daemon_passphrase_from_stdin()?
    } else if c.prompt_passphrase {
        Zeroizing::new(rpassword::prompt_password(
            "BIP39 passphrase (leave blank for none): ",
        )?)
    } else {
        Zeroizing::new(String::new())
    };
    let cfg = SearchConfig {
        wallet,
        network,
        bounceable: c.bounceable,
        matcher,
        threads: c.threads,
        gpu_devices: c.gpu_devices,
        account: c.account,
        passphrase: Zeroizing::new(passphrase.to_string()),
        gpu_backend: backend,
        gpu_batch: c.gpu_batch,
        max_candidates: c.max_candidates,
        cancellation: Arc::new(AtomicBool::new(false)),
    };
    cfg.validate()?;
    #[cfg(feature = "sealed-output")]
    let _sealed_recipient = embedded_age_recipient()?;
    let output_path = selected_output_path(c.output.as_deref())?;
    #[cfg(feature = "sealed-output")]
    if let Some(path) = output_path.as_deref() {
        preflight_secret_output_target(path)?;
    }
    #[cfg(feature = "sealed-output")]
    eprintln!(
        "sealed_output=age recipient embedded in this binary kind={}",
        _sealed_recipient.kind()
    );
    let mut daemon_worker = if c.daemon_worker {
        let state_dir = c
            .daemon_state_dir
            .clone()
            .context("daemon worker state directory is missing")?;
        Some(DaemonWorkerGuard::start(
            state_dir,
            Arc::clone(&cfg.cancellation),
        )?)
    } else {
        None
    };
    if !c.daemon_worker {
        let signal_cancellation = Arc::clone(&cfg.cancellation);
        ctrlc::set_handler(move || {
            signal_cancellation.store(true, Ordering::Release);
        })
        .context("could not install Ctrl+C handler")?;
    }
    println!(
        "wallet={} requested_backend={}",
        wallet.name(),
        search_backend_name(&c.backend)
    );

    let counter = AtomicU64::new(0);
    let progress = Arc::new(SearchProgress::default());
    let stop_reporter = Arc::new(AtomicBool::new(false));
    let search_started = Instant::now();
    let expected_candidates = expected_search_candidates(&cfg);
    eprintln!(
        "estimated_candidates_to_match={}",
        if expected_candidates == u64::MAX {
            "unknown/unbounded".to_string()
        } else {
            expected_candidates.to_string()
        }
    );
    let reporter = spawn_progress_reporter(
        Arc::clone(&progress),
        Arc::clone(&stop_reporter),
        expected_candidates,
    );
    let search_result = match backend {
        GpuBackend::Cpu => search_cpu(&cfg, &counter, &progress),
        GpuBackend::Auto => search_auto(&cfg, &counter, &progress),
        GpuBackend::Hybrid => search_hybrid(&cfg, &counter, &progress),
        GpuBackend::Vulkan
        | GpuBackend::Dx12
        | GpuBackend::Dx11
        | GpuBackend::Metal
        | GpuBackend::OpenGl
        | GpuBackend::OpenCl
        | GpuBackend::Cuda => search_gpu(&cfg, &counter, &progress),
    };
    let total_elapsed = search_started.elapsed();
    let elapsed = progress.search_elapsed().unwrap_or(total_elapsed);
    stop_reporter.store(true, Ordering::Release);
    let progress_line_width = reporter.join().unwrap_or(0);
    clear_progress_line(progress_line_width);
    let found = match search_result {
        Ok(found) => found,
        Err(error) => {
            if matches!(backend, GpuBackend::Hybrid | GpuBackend::Auto) {
                let (cpu_candidates, gpu_candidates) = progress.pbkdf2_candidates_by_source();
                println!("cpu_pbkdf2_candidates={cpu_candidates}");
                println!("gpu_pbkdf2_candidates={gpu_candidates}");
            }
            let checked = progress.checked_addresses();
            println!("checked_addresses={checked}");
            println!("search_elapsed={}", format_elapsed(elapsed));
            println!("total_elapsed={}", format_elapsed(total_elapsed));
            if checked > 0 {
                let measured_rate = checked as f64 / elapsed.as_secs_f64().max(0.001);
                println!("average_rate={measured_rate:.2} addresses/s");
                if expected_candidates != u64::MAX {
                    let seconds = expected_candidates as f64 / measured_rate;
                    if seconds.is_finite() && seconds < u64::MAX as f64 {
                        println!(
                            "estimated_search_time_at_measured_rate={}",
                            format_elapsed(Duration::from_secs_f64(seconds))
                        );
                    }
                }
            }
            return Err(error);
        }
    };
    let found_results = progress.take_found_results();
    let found_wallets = if found_results.is_empty() {
        vec![(None, found)]
    } else {
        drop(found);
        found_results
            .into_iter()
            .map(|(criterion_index, wallet)| {
                let label = (cfg.matcher.criterion_count() > 1)
                    .then(|| cfg.matcher.criterion_label(criterion_index))
                    .flatten();
                (label, wallet)
            })
            .collect::<Vec<_>>()
    };

    let checked_addresses = progress.checked_addresses();
    if c.match_mode == MatchMode::All && cfg.matcher.criterion_count() > 1 {
        println!(
            "matched {}/{} criteria after {checked_addresses} addresses checked",
            progress.matched_criteria_count(),
            cfg.matcher.criterion_count()
        );
    } else {
        println!("matched after {checked_addresses} addresses checked");
    }
    if matches!(backend, GpuBackend::Hybrid | GpuBackend::Auto) {
        let (cpu_candidates, gpu_candidates) = progress.pbkdf2_candidates_by_source();
        println!("cpu_pbkdf2_candidates={cpu_candidates}");
        println!("gpu_pbkdf2_candidates={gpu_candidates}");
    }
    println!("search_elapsed={}", format_elapsed(elapsed));
    println!("total_elapsed={}", format_elapsed(total_elapsed));
    println!(
        "average_rate={:.2} addresses/s",
        checked_addresses as f64 / elapsed.as_secs_f64().max(0.001)
    );
    let average_rate = checked_addresses as f64 / elapsed.as_secs_f64().max(0.001);
    if expected_candidates != u64::MAX && average_rate > 0.0 {
        let estimated_seconds = expected_candidates as f64 / average_rate;
        if estimated_seconds.is_finite() && estimated_seconds < u64::MAX as f64 {
            println!(
                "estimated_search_time_at_measured_rate={}",
                format_elapsed(Duration::from_secs_f64(estimated_seconds))
            );
        }
    }
    if let Some((candidates, micros)) = progress.last_gpu_dispatch() {
        println!(
            "last_gpu_pbkdf2_call={candidates} candidates in {:.2} ms (host-observed)",
            micros as f64 / 1000.0
        );
    }
    if let Some((candidates, micros)) = progress.last_gpu_kernel() {
        println!(
            "last_opencl_kernel={candidates} candidates in {:.2} ms (device event)",
            micros as f64 / 1000.0
        );
    }
    for (index, (criterion, wallet)) in found_wallets.iter().enumerate() {
        if found_wallets.len() > 1 {
            println!("wallet_result={}", index + 1);
        }
        if let Some(criterion) = criterion {
            println!("matched_criterion={criterion}");
        }
        println!("address={}", wallet.address);
    }

    if let Some(path) = output_path.as_deref() {
        #[cfg(not(feature = "sealed-output"))]
        eprintln!(
            "warning: writing wallet secrets to {}; protect this file and its access permissions",
            path.display()
        );
        #[cfg(feature = "sealed-output")]
        eprintln!("writing age-encrypted wallet result to {}", path.display());
        write_secret_file(path, &found_wallets)?;
        #[cfg(not(feature = "sealed-output"))]
        println!("secrets_saved={}", path.display());
        #[cfg(feature = "sealed-output")]
        println!("encrypted_result_saved={}", path.display());
    }
    if c.show_secrets {
        eprintln!(
            "warning: printing wallet secrets to stdout; terminal logs and shell capture can expose them"
        );
        for (index, (criterion, wallet)) in found_wallets.iter().enumerate() {
            if found_wallets.len() > 1 {
                println!("wallet_result={}", index + 1);
            }
            if let Some(criterion) = criterion {
                println!("matched_criterion={criterion}");
            }
            println!("mnemonic={}", wallet.mnemonic);
            println!("private_key={}", wallet.private_key_hex);
        }
    } else if output_path.is_none() {
        eprintln!(
            "wallet secrets withheld; use --output <new-file> or --show-secrets to export them"
        );
    }
    if let Some(worker) = daemon_worker.as_mut() {
        worker.mark_complete()?;
    }
    Ok(())
}
#[cfg(test)]
mod runtime_output_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos();
            for _ in 0..16 {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "ton-vanity-output-test-{}-{stamp}-{id}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create output test directory: {error}"),
                }
            }
            panic!("could not allocate a unique output test directory")
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn elapsed_format_has_day_hour_minute_second_fields() {
        assert_eq!(
            format_elapsed(Duration::from_millis(123)),
            "00:00:00:00.123"
        );
        assert_eq!(
            format_elapsed(Duration::from_secs(90061)),
            "01:01:01:01.000"
        );
        assert_eq!(
            format_elapsed(Duration::from_secs(172800)),
            "02:00:00:00.000"
        );
    }

    #[test]
    fn clap_accepts_prefix_and_suffix_values_starting_with_hyphens() {
        let cli = Cli::try_parse_from([
            "ton-vanity",
            "--backend",
            "cpu",
            "--prefix",
            "-xxx",
            "--suffix",
            "-abc",
            "--max-candidates",
            "1",
        ])
        .unwrap();
        assert_eq!(cli.prefix, ["-xxx"]);
        assert_eq!(cli.suffix, ["-abc"]);
    }

    #[test]
    fn daemon_bare_output_name_uses_executable_directory() {
        let scratch = ScratchDir::new();
        let executable = scratch.0.join("bin").join("ton-vanity.exe");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();

        assert_eq!(
            resolve_daemon_output_path(Path::new("wallet.txt"), &executable).unwrap(),
            executable.parent().unwrap().join("wallet.txt")
        );
        assert_eq!(
            resolve_daemon_output_path(&Path::new("secrets").join("wallet.txt"), &executable)
                .unwrap(),
            env::current_dir()
                .unwrap()
                .join(Path::new("secrets").join("wallet.txt"))
        );
        let absolute = scratch.0.join("absolute.txt");
        assert_eq!(
            resolve_daemon_output_path(&absolute, &executable).unwrap(),
            absolute
        );
    }

    #[test]
    fn daemon_worker_arguments_replace_both_output_option_forms() {
        let mut args = vec![
            OsString::from("--output"),
            OsString::from("old.txt"),
            OsString::from("--backend"),
            OsString::from("cpu"),
            OsString::from("--output=older.txt"),
        ];
        remove_output_arguments(&mut args);
        assert_eq!(args, [OsString::from("--backend"), OsString::from("cpu")]);
    }

    #[test]
    fn daemon_status_tracks_running_stopping_and_completed_states() {
        let scratch = ScratchDir::new();
        write_daemon_text(&daemon_file(&scratch.0, "daemon.lock"), "starting").unwrap();
        write_daemon_text(
            &daemon_file(&scratch.0, "heartbeat"),
            &epoch_millis().to_string(),
        )
        .unwrap();
        write_daemon_text(&daemon_file(&scratch.0, "state"), "running").unwrap();
        assert_eq!(daemon_status(&scratch.0), "running");

        write_daemon_text(&daemon_file(&scratch.0, "state"), "stopping").unwrap();
        assert_eq!(daemon_status(&scratch.0), "stopping");

        write_daemon_text(&daemon_file(&scratch.0, "state"), "completed").unwrap();
        assert_eq!(daemon_status(&scratch.0), "completed");
    }

    #[test]
    fn daemon_start_accepts_search_options_after_subcommand() {
        let cli = Cli::try_parse_from([
            "ton-vanity",
            "daemon",
            "start",
            "--backend",
            "cpu",
            "--suffix",
            "zzzzzz",
            "--wallet",
            "v4r2",
        ])
        .unwrap();
        assert_eq!(cli.suffix, vec!["zzzzzz"]);
        assert_eq!(cli.wallet, "v4r2");
        assert!(matches!(
            cli.command,
            Some(Command::Daemon {
                action: DaemonAction::Start
            })
        ));
    }
    #[test]
    fn cli_collects_repeated_match_criteria_and_mode() {
        let cli = Cli::try_parse_from([
            "ton-vanity",
            "--prefix",
            "UQ",
            "--prefix",
            "EQ",
            "--suffix",
            "abc",
            "--suffix",
            "xyz",
            "--match-mode",
            "any",
        ])
        .unwrap();
        assert_eq!(cli.prefix, vec!["UQ", "EQ"]);
        assert_eq!(cli.suffix, vec!["abc", "xyz"]);
        assert_eq!(cli.match_mode, MatchMode::Any);
    }

    #[test]
    fn gpu_batch_defaults_to_device_sizing_and_accepts_an_explicit_cap() {
        let automatic = Cli::try_parse_from(["ton-vanity", "--backend", "opengl"]).unwrap();
        assert_eq!(automatic.gpu_batch, 0);

        let capped =
            Cli::try_parse_from(["ton-vanity", "--backend", "opengl", "--gpu-batch", "4096"])
                .unwrap();
        assert_eq!(capped.gpu_batch, 4096);
    }

    #[test]
    fn live_status_fits_one_terminal_row_and_keeps_lane_rates_and_eta() {
        let line = format_live_status(LiveStatus {
            checked: 25_983,
            recent_rate: 1_017.0,
            average_rate: 912.11,
            cpu_candidates: 12_889,
            cpu_rate: 477.0,
            gpu_candidates: 12_800,
            gpu_rate: 536.0,
            elapsed: "00:00:00:28.486",
            eta: "00:00:04:47.403",
            gpu_status: "512 pending 0.2s",
            max_width: 79,
        });
        assert!(line.len() <= 79, "{line:?} has {} columns", line.len());
        assert!(line.contains("checked"));
        assert!(line.contains("CPU"));
        assert!(line.contains("GPU"));
        assert!(line.contains("ETA"));

        let narrow = format_live_status(LiveStatus {
            checked: 25_983,
            recent_rate: 1_017.0,
            average_rate: 912.11,
            cpu_candidates: 12_889,
            cpu_rate: 477.0,
            gpu_candidates: 12_800,
            gpu_rate: 536.0,
            elapsed: "00:00:00:28.486",
            eta: "00:00:04:47.403",
            gpu_status: "512 pending 0.2s",
            max_width: 38,
        });
        assert!(
            narrow.len() <= 38,
            "{narrow:?} has {} columns",
            narrow.len()
        );
        assert!(narrow.contains("ETA"));
    }
    #[cfg(not(feature = "sealed-output"))]
    #[test]
    fn secret_output_commits_once_and_never_overwrites() {
        let scratch = ScratchDir::new();
        let target = scratch.0.join("wallet.txt");
        let found = FoundWallet {
            address: "synthetic-address".into(),
            mnemonic: "synthetic mnemonic".into(),
            private_key_hex: "synthetic-key".into(),
        };
        let expected = concat!(
            "address=synthetic-address\n",
            "mnemonic=synthetic mnemonic\n",
            "private_key=synthetic-key\n"
        );

        write_secret_file(&target, &[(None, found)])
            .expect("first secret-file commit should succeed");
        assert_eq!(fs::read_to_string(&target).unwrap(), expected);

        let replacement = FoundWallet {
            address: "replacement-address".into(),
            mnemonic: "replacement mnemonic".into(),
            private_key_hex: "replacement-key".into(),
        };
        assert!(write_secret_file(&target, &[(None, replacement)]).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), expected);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
    }

    #[cfg(not(feature = "sealed-output"))]
    #[test]
    fn all_mode_secret_file_contains_each_matched_wallet() {
        let scratch = ScratchDir::new();
        let target = scratch.0.join("wallets.txt");
        let wallets = [
            (
                Some("prefix:EQ".into()),
                FoundWallet {
                    address: "address-one".into(),
                    mnemonic: "phrase one".into(),
                    private_key_hex: "key-one".into(),
                },
            ),
            (
                Some("suffix:YY".into()),
                FoundWallet {
                    address: "address-two".into(),
                    mnemonic: "phrase two".into(),
                    private_key_hex: "key-two".into(),
                },
            ),
        ];
        write_secret_file(&target, &wallets).unwrap();
        let contents = fs::read_to_string(&target).unwrap();
        assert!(
            contents
                .contains("wallet_result=1\nmatched_criterion=prefix:EQ\naddress=address-one\n")
        );
        assert!(
            contents
                .contains("wallet_result=2\nmatched_criterion=suffix:YY\naddress=address-two\n")
        );
        assert!(contents.contains("mnemonic=phrase one\nprivate_key=key-one\n"));
        assert!(contents.contains("mnemonic=phrase two\nprivate_key=key-two\n"));
    }

    #[test]
    fn daemon_output_preflight_checks_target_and_writability_without_leaking_temp_files() {
        let scratch = ScratchDir::new();
        let target = scratch.0.join("daemon-wallet.txt");
        preflight_secret_output_target(&target).unwrap();
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);

        fs::write(&target, "keep").unwrap();
        assert!(preflight_secret_output_target(&target).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");

        let missing_parent = scratch.0.join("missing").join("wallet.txt");
        assert!(preflight_secret_output_target(&missing_parent).is_err());
    }

    #[cfg(feature = "sealed-output")]
    #[test]
    fn sealed_wallet_contents_round_trip_only_for_the_age_identity() {
        let identity = age::x25519::Identity::generate();
        let recipient = EmbeddedAgeRecipient::X25519(identity.to_public());
        let scratch = ScratchDir::new();
        let target = scratch.0.join("wallet.age");
        let wallet = FoundWallet {
            address: "synthetic-address".into(),
            mnemonic: "synthetic phrase".into(),
            private_key_hex: "synthetic-key".into(),
        };
        write_secret_file_to_recipient(&target, &[(None, wallet)], &recipient).unwrap();

        let encrypted = fs::read(&target).unwrap();
        assert!(
            !encrypted
                .windows(b"synthetic".len())
                .any(|part| part == b"synthetic")
        );
        let decrypted = age::decrypt(&identity, &encrypted).unwrap();
        let decrypted = String::from_utf8(decrypted).unwrap();
        assert!(decrypted.contains("mnemonic=synthetic phrase\n"));
        assert!(decrypted.contains("private_key=synthetic-key\n"));
    }

    #[cfg(feature = "sealed-output")]
    #[test]
    fn embedded_age_recipient_encrypts_synthetic_wallet_data() {
        let recipient = embedded_age_recipient().unwrap();
        let ciphertext = encrypt_wallet_contents(b"synthetic wallet secret", &recipient).unwrap();
        assert!(ciphertext.starts_with(b"age-encryption.org/v1\n"));
        assert!(
            !ciphertext
                .windows(b"synthetic wallet secret".len())
                .any(|part| part == b"synthetic wallet secret")
        );
    }

    #[cfg(feature = "sealed-output")]
    #[test]
    fn sealed_build_selects_an_age_file_by_default() {
        let path = selected_output_path(None).unwrap().unwrap();
        assert_eq!(path.extension().and_then(OsStr::to_str), Some("age"));
    }

    #[cfg(all(feature = "sealed-output", not(feature = "sealed-output-gpu")))]
    #[test]
    fn sealed_build_defaults_to_cpu_and_rejects_explicit_gpu_routes() {
        assert_eq!(
            effective_search_backend(GpuBackend::Cpu).unwrap(),
            GpuBackend::Cpu
        );
        assert_eq!(
            effective_search_backend(GpuBackend::Auto).unwrap(),
            GpuBackend::Cpu
        );
        assert_eq!(
            effective_search_backend(GpuBackend::Hybrid).unwrap(),
            GpuBackend::Cpu
        );
        assert!(effective_search_backend(GpuBackend::OpenGl).is_err());
        assert!(enforce_benchmark_security_policy().is_err());
    }
}
