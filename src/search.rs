use crate::gpu::GpuBackend;
#[cfg(feature = "gpu")]
use crate::gpu::{GpuSeedEngine, GpuSeedEngineConfig};
use crate::{
    derivation::{derive_bip39_seed, derive_ton_private_key_from_seed},
    matcher::{MatchMode, Matcher},
    wallet::{Network, WalletVersion, derive_friendly_address_bytes},
};
#[cfg(feature = "gpu")]
use anyhow::Context;
use anyhow::{Result, anyhow};
use bip39::{Language, Mnemonic};
use rand::TryRngCore;
use rayon::prelude::*;
use std::time::{Duration, Instant};

use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use zeroize::{Zeroize, Zeroizing};

const MAX_SEARCH_THREADS: usize = 256;
const MAX_GPU_DEVICES: usize = 16;
#[cfg(feature = "gpu")]
const HYBRID_GPU_BENCHMARK_INDEX_START: u64 = 1 << 63;
#[cfg(feature = "gpu")]
const GPU_DISPATCH_TUNING_INDEX_START: u64 = 1 << 62;

pub fn format_duration_dhms(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}:{:02}.{:03}",
        total_seconds / 86_400,
        (total_seconds / 3600) % 24,
        (total_seconds / 60) % 60,
        total_seconds % 60,
        duration.subsec_millis()
    )
}

#[cfg(feature = "gpu")]
fn format_estimated_duration(candidates: u64, rate: f64) -> String {
    if candidates == u64::MAX || rate <= 0.0 || !rate.is_finite() {
        return "unknown/unbounded".into();
    }
    let seconds = candidates as f64 / rate;
    if !seconds.is_finite() || seconds >= u64::MAX as f64 {
        return "very long".into();
    }
    format_duration_dhms(Duration::from_secs_f64(seconds))
}

#[derive(Clone, Copy)]
enum SearchSource {
    Cpu,
    #[cfg(feature = "gpu")]
    Gpu,
}

#[derive(Default)]
pub struct SearchProgress {
    checked_addresses: AtomicU64,
    cpu_pbkdf2_candidates: AtomicU64,
    gpu_pbkdf2_candidates: AtomicU64,
    last_gpu_dispatch_candidates: AtomicU64,
    last_gpu_dispatch_micros: AtomicU64,
    last_gpu_kernel_candidates: AtomicU64,
    last_gpu_kernel_micros: AtomicU64,
    gpu_result_batches_pending: AtomicU64,
    search_work_started: Mutex<Option<Instant>>,
    gpu_dispatch_started: Mutex<Option<Instant>>,
    gpu_dispatch_candidates: AtomicU64,
    match_accumulator: Mutex<MatchAccumulator>,
}

#[derive(Default)]
struct MatchAccumulator {
    criteria_found: Vec<bool>,
    wallet_criteria: Vec<usize>,
    wallets: Vec<FoundWallet>,
}

impl SearchProgress {
    fn record_matching_wallet(&self, matcher: &Matcher, found: FoundWallet) -> Option<FoundWallet> {
        let matching = matcher.matching_criteria(&found.address);
        if matching.is_empty() {
            return None;
        }
        let mut accumulator = self.match_accumulator.lock().ok()?;
        if accumulator.criteria_found.len() != matcher.criterion_count() {
            accumulator.criteria_found = vec![false; matcher.criterion_count()];
            accumulator.wallet_criteria.clear();
            accumulator.wallets.clear();
        }
        if matcher.mode == MatchMode::Any {
            if !accumulator.wallets.is_empty() {
                return None;
            }
            accumulator.wallet_criteria.push(matching[0]);
            accumulator.wallets.push(found.clone());
            return Some(found);
        }

        // In all mode, each criterion receives its own candidate wallet. If a
        // candidate overlaps criteria, assign it only to the first unmet one.
        let index = matching
            .into_iter()
            .find(|index| !accumulator.criteria_found[*index])?;
        accumulator.criteria_found[index] = true;
        accumulator.wallet_criteria.push(index);
        accumulator.wallets.push(found.clone());
        accumulator
            .criteria_found
            .iter()
            .all(|satisfied| *satisfied)
            .then_some(found)
    }

    pub fn take_found_wallets(&self) -> Vec<FoundWallet> {
        self.take_found_results()
            .into_iter()
            .map(|(_, wallet)| wallet)
            .collect()
    }

    pub fn take_found_results(&self) -> Vec<(usize, FoundWallet)> {
        self.match_accumulator
            .lock()
            .map(|mut accumulator| {
                let criteria = std::mem::take(&mut accumulator.wallet_criteria);
                let wallets = std::mem::take(&mut accumulator.wallets);
                criteria.into_iter().zip(wallets).collect()
            })
            .unwrap_or_default()
    }

    pub fn matched_criteria_count(&self) -> usize {
        self.match_accumulator
            .lock()
            .map(|accumulator| {
                accumulator
                    .criteria_found
                    .iter()
                    .filter(|satisfied| **satisfied)
                    .count()
            })
            .unwrap_or_default()
    }

    pub fn checked_addresses(&self) -> u64 {
        self.checked_addresses.load(Ordering::Acquire)
    }

    pub fn last_gpu_dispatch(&self) -> Option<(u64, u64)> {
        let candidates = self.last_gpu_dispatch_candidates.load(Ordering::Acquire);
        (candidates > 0).then(|| {
            (
                candidates,
                self.last_gpu_dispatch_micros.load(Ordering::Relaxed),
            )
        })
    }

    pub fn last_gpu_kernel(&self) -> Option<(u64, u64)> {
        let candidates = self.last_gpu_kernel_candidates.load(Ordering::Acquire);
        (candidates > 0).then(|| {
            (
                candidates,
                self.last_gpu_kernel_micros.load(Ordering::Relaxed),
            )
        })
    }

    pub fn mark_search_work_started(&self) {
        if let Ok(mut started) = self.search_work_started.lock()
            && started.is_none()
        {
            *started = Some(Instant::now());
        }
    }

    pub fn search_work_started_at(&self) -> Option<Instant> {
        self.search_work_started.lock().ok()?.as_ref().copied()
    }

    pub fn search_elapsed(&self) -> Option<Duration> {
        self.search_work_started_at()
            .map(|started| started.elapsed())
    }

    pub fn pbkdf2_candidates_by_source(&self) -> (u64, u64) {
        (
            self.cpu_pbkdf2_candidates.load(Ordering::Acquire),
            self.gpu_pbkdf2_candidates.load(Ordering::Acquire),
        )
    }

    fn address_checked(&self, source: SearchSource) {
        self.address_checked_batch(source, 1);
    }

    fn address_checked_batch(&self, source: SearchSource, candidates: u64) {
        self.checked_addresses
            .fetch_add(candidates, Ordering::Relaxed);
        match source {
            SearchSource::Cpu => {
                self.cpu_pbkdf2_candidates
                    .fetch_add(candidates, Ordering::Relaxed);
            }
            #[cfg(feature = "gpu")]
            SearchSource::Gpu => {
                self.gpu_pbkdf2_candidates
                    .fetch_add(candidates, Ordering::Relaxed);
            }
        }
    }

    #[cfg(any(feature = "gpu", feature = "cuda"))]
    fn begin_gpu_dispatch(&self, candidates: usize) {
        self.gpu_dispatch_candidates
            .store(candidates as u64, Ordering::Release);
        if let Ok(mut started) = self.gpu_dispatch_started.lock() {
            *started = Some(Instant::now());
        }
    }

    pub fn current_gpu_dispatch_candidates(&self) -> u64 {
        self.gpu_dispatch_candidates.load(Ordering::Acquire)
    }

    pub fn current_gpu_dispatch_elapsed(&self) -> Option<Duration> {
        self.gpu_dispatch_started
            .lock()
            .ok()?
            .as_ref()
            .map(Instant::elapsed)
    }

    pub fn current_gpu_result_batches_pending(&self) -> u64 {
        self.gpu_result_batches_pending.load(Ordering::Acquire)
    }

    #[cfg(feature = "gpu")]
    fn begin_gpu_result_batch(&self) {
        self.gpu_result_batches_pending
            .fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(feature = "gpu")]
    fn finish_gpu_result_batch(&self) {
        self.gpu_result_batches_pending
            .fetch_sub(1, Ordering::AcqRel);
    }

    #[cfg(any(feature = "gpu", feature = "cuda"))]
    fn record_gpu_dispatch(
        &self,
        candidates: usize,
        elapsed_micros: u64,
        kernel_micros: Option<u64>,
    ) {
        self.last_gpu_dispatch_micros
            .store(elapsed_micros, Ordering::Relaxed);
        self.last_gpu_dispatch_candidates
            .store(candidates as u64, Ordering::Release);
        if let Some(kernel_micros) = kernel_micros {
            self.last_gpu_kernel_micros
                .store(kernel_micros, Ordering::Relaxed);
            self.last_gpu_kernel_candidates
                .store(candidates as u64, Ordering::Release);
        } else {
            self.last_gpu_kernel_candidates.store(0, Ordering::Release);
        }
        if let Ok(mut started) = self.gpu_dispatch_started.lock() {
            *started = None;
        }
    }
}

const CPU_PROGRESS_BATCH: u64 = 64;

struct CpuProgressBatch<'a> {
    progress: &'a SearchProgress,
    pending: u64,
}

impl<'a> CpuProgressBatch<'a> {
    fn new(progress: &'a SearchProgress) -> Self {
        Self {
            progress,
            pending: 0,
        }
    }

    fn record_checked(&mut self) {
        self.pending += 1;
        if self.pending >= CPU_PROGRESS_BATCH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.pending > 0 {
            self.progress
                .address_checked_batch(SearchSource::Cpu, self.pending);
            self.pending = 0;
        }
    }
}

impl Drop for CpuProgressBatch<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

pub struct SearchConfig {
    pub wallet: WalletVersion,
    pub network: Network,
    pub bounceable: bool,
    pub matcher: Matcher,
    pub threads: usize,
    pub gpu_devices: usize,
    pub account: u32,
    pub passphrase: Zeroizing<String>,
    pub gpu_backend: GpuBackend,
    pub gpu_batch: usize,
    pub max_candidates: Option<u64>,
    pub cancellation: Arc<AtomicBool>,
}

impl SearchConfig {
    pub fn validate(&self) -> Result<()> {
        self.matcher.validate_for_output_format(
            self.bounceable,
            self.network == Network::Testnet,
            0,
        )?;
        if self.account > 0x7fff_ffff {
            anyhow::bail!("account index must be <= 2147483647");
        }
        if self.wallet == WalletVersion::WalletTg {
            if self.account != 0 {
                anyhow::bail!(
                    "WalletTg uses the fixed account path m/44'/607'/0'; --account must be 0"
                );
            }
            if !self.passphrase.is_empty() {
                anyhow::bail!(
                    "WalletTg uses an empty BIP39 passphrase; omit --prompt-passphrase or leave it blank"
                );
            }
        }
        if self.wallet == WalletVersion::WalletTg && self.network != Network::Mainnet {
            anyhow::bail!("WalletTg address derivation currently supports Telegram mainnet only");
        }
        if self.wallet == WalletVersion::V5R1 && self.account >= (1 << 15) {
            anyhow::bail!("W5 subwallet number must be < 32768");
        }
        if self.threads > MAX_SEARCH_THREADS {
            anyhow::bail!("thread count must be <= {MAX_SEARCH_THREADS}");
        }
        if self.gpu_devices == 0 || self.gpu_devices > MAX_GPU_DEVICES {
            anyhow::bail!("GPU device count must be between 1 and {MAX_GPU_DEVICES}");
        }
        // gpu_batch == 0 means to use the selected device's derived limit.
        if self.max_candidates == Some(0) {
            anyhow::bail!("maximum candidate count must be greater than zero");
        }
        if !self.matcher.exact.is_empty() && self.max_candidates.is_none() {
            anyhow::bail!(
                "exact-address search must specify --max-candidates; a random exact-address match is computationally infeasible"
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct FoundWallet {
    pub mnemonic: String,
    pub address: String,
    pub private_key_hex: String,
}

impl Drop for FoundWallet {
    fn drop(&mut self) {
        self.mnemonic.zeroize();
        self.private_key_hex.zeroize();
    }
}

#[derive(Debug)]
struct SeedIntegrityError;

impl fmt::Display for SeedIntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "GPU-derived BIP39 seed did not match the mnemonic; refusing to return a wallet",
        )
    }
}

impl Error for SeedIntegrityError {}

#[derive(Debug)]
struct SearchLimitError(u64);

impl fmt::Display for SearchLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "candidate limit ({}) reached without a match", self.0)
    }
}

impl Error for SearchLimitError {}

#[derive(Debug)]
struct SearchCancelledError;

impl fmt::Display for SearchCancelledError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("search cancelled by user")
    }
}

impl Error for SearchCancelledError {}

#[cfg(feature = "gpu")]
fn calibration_error_is_cancellation(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SearchCancelledError>().is_some()
}

#[cfg(feature = "gpu")]
fn calibration_error_is_seed_integrity(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SeedIntegrityError>().is_some()
}

fn ensure_not_cancelled(cfg: &SearchConfig) -> Result<()> {
    if cfg.cancellation.load(Ordering::Acquire) {
        Err(SearchCancelledError.into())
    } else {
        Ok(())
    }
}
fn limit_error(cfg: &SearchConfig) -> anyhow::Error {
    match cfg.max_candidates {
        Some(maximum) => SearchLimitError(maximum).into(),
        None => anyhow!("candidate counter exhausted"),
    }
}

fn reserve_candidate(counter: &AtomicU64, maximum: Option<u64>) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
            if current == u64::MAX || maximum.is_some_and(|limit| current >= limit) {
                None
            } else {
                Some(current + 1)
            }
        })
        .is_ok()
}

fn release_candidate(counter: &AtomicU64) {
    let released = counter.try_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
        current.checked_sub(1)
    });
    debug_assert!(
        released.is_ok(),
        "candidate reservation was not outstanding"
    );
}

#[cfg(feature = "gpu")]
fn reserve_batch_len(cfg: &SearchConfig, counter: &AtomicU64, requested: usize) -> Result<usize> {
    let requested = requested as u64;
    let maximum = cfg.max_candidates;
    let previous = counter
        .try_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
            let available_by_counter = u64::MAX - current;
            let available_by_limit = maximum
                .map(|limit| limit.saturating_sub(current))
                .unwrap_or(u64::MAX);
            let reserved = requested.min(available_by_counter).min(available_by_limit);
            (reserved > 0).then(|| current + reserved)
        })
        .map_err(|_| limit_error(cfg))?;

    let available_by_counter = u64::MAX - previous;
    let available_by_limit = maximum
        .map(|limit| limit.saturating_sub(previous))
        .unwrap_or(u64::MAX);
    let reserved = requested.min(available_by_counter).min(available_by_limit);
    usize::try_from(reserved).map_err(|_| anyhow!("reserved GPU batch does not fit usize"))
}

#[cfg(feature = "gpu")]
fn release_batch_len(counter: &AtomicU64, batch_len: usize) {
    let reserved = batch_len as u64;
    let released = counter.try_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
        current.checked_sub(reserved)
    });
    debug_assert!(
        released.is_ok(),
        "GPU candidate reservation was not outstanding"
    );
}

#[cfg(feature = "gpu")]
struct GpuBatchReservation<'a> {
    counter: &'a AtomicU64,
    remaining: usize,
}

#[cfg(feature = "gpu")]
impl<'a> GpuBatchReservation<'a> {
    fn new(counter: &'a AtomicU64, reserved: usize) -> Self {
        Self {
            counter,
            remaining: reserved,
        }
    }

    fn consume(&mut self, count: usize) {
        debug_assert!(count <= self.remaining);
        self.remaining -= count;
    }
}

#[cfg(feature = "gpu")]
impl Drop for GpuBatchReservation<'_> {
    fn drop(&mut self) {
        if self.remaining > 0 {
            release_batch_len(self.counter, self.remaining);
            self.remaining = 0;
        }
    }
}

fn make_candidate() -> Result<Mnemonic> {
    let mut entropy = Zeroizing::new([0u8; 16]);
    rand::rng()
        .try_fill_bytes(&mut entropy[..])
        .map_err(|error| anyhow!("secure entropy generation failed: {error}"))?;
    Mnemonic::from_entropy_in(Language::English, &entropy[..])
        .map_err(|error| anyhow!("failed to create a BIP39 mnemonic from entropy: {error}"))
}

#[cfg(feature = "gpu")]
fn make_candidate_batch(count: usize, cancellation: &AtomicBool) -> Result<Vec<Mnemonic>> {
    let mut batch = Vec::with_capacity(count);
    for _ in 0..count {
        if cancellation.load(Ordering::Acquire) {
            return Err(SearchCancelledError.into());
        }
        batch.push(make_candidate()?);
    }
    Ok(batch)
}

fn check_seed(
    cfg: &SearchConfig,
    mnemonic: &Mnemonic,
    seed: &[u8; 64],
) -> Result<Option<FoundWallet>> {
    let key = derive_ton_private_key_from_seed(seed, cfg.account)?;
    let address = derive_friendly_address_bytes(
        cfg.wallet,
        &key,
        cfg.network,
        0,
        if cfg.wallet == WalletVersion::V5R1 {
            cfg.account
        } else {
            0
        },
        cfg.bounceable,
    )?;
    if !cfg.matcher.matches_bytes(&address) {
        return Ok(None);
    }

    // Verify the exact candidate that would be returned. This preserves the GPU
    // speed path while ensuring a faulty kernel can never yield a mismatched wallet.
    let reference_seed = derive_bip39_seed(mnemonic, &cfg.passphrase);
    if reference_seed[..] != seed[..] {
        return Err(SeedIntegrityError.into());
    }
    let address = std::str::from_utf8(&address)
        .map_err(|_| anyhow!("friendly address encoder returned invalid UTF-8"))?
        .to_owned();
    Ok(Some(FoundWallet {
        mnemonic: mnemonic.to_string(),
        address,
        private_key_hex: hex::encode(&key[..]),
    }))
}

#[cfg(feature = "gpu")]
fn check_seed_counted(
    cfg: &SearchConfig,
    mnemonic: &Mnemonic,
    seed: &[u8; 64],
    progress: &SearchProgress,
    source: SearchSource,
) -> Result<Option<FoundWallet>> {
    let result = check_seed(cfg, mnemonic, seed)?;
    progress.address_checked(source);
    Ok(result.and_then(|found| progress.record_matching_wallet(&cfg.matcher, found)))
}

fn matcher_matches_all_addresses_from_header(cfg: &SearchConfig) -> bool {
    // The first two base64url characters encode only the friendly-address tag
    // and workchain. For workchain 0 they are fixed for the selected network and
    // bounceability, independent of the wallet key/hash.
    let sample = crate::address::friendly_address(
        0,
        [0u8; 32],
        cfg.bounceable,
        cfg.network == Network::Testnet,
    );
    match cfg.matcher.mode {
        MatchMode::All => {
            !cfg.matcher.prefix.is_empty()
                && cfg.matcher.suffix.is_empty()
                && cfg.matcher.exact.is_empty()
                && cfg.matcher.prefix.iter().all(|prefix| prefix.len() <= 2)
                && cfg.matcher.prefix.iter().all(|prefix| {
                    sample.get(..prefix.len()).is_some_and(|head| {
                        if cfg.matcher.case_sensitive {
                            head == prefix
                        } else {
                            head.eq_ignore_ascii_case(prefix)
                        }
                    })
                })
        }
        MatchMode::Any => cfg.matcher.prefix.iter().any(|prefix| {
            prefix.len() <= 2
                && sample.get(..prefix.len()).is_some_and(|head| {
                    if cfg.matcher.case_sensitive {
                        head == prefix
                    } else {
                        head.eq_ignore_ascii_case(prefix)
                    }
                })
        }),
    }
}

fn search_one_header_match(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
) -> Result<FoundWallet> {
    progress.mark_search_work_started();
    loop {
        ensure_not_cancelled(cfg)?;
        if !reserve_candidate(counter, cfg.max_candidates) {
            return Err(limit_error(cfg));
        }
        let mnemonic = match make_candidate() {
            Ok(mnemonic) => mnemonic,
            Err(error) => {
                release_candidate(counter);
                return Err(error);
            }
        };
        let seed = derive_bip39_seed(&mnemonic, &cfg.passphrase);
        let found = check_seed(cfg, &mnemonic, &seed)?
            .ok_or_else(|| anyhow!("TON address-header prefix invariant did not hold"))?;
        progress.address_checked(SearchSource::Cpu);
        if let Some(complete) = progress.record_matching_wallet(&cfg.matcher, found) {
            return Ok(complete);
        }
    }
}
fn configured_search_threads(cfg: &SearchConfig) -> usize {
    if cfg.threads == 0 {
        rayon::current_num_threads().clamp(1, MAX_SEARCH_THREADS)
    } else {
        cfg.threads
    }
}

fn build_search_pool(cfg: &SearchConfig) -> Result<rayon::ThreadPool> {
    build_search_pool_with_threads(configured_search_threads(cfg))
}

fn build_search_pool_with_threads(threads: usize) -> Result<rayon::ThreadPool> {
    Ok(rayon::ThreadPoolBuilder::new()
        .num_threads(threads.clamp(1, MAX_SEARCH_THREADS))
        .build()?)
}

#[cfg(feature = "gpu")]
fn check_seed_batch_in_order(
    cfg: &SearchConfig,
    mnemonics: &[Mnemonic],
    seeds: &[[u8; 64]],
    progress: &SearchProgress,
    pool: Option<&rayon::ThreadPool>,
) -> Result<Option<FoundWallet>> {
    if mnemonics.len() != seeds.len() {
        anyhow::bail!(
            "GPU returned {} seeds for {} candidate mnemonics",
            seeds.len(),
            mnemonics.len()
        );
    }
    if let Some(pool) = pool {
        if cfg.matcher.mode == MatchMode::Any {
            let found = pool.install(|| {
                mnemonics
                    .par_iter()
                    .zip(seeds.par_iter())
                    .find_map_first(|(mnemonic, seed)| {
                        if cfg.cancellation.load(Ordering::Acquire) {
                            return Some(Err(anyhow::Error::new(SearchCancelledError)));
                        }
                        match check_seed(cfg, mnemonic, seed) {
                            Ok(found) => {
                                progress.address_checked(SearchSource::Gpu);
                                found.map(Ok)
                            }
                            Err(error) => Some(Err(error)),
                        }
                    })
            });
            let found = found.transpose()?;
            return Ok(
                found.and_then(|wallet| progress.record_matching_wallet(&cfg.matcher, wallet))
            );
        }

        // Preserve parallel CPU address derivation for ALL mode, then apply
        // results in candidate order so one wallet can satisfy several
        // criteria and completion is deterministic within each dispatch.
        let checked = pool.install(|| {
            mnemonics
                .par_iter()
                .zip(seeds.par_iter())
                .map(|(mnemonic, seed)| {
                    if cfg.cancellation.load(Ordering::Acquire) {
                        return Err(anyhow::Error::new(SearchCancelledError));
                    }
                    let result = check_seed(cfg, mnemonic, seed);
                    progress.address_checked(SearchSource::Gpu);
                    result
                })
                .collect::<Vec<_>>()
        });
        for candidate in checked {
            if let Some(wallet) = candidate?
                && let Some(complete) = progress.record_matching_wallet(&cfg.matcher, wallet)
            {
                return Ok(Some(complete));
            }
        }
        return Ok(None);
    }

    for (mnemonic, seed) in mnemonics.iter().zip(seeds.iter()) {
        if cfg.cancellation.load(Ordering::Acquire) {
            return Err(SearchCancelledError.into());
        }
        if let Some(found) = check_seed_counted(cfg, mnemonic, seed, progress, SearchSource::Gpu)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

#[cfg(feature = "gpu")]
fn join_gpu_result_task(
    task: std::thread::ScopedJoinHandle<'_, Result<Option<FoundWallet>>>,
) -> Result<Option<FoundWallet>> {
    task.join()
        .map_err(|_| anyhow!("GPU-result CPU worker panicked"))?
}

pub fn search_cpu(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
) -> Result<FoundWallet> {
    search_cpu_with_threads(cfg, counter, progress, configured_search_threads(cfg))
}

fn search_cpu_with_threads(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
    worker_count: usize,
) -> Result<FoundWallet> {
    cfg.validate()?;
    ensure_not_cancelled(cfg)?;
    if matcher_matches_all_addresses_from_header(cfg) {
        return search_one_header_match(cfg, counter, progress);
    }
    let threads = worker_count.clamp(1, MAX_SEARCH_THREADS);
    let pool = if threads == configured_search_threads(cfg) {
        build_search_pool(cfg)?
    } else {
        build_search_pool_with_threads(threads)?
    };
    progress.mark_search_work_started();
    let stop = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None::<Result<FoundWallet>>));

    pool.install(|| {
        (0usize..threads).into_par_iter().for_each(|_| {
            let mut local_progress = CpuProgressBatch::new(progress);
            while !stop.load(Ordering::Acquire) {
                if cfg.cancellation.load(Ordering::Acquire) {
                    if let Ok(mut slot) = result.lock()
                        && slot.is_none()
                    {
                        *slot = Some(Err(SearchCancelledError.into()));
                    }
                    stop.store(true, Ordering::Release);
                    break;
                }
                if !reserve_candidate(counter, cfg.max_candidates) {
                    if let Ok(mut slot) = result.lock()
                        && slot.is_none()
                    {
                        *slot = Some(Err(limit_error(cfg)));
                    }
                    stop.store(true, Ordering::Release);
                    break;
                }
                let mnemonic = match make_candidate() {
                    Ok(mnemonic) => mnemonic,
                    Err(error) => {
                        release_candidate(counter);
                        if let Ok(mut slot) = result.lock() {
                            let may_replace_limit = match slot.as_ref() {
                                None => true,
                                Some(Err(current)) => {
                                    current.downcast_ref::<SearchLimitError>().is_some()
                                }
                                Some(Ok(_)) => false,
                            };
                            if may_replace_limit {
                                *slot = Some(Err(error));
                            }
                        }
                        stop.store(true, Ordering::Release);
                        break;
                    }
                };
                let seed = derive_bip39_seed(&mnemonic, &cfg.passphrase);
                match check_seed(cfg, &mnemonic, &seed) {
                    Ok(found) => {
                        local_progress.record_checked();
                        if let Some(found) = found
                            .and_then(|found| progress.record_matching_wallet(&cfg.matcher, found))
                        {
                            if let Ok(mut slot) = result.lock() {
                                let may_replace_limit = match slot.as_ref() {
                                    None => true,
                                    Some(Err(error)) => {
                                        error.downcast_ref::<SearchLimitError>().is_some()
                                    }
                                    Some(Ok(_)) => false,
                                };
                                if may_replace_limit {
                                    *slot = Some(Ok(found));
                                }
                            }
                            stop.store(true, Ordering::Release);
                            break;
                        }
                    }
                    Err(error) => {
                        if let Ok(mut slot) = result.lock() {
                            let may_replace_limit = match slot.as_ref() {
                                None => true,
                                Some(Err(current)) => {
                                    current.downcast_ref::<SearchLimitError>().is_some()
                                }
                                Some(Ok(_)) => false,
                            };
                            if may_replace_limit {
                                *slot = Some(Err(error));
                            }
                        }
                        stop.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });
    });

    result
        .lock()
        .map_err(|_| anyhow!("search result lock poisoned"))?
        .take()
        .ok_or_else(|| anyhow!("search stopped without a result"))?
}

#[cfg(feature = "gpu")]
fn gpu_passphrase_supported(passphrase: &str) -> bool {
    if passphrase.is_ascii() {
        passphrase.len() <= 99
    } else {
        crate::derivation::normalize_bip39_passphrase(passphrase).len() <= 99
    }
}

#[cfg(feature = "gpu")]
fn log_after_clearing_live_status(arguments: fmt::Arguments<'_>) {
    use std::io::{IsTerminal, Write};

    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    if std::io::stderr().is_terminal() {
        let _ = write!(stderr, "\r\x1b[2K");
    }
    let _ = writeln!(stderr, "{arguments}");
}

#[cfg(feature = "gpu")]
pub fn search_gpu(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
) -> Result<FoundWallet> {
    search_gpu_with_backend(cfg, counter, progress, cfg.gpu_backend, None, false, 0)
}

#[cfg(feature = "gpu")]
fn search_gpu_with_backend(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
    backend: GpuBackend,
    dispatch_override: Option<usize>,
    hybrid_lane: bool,
    hybrid_gpu_result_workers: usize,
) -> Result<FoundWallet> {
    cfg.validate()?;
    ensure_not_cancelled(cfg)?;
    if !gpu_passphrase_supported(&cfg.passphrase) {
        eprintln!(
            "BIP39 passphrase exceeds GPU PBKDF2's 99-byte normalized salt capacity; continuing with CPU search for this run"
        );
        return search_cpu(cfg, counter, progress);
    }
    #[cfg(not(feature = "cuda"))]
    if backend == GpuBackend::Cuda {
        anyhow::bail!("CUDA backend not compiled; rebuild with --features cuda");
    }

    let mut engine = AutoGpuEngine::new(cfg, backend)?;
    let device_batch_limit = engine.max_batch_size();
    let configured_batch_limit = if cfg.gpu_batch == 0 {
        device_batch_limit
    } else {
        cfg.gpu_batch.min(device_batch_limit)
    };
    let tuner_maximum = cfg
        .max_candidates
        .map(|maximum| {
            let lane_maximum = if hybrid_lane {
                maximum.saturating_div(2).max(1)
            } else {
                maximum
            };
            configured_batch_limit.min(usize::try_from(lane_maximum).unwrap_or(usize::MAX))
        })
        .unwrap_or(configured_batch_limit);
    let dispatch_batch_size = if let Some(size) = dispatch_override {
        engine.set_dispatch_batch_size(size)?;
        size
    } else {
        safe_tune_gpu_dispatch_size(cfg, backend, &mut engine, tuner_maximum)?
    };
    if hybrid_lane {
        log_after_clearing_live_status(format_args!(
            "selected_gpu_backend={} mode=hybrid dispatch_batch={dispatch_batch_size}",
            engine.backend_name()
        ));
    } else {
        log_after_clearing_live_status(format_args!(
            "selected_gpu_backend={} dispatch_batch={dispatch_batch_size}",
            engine.backend_name()
        ));
    }
    // GPU PBKDF2 leaves key/address derivation on the host. GPU-only searches
    // use the configured CPU pool; Hybrid uses an independently tuned result
    // pool in addition to the full CPU search pool.
    let check_pool = if hybrid_lane {
        if hybrid_gpu_result_workers > 0 {
            Some(build_search_pool_with_threads(hybrid_gpu_result_workers)?)
        } else {
            None
        }
    } else {
        Some(build_search_pool(cfg)?)
    };
    progress.mark_search_work_started();
    if cfg.gpu_devices > 1 {
        return search_gpu_on_multiple_devices(MultiDeviceSearch {
            cfg,
            counter,
            progress,
            backend,
            first_engine: engine,
            dispatch_batch_size,
            hybrid_lane,
            check_pool: check_pool.as_ref(),
        });
    }
    search_gpu_engine_loop(
        cfg,
        counter,
        progress,
        engine,
        dispatch_batch_size,
        hybrid_lane,
        check_pool.as_ref(),
    )
}

#[cfg(feature = "gpu")]
struct MultiDeviceSearch<'a> {
    cfg: &'a SearchConfig,
    counter: &'a AtomicU64,
    progress: &'a SearchProgress,
    backend: GpuBackend,
    first_engine: AutoGpuEngine,
    dispatch_batch_size: usize,
    hybrid_lane: bool,
    check_pool: Option<&'a rayon::ThreadPool>,
}

#[cfg(feature = "gpu")]
fn search_gpu_on_multiple_devices(request: MultiDeviceSearch<'_>) -> Result<FoundWallet> {
    let MultiDeviceSearch {
        cfg,
        counter,
        progress,
        backend,
        first_engine,
        dispatch_batch_size,
        hybrid_lane,
        check_pool,
    } = request;
    let available = AutoGpuEngine::available_device_count(backend)?;
    let maximum_workers = selected_gpu_worker_count(cfg.gpu_devices, available);
    if available < cfg.gpu_devices {
        eprintln!(
            "gpu_devices requested={} available={} for {}; verifying up to {} GPU worker(s)",
            cfg.gpu_devices, available, backend, maximum_workers
        );
    }
    let mut engines = Vec::with_capacity(maximum_workers);
    engines.push(first_engine);
    engines.extend(verify_additional_gpu_devices(
        cfg,
        backend,
        dispatch_batch_size,
        available,
    )?);
    let worker_count = engines.len();
    log_after_clearing_live_status(format_args!(
        "selected_gpu_backend={backend} mode={} gpu_workers={worker_count} dispatch_batch={dispatch_batch_size}",
        if hybrid_lane { "hybrid" } else { "gpu" }
    ));

    let (sender, receiver) = std::sync::mpsc::channel::<Result<FoundWallet>>();
    std::thread::scope(|scope| {
        for (worker_index, engine) in engines.into_iter().enumerate() {
            let sender = sender.clone();
            scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    search_gpu_engine_loop(
                        cfg,
                        counter,
                        progress,
                        engine,
                        dispatch_batch_size,
                        hybrid_lane,
                        check_pool,
                    )
                }))
                .unwrap_or_else(|_| Err(anyhow!("{backend} GPU worker {worker_index} panicked")));
                if result.is_ok() {
                    cfg.cancellation.store(true, Ordering::Release);
                }
                let _ = sender.send(result);
            });
        }
        drop(sender);
        collect_gpu_worker_results(cfg, receiver)
    })
}

#[cfg(feature = "gpu")]
fn verify_additional_gpu_devices(
    cfg: &SearchConfig,
    backend: GpuBackend,
    dispatch_batch_size: usize,
    available: usize,
) -> Result<Vec<AutoGpuEngine>> {
    if cfg.gpu_devices <= 1 {
        return Ok(Vec::new());
    }
    let maximum_workers = selected_gpu_worker_count(cfg.gpu_devices, available);
    let mut engines = Vec::with_capacity(maximum_workers.saturating_sub(1));
    for device_index in 1..maximum_workers {
        ensure_not_cancelled(cfg)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut engine = AutoGpuEngine::new_for_device(cfg, backend, device_index)
                .with_context(|| format!("initializing {backend} GPU device {device_index}"))?;
            engine.set_dispatch_batch_size(dispatch_batch_size)?;
            let parity_batch =
                gpu_dispatch_tuning_batch(dispatch_batch_size, &cfg.cancellation)?;
            let seeds = engine.derive(&parity_batch, &cfg.passphrase, &cfg.cancellation)?;
            validate_benchmark_seed_samples(cfg, &parity_batch, &seeds).with_context(|| {
                format!(
                    "GPU device {device_index} failed parity at dispatch size {dispatch_batch_size}"
                )
            })?;
            Ok::<_, anyhow::Error>(engine)
        }))
        .unwrap_or_else(|_| {
            Err(anyhow!("{backend} GPU device {device_index} panicked during initialization or parity verification"))
        });
        match result {
            Ok(engine) => {
                eprintln!(
                    "gpu_device_verification=passed backend={backend} device_index={device_index} dispatch_batch={dispatch_batch_size}"
                );
                engines.push(engine);
            }
            Err(error) if calibration_error_is_cancellation(&error) => return Err(error),
            Err(error) => eprintln!(
                "gpu_device_verification=failed backend={backend} device_index={device_index}; skipping device ({error:#})"
            ),
        }
    }
    let verified_workers = 1 + engines.len();
    if verified_workers < cfg.gpu_devices {
        eprintln!(
            "gpu_devices requested={} verified={} for {}; continuing with verified devices only",
            cfg.gpu_devices, verified_workers, backend
        );
    }
    Ok(engines)
}

#[cfg(feature = "gpu")]
fn selected_gpu_worker_count(requested: usize, available: usize) -> usize {
    requested.min(available).max(1)
}

#[cfg(feature = "gpu")]
fn search_gpu_engine_loop(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
    engine: AutoGpuEngine,
    dispatch_batch_size: usize,
    hybrid_lane: bool,
    check_pool: Option<&rayon::ThreadPool>,
) -> Result<FoundWallet> {
    std::thread::scope(|scope| {
        let mut pending_result = None;
        loop {
            ensure_not_cancelled(cfg)?;
            let requested_reservation = if hybrid_lane {
                cfg.max_candidates
                    .map(|maximum| {
                        dispatch_batch_size.min(
                            usize::try_from(maximum.saturating_div(2).max(1)).unwrap_or(usize::MAX),
                        )
                    })
                    .unwrap_or(dispatch_batch_size)
            } else {
                dispatch_batch_size
            };
            let batch_len = match reserve_batch_len(cfg, counter, requested_reservation) {
                Ok(batch_len) => batch_len,
                Err(limit_error) => {
                    if let Some(found) = match pending_result.take() {
                        Some(task) => join_gpu_result_task(task)?,
                        None => None,
                    } {
                        return Ok(found);
                    }
                    return Err(limit_error);
                }
            };
            let mut reservation = GpuBatchReservation::new(counter, batch_len);
            let mut remaining = batch_len;
            while remaining > 0 {
                ensure_not_cancelled(cfg)?;
                let chunk_len = remaining.min(dispatch_batch_size);
                let batch = make_candidate_batch(chunk_len, &cfg.cancellation)?;
                progress.begin_gpu_dispatch(batch.len());
                let gpu_started = Instant::now();
                let seeds_result = engine.derive(&batch, &cfg.passphrase, &cfg.cancellation);
                progress.record_gpu_dispatch(
                    batch.len(),
                    gpu_started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                    engine.last_kernel_micros(),
                );
                let seeds = match seeds_result {
                    Ok(seeds) => seeds,
                    Err(error) => {
                        if let Some(task) = pending_result.take()
                            && let Some(found) = join_gpu_result_task(task)?
                        {
                            return Ok(found);
                        }
                        ensure_not_cancelled(cfg)?;
                        return Err(error);
                    }
                };

                // The GPU dispatch is synchronous through readback. Once its
                // seeds are available, process this batch on the dedicated CPU
                // pool while the next PBKDF2 dispatch runs on the GPU.
                reservation.consume(chunk_len);
                remaining -= chunk_len;
                if let Some(task) = pending_result.take()
                    && let Some(found) = join_gpu_result_task(task)?
                {
                    return Ok(found);
                }

                progress.begin_gpu_result_batch();
                pending_result = Some(scope.spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        check_seed_batch_in_order(cfg, &batch, &seeds, progress, check_pool)
                    }))
                    .unwrap_or_else(|_| Err(anyhow!("GPU-result CPU worker panicked")));
                    progress.finish_gpu_result_batch();
                    result
                }));
            }
        }
    })
}

#[cfg(feature = "gpu")]
fn collect_gpu_worker_results(
    cfg: &SearchConfig,
    receiver: std::sync::mpsc::Receiver<Result<FoundWallet>>,
) -> Result<FoundWallet> {
    let mut found = None;
    let mut failures = Vec::new();
    for result in receiver {
        match result {
            Ok(wallet) => {
                if found.is_none() {
                    found = Some(wallet);
                }
            }
            Err(error) => failures.push(error),
        }
    }
    if let Some(wallet) = found {
        return Ok(wallet);
    }
    if cfg.cancellation.load(Ordering::Acquire) {
        return Err(SearchCancelledError.into());
    }
    if let Some(limit_index) = failures
        .iter()
        .position(|error| error.downcast_ref::<SearchLimitError>().is_some())
    {
        return Err(failures.remove(limit_index));
    }
    Err(failures
        .into_iter()
        .next()
        .unwrap_or_else(|| anyhow!("multi-GPU search stopped without a result")))
}

#[cfg(feature = "gpu")]
fn hybrid_gpu_backend_order(preferred: Option<GpuBackend>) -> Vec<GpuBackend> {
    let mut order = vec![
        GpuBackend::Vulkan,
        GpuBackend::Dx12,
        GpuBackend::Dx11,
        GpuBackend::Metal,
        GpuBackend::OpenCl,
        GpuBackend::OpenGl,
    ];
    if let Some(preferred) = preferred {
        order.retain(|backend| *backend != preferred);
        order.insert(0, preferred);
    }
    order
}
#[cfg(feature = "gpu")]
fn search_hybrid_gpu_lane_with_order(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
    backends: &[GpuBackend],
    preferred_dispatch: Option<(GpuBackend, usize)>,
    schedule: HybridSchedule,
) -> Result<FoundWallet> {
    let mut last_error = None;
    for backend in backends {
        if cfg.cancellation.load(Ordering::Acquire) {
            return Err(SearchCancelledError.into());
        }
        let dispatch_override = preferred_dispatch
            .filter(|(preferred, _)| preferred == backend)
            .map(|(_, size)| size);
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            search_gpu_with_backend(
                cfg,
                counter,
                progress,
                *backend,
                dispatch_override,
                true,
                schedule.gpu_result_workers,
            )
        }));
        let attempt = match attempt {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "GPU backend {backend} panicked during Hybrid search"
            )),
        };
        match attempt {
            Ok(found) => return Ok(found),
            Err(error)
                if error.downcast_ref::<SearchCancelledError>().is_some()
                    || error.downcast_ref::<SearchLimitError>().is_some()
                    || error.downcast_ref::<SeedIntegrityError>().is_some() =>
            {
                return Err(error);
            }
            Err(error) => {
                eprintln!(
                    "hybrid {backend} worker unavailable ({error:#}); trying next GPU backend"
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no GPU backend is available for hybrid search")))
}

#[cfg(not(feature = "gpu"))]
pub fn search_gpu(
    _cfg: &SearchConfig,
    _counter: &AtomicU64,
    _progress: &SearchProgress,
) -> Result<FoundWallet> {
    anyhow::bail!("GPU feature not compiled")
}

#[cfg(feature = "gpu")]
#[derive(Clone, Copy, Debug)]
struct HybridSchedule {
    cpu_workers: usize,
    gpu_result_workers: usize,
}

#[cfg(feature = "gpu")]
fn hybrid_schedule(cfg: &SearchConfig) -> HybridSchedule {
    hybrid_schedules(cfg)[0]
}

#[cfg(feature = "gpu")]
fn hybrid_schedules(cfg: &SearchConfig) -> Vec<HybridSchedule> {
    hybrid_schedules_for_workers(configured_search_threads(cfg))
}

#[cfg(feature = "gpu")]
fn hybrid_schedules_for_workers(cpu_workers: usize) -> Vec<HybridSchedule> {
    use std::collections::BTreeSet;

    let cpu_workers = cpu_workers.clamp(1, MAX_SEARCH_THREADS);
    let mut result_worker_counts = BTreeSet::new();
    if cpu_workers <= 16 {
        result_worker_counts.extend(1..=cpu_workers);
    } else {
        // Probe the full range for small CPUs. For larger machines, use a
        // logarithmic sweep plus balanced fractions so calibration time grows
        // with log(thread count), not with every possible pool size.
        let mut count = 1usize;
        while count < cpu_workers {
            result_worker_counts.insert(count);
            count = count.saturating_mul(2);
        }
        result_worker_counts.insert(cpu_workers);
        for numerator in [1usize, 2, 3] {
            result_worker_counts.insert(cpu_workers.saturating_mul(numerator).div_ceil(4));
        }
    }

    result_worker_counts
        .into_iter()
        .map(|gpu_result_workers| HybridSchedule {
            // The CPU search always retains its complete configured worker
            // pool. GPU-result workers are additional CPU workers and are
            // tuned separately; GPU PBKDF2 runs on the device itself.
            cpu_workers,
            gpu_result_workers,
        })
        .collect()
}

#[cfg(feature = "gpu")]
#[derive(Clone, Copy, Debug)]
enum AutoRoute {
    Cpu,
    Gpu(GpuBackend, usize),
    Hybrid(GpuBackend, usize, HybridSchedule),
}

#[cfg(feature = "gpu")]
fn benchmark_wallet_seed(cfg: &SearchConfig, seed: &[u8; 64]) -> Result<()> {
    ensure_not_cancelled(cfg)?;
    let key = derive_ton_private_key_from_seed(seed, cfg.account)?;
    let address = derive_friendly_address_bytes(
        cfg.wallet,
        &key,
        cfg.network,
        0,
        if cfg.wallet == WalletVersion::V5R1 {
            cfg.account
        } else {
            0
        },
        cfg.bounceable,
    )?;
    let matches = cfg.matcher.matches_bytes(&address);
    std::hint::black_box((address, matches));
    Ok(())
}

#[cfg(feature = "gpu")]
fn benchmark_wallet_stage(cfg: &SearchConfig, seeds: &[[u8; 64]]) -> Result<()> {
    for seed in seeds {
        benchmark_wallet_seed(cfg, seed)?;
    }
    Ok(())
}

#[cfg(feature = "gpu")]
fn benchmark_wallet_stage_parallel(
    cfg: &SearchConfig,
    pool: &rayon::ThreadPool,
    seeds: &[[u8; 64]],
) -> Result<()> {
    pool.install(|| {
        seeds
            .par_iter()
            .try_for_each(|seed| benchmark_wallet_seed(cfg, seed))
    })
}

#[cfg(feature = "gpu")]
fn benchmark_wallet_stage_with_pool(
    cfg: &SearchConfig,
    seeds: &[[u8; 64]],
    pool: Option<&rayon::ThreadPool>,
) -> Result<()> {
    if let Some(pool) = pool {
        benchmark_wallet_stage_parallel(cfg, pool, seeds)
    } else {
        benchmark_wallet_stage(cfg, seeds)
    }
}

#[cfg(feature = "gpu")]
fn benchmark_mnemonic(candidate_index: u64) -> Result<Mnemonic> {
    // A deterministic public corpus keeps backend comparisons on identical
    // BIP39 inputs while still varying mnemonic lengths and word indices.
    let mut state = candidate_index ^ 0x544f_4e5f_4245_4e43;
    let mut entropy = [0u8; 16];
    let (chunks, remainder) = entropy.as_chunks_mut::<8>();
    debug_assert!(remainder.is_empty());
    for chunk in chunks {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        chunk.copy_from_slice(&mixed.to_le_bytes());
    }
    Mnemonic::from_entropy_in(Language::English, &entropy)
        .map_err(|error| anyhow!("failed to create deterministic benchmark mnemonic: {error}"))
}

#[cfg(feature = "gpu")]
fn benchmark_candidate_batch(
    start_index: &mut u64,
    count: usize,
    cancellation: &AtomicBool,
) -> Result<Vec<Mnemonic>> {
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(count)
        .map_err(|error| anyhow!("cannot allocate {count} benchmark candidates: {error}"))?;
    for _ in 0..count {
        if cancellation.load(Ordering::Acquire) {
            return Err(SearchCancelledError.into());
        }
        candidates.push(benchmark_mnemonic(*start_index)?);
        *start_index = start_index
            .checked_add(1)
            .ok_or_else(|| anyhow!("benchmark candidate index overflowed"))?;
    }
    Ok(candidates)
}

#[cfg(feature = "gpu")]
const GPU_DISPATCH_TUNING_CORPUS_SIZE: usize = 257;

#[cfg(feature = "gpu")]
fn gpu_dispatch_tuning_batch(count: usize, cancellation: &AtomicBool) -> Result<Vec<Mnemonic>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let corpus_len = count.min(GPU_DISPATCH_TUNING_CORPUS_SIZE);
    let mut candidate_index = GPU_DISPATCH_TUNING_INDEX_START;
    let corpus = benchmark_candidate_batch(&mut candidate_index, corpus_len, cancellation)?;
    let mut batch = Vec::new();
    batch
        .try_reserve_exact(count)
        .map_err(|error| anyhow!("cannot allocate {count} GPU tuning candidates: {error}"))?;
    batch.extend((0..count).map(|index| corpus[index % corpus_len].clone()));
    Ok(batch)
}

#[cfg(feature = "gpu")]
fn benchmark_cpu_candidate(cfg: &SearchConfig, candidate_index: u64) -> Result<()> {
    ensure_not_cancelled(cfg)?;
    let mnemonic = benchmark_mnemonic(candidate_index)?;
    let seed = derive_bip39_seed(&mnemonic, &cfg.passphrase);
    benchmark_wallet_seed(cfg, &seed)
}

#[cfg(feature = "gpu")]
fn benchmark_cpu_batch(
    cfg: &SearchConfig,
    pool: &rayon::ThreadPool,
    candidate_count: usize,
) -> Result<()> {
    pool.install(|| {
        (0..candidate_count)
            .into_par_iter()
            .try_for_each(|candidate_index| benchmark_cpu_candidate(cfg, candidate_index as u64))
    })
}

#[cfg(feature = "gpu")]
const ROUTE_BENCHMARK_WINDOW: Duration = Duration::from_millis(1500);
#[cfg(feature = "gpu")]
const ROUTE_BENCHMARK_MIN_GPU_BATCHES: usize = 2;

#[cfg(feature = "gpu")]
fn benchmark_cpu_workers_until(
    cfg: &SearchConfig,
    pool: &rayon::ThreadPool,
    worker_count: usize,
    first_candidate_index: u64,
    deadline: Option<Instant>,
    stop: &AtomicBool,
) -> Result<u64> {
    let completed = AtomicU64::new(0);
    let next_candidate = AtomicU64::new(first_candidate_index);
    pool.install(|| {
        (0..worker_count)
            .into_par_iter()
            .try_for_each(|_| -> Result<()> {
                let mut local_completed = 0u64;
                while !stop.load(Ordering::Acquire)
                    && deadline.is_none_or(|deadline| Instant::now() < deadline)
                {
                    let candidate_index = next_candidate.fetch_add(1, Ordering::Relaxed);
                    benchmark_cpu_candidate(cfg, candidate_index)?;
                    local_completed = local_completed.saturating_add(1);
                }
                completed.fetch_add(local_completed, Ordering::Relaxed);
                Ok(())
            })
    })?;
    Ok(completed.load(Ordering::Acquire))
}

#[cfg(feature = "gpu")]
fn benchmark_cpu_for_window(
    cfg: &SearchConfig,
    pool: &rayon::ThreadPool,
) -> Result<(u64, Duration)> {
    benchmark_cpu_batch(cfg, pool, 32)?;
    let started = Instant::now();
    let deadline = started + ROUTE_BENCHMARK_WINDOW;
    let stop = AtomicBool::new(false);
    let candidates = benchmark_cpu_workers_until(
        cfg,
        pool,
        configured_search_threads(cfg),
        0,
        Some(deadline),
        &stop,
    )?;
    Ok((candidates, started.elapsed()))
}

#[cfg(feature = "gpu")]
enum AutoGpuEngine {
    Wgpu(GpuSeedEngine),
    #[cfg(feature = "opencl")]
    OpenCl(Box<crate::gpu::opencl::OpenClSeedEngine>),
    #[cfg(all(feature = "d3d11", target_os = "windows"))]
    Dx11(crate::gpu::d3d11::D3d11SeedEngine),
    #[cfg(feature = "cuda")]
    Cuda {
        batch_limit: usize,
    },
}

#[cfg(feature = "gpu")]
impl AutoGpuEngine {
    fn new(cfg: &SearchConfig, backend: GpuBackend) -> Result<Self> {
        Self::new_for_device(cfg, backend, 0)
    }

    fn available_device_count(backend: GpuBackend) -> Result<usize> {
        #[cfg(feature = "opencl")]
        if backend == GpuBackend::OpenCl {
            return crate::gpu::opencl::device_count();
        }
        #[cfg(all(feature = "d3d11", target_os = "windows"))]
        if backend == GpuBackend::Dx11 {
            // The native DX11 engine currently binds the default adapter.
            return Ok(1);
        }
        #[cfg(feature = "cuda")]
        if backend == GpuBackend::Cuda {
            // CUDA engine currently uses the runtime's default device.
            return Ok(1);
        }
        GpuSeedEngine::available_adapter_count(backend)
    }

    fn new_for_device(
        cfg: &SearchConfig,
        backend: GpuBackend,
        device_index: usize,
    ) -> Result<Self> {
        #[cfg(feature = "cuda")]
        if backend == GpuBackend::Cuda {
            if device_index != 0 {
                anyhow::bail!("CUDA multi-device selection is not implemented yet");
            }
            if crate::gpu::cuda::device_count() <= 0 {
                anyhow::bail!("no CUDA device is available");
            }
            let batch_limit = crate::gpu::cuda::max_batch_size()?;
            crate::gpu::cuda::self_test()?;
            return Ok(Self::Cuda { batch_limit });
        }
        #[cfg(not(feature = "cuda"))]
        if backend == GpuBackend::Cuda {
            anyhow::bail!("CUDA backend not compiled; rebuild with --features cuda");
        }
        #[cfg(all(feature = "d3d11", target_os = "windows"))]
        if backend == GpuBackend::Dx11 {
            if device_index != 0 {
                anyhow::bail!("DX11 multi-device selection is not implemented yet");
            }
            let engine = crate::gpu::d3d11::D3d11SeedEngine::new(cfg.gpu_batch)?;
            engine.self_test()?;
            return Ok(Self::Dx11(engine));
        }
        #[cfg(not(all(feature = "d3d11", target_os = "windows")))]
        if backend == GpuBackend::Dx11 {
            anyhow::bail!("Direct3D 11 is only available on Windows with the d3d11 feature");
        }
        #[cfg(feature = "opencl")]
        if backend == GpuBackend::OpenCl {
            let engine =
                crate::gpu::opencl::OpenClSeedEngine::new_for_device(cfg.gpu_batch, device_index)?;
            engine.self_test()?;
            return Ok(Self::OpenCl(Box::new(engine)));
        }
        #[cfg(not(feature = "opencl"))]
        if backend == GpuBackend::OpenCl {
            anyhow::bail!("OpenCL backend not compiled");
        }

        let engine = GpuSeedEngine::new_for_device(
            &GpuSeedEngineConfig {
                backend,
                batch_size: cfg.gpu_batch,
            },
            device_index,
        )?;
        engine.self_test()?;
        Ok(Self::Wgpu(engine))
    }

    fn backend_name(&self) -> String {
        match self {
            Self::Wgpu(engine) => format!(
                "{} adapter={}",
                engine.backend(),
                engine.adapter_description()
            ),
            #[cfg(feature = "opencl")]
            Self::OpenCl(engine) => format!("OpenCL device={}", engine.name()),
            #[cfg(all(feature = "d3d11", target_os = "windows"))]
            Self::Dx11(engine) => format!("Dx11 device={}", engine.device_name()),
            #[cfg(feature = "cuda")]
            Self::Cuda { .. } => "CUDA".into(),
        }
    }

    fn max_batch_size(&self) -> usize {
        match self {
            Self::Wgpu(engine) => engine.max_batch_size(),
            #[cfg(feature = "opencl")]
            Self::OpenCl(engine) => engine.max_batch_size(),
            #[cfg(all(feature = "d3d11", target_os = "windows"))]
            Self::Dx11(engine) => engine.max_batch_size(),
            #[cfg(feature = "cuda")]
            Self::Cuda { batch_limit } => *batch_limit,
        }
    }

    fn set_dispatch_batch_size(&mut self, size: usize) -> Result<()> {
        match self {
            Self::Wgpu(engine) => engine.set_dispatch_batch_size(size),
            #[cfg(feature = "opencl")]
            Self::OpenCl(engine) => engine.set_dispatch_batch_size(size),
            #[cfg(all(feature = "d3d11", target_os = "windows"))]
            Self::Dx11(engine) => engine.set_dispatch_batch_size(size),
            #[cfg(feature = "cuda")]
            Self::Cuda { batch_limit } => {
                if size == 0 || size > *batch_limit {
                    anyhow::bail!(
                        "CUDA dispatch exceeds the current free-memory-derived limit of {batch_limit}"
                    );
                }
                Ok(())
            }
        }
    }

    fn last_kernel_micros(&self) -> Option<u64> {
        match self {
            Self::Wgpu(_) => None,
            #[cfg(feature = "opencl")]
            Self::OpenCl(engine) => engine.last_kernel_micros(),
            #[cfg(all(feature = "d3d11", target_os = "windows"))]
            Self::Dx11(_) => None,
            #[cfg(feature = "cuda")]
            Self::Cuda { .. } => None,
        }
    }

    fn derive(
        &self,
        mnemonics: &[Mnemonic],
        passphrase: &str,
        cancellation: &AtomicBool,
    ) -> Result<Zeroizing<Vec<[u8; 64]>>> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match self {
            Self::Wgpu(engine) => {
                engine.derive_batch_cancellable(mnemonics, passphrase, cancellation)
            }
            #[cfg(feature = "opencl")]
            Self::OpenCl(engine) => {
                engine.derive_batch_cancellable(mnemonics, passphrase, cancellation)
            }
            #[cfg(all(feature = "d3d11", target_os = "windows"))]
            Self::Dx11(engine) => {
                engine.derive_batch_cancellable(mnemonics, passphrase, Some(cancellation))
            }
            #[cfg(feature = "cuda")]
            Self::Cuda { .. } => crate::gpu::cuda::derive_batch(mnemonics, passphrase),
        }))
        .unwrap_or_else(|_| {
            anyhow::bail!(
                "GPU backend {} panicked during PBKDF2 derivation",
                self.backend_name()
            )
        })
    }
}

/// Benchmarks the requested GPU backend, or all GPU backends when `only_backend` is `None.
/// Uses deterministic public BIP39 inputs and reports the fastest measured backend and dispatch size.
pub fn benchmark_gpu_backends(cfg: &SearchConfig, only_backend: Option<GpuBackend>) -> Result<()> {
    #[cfg(feature = "gpu")]
    {
        cfg.validate()?;
        if let Some(backend @ (GpuBackend::Cpu | GpuBackend::Auto | GpuBackend::Hybrid)) =
            only_backend
        {
            anyhow::bail!("{backend} is not a GPU backend that can be benchmarked");
        }
        if !gpu_passphrase_supported(&cfg.passphrase) {
            anyhow::bail!(
                "the supplied BIP39 passphrase exceeds the GPU PBKDF2 salt limit; benchmark with a shorter passphrase"
            );
        }

        let backends = if let Some(backend) = only_backend {
            vec![backend]
        } else {
            vec![
                GpuBackend::Vulkan,
                GpuBackend::Dx12,
                GpuBackend::Dx11,
                GpuBackend::Metal,
                GpuBackend::OpenCl,
                GpuBackend::OpenGl,
            ]
        };
        #[cfg(feature = "cuda")]
        let backends = {
            let mut backends = backends;
            if only_backend.is_none() {
                backends.push(GpuBackend::Cuda);
            }
            backends
        };

        println!(
            "GPU benchmark uses deterministic public BIP39 candidates; no random wallet candidates or secrets are generated"
        );
        let pool = build_search_pool(cfg)?;
        let mut best: Option<(GpuBackend, String, usize, f64)> = None;

        for backend in backends {
            let setup_started = Instant::now();
            let initialized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                AutoGpuEngine::new(cfg, backend)
            }));
            let mut engine = match initialized {
                Ok(Ok(engine)) => engine,
                Ok(Err(error)) => {
                    println!("gpu_benchmark {backend}: unavailable ({error:#})");
                    continue;
                }
                Err(_) => {
                    println!("gpu_benchmark {backend}: initialization panicked; skipped");
                    continue;
                }
            };
            println!(
                "gpu_benchmark {backend}: initialized in {:.2}s engine={}",
                setup_started.elapsed().as_secs_f64(),
                engine.backend_name()
            );

            let dispatch_size = match safe_tune_gpu_dispatch_size(
                cfg,
                backend,
                &mut engine,
                cfg.gpu_batch,
            ) {
                Ok(size) => size,
                Err(error) if calibration_error_is_cancellation(&error) => {
                    return Err(error);
                }
                Err(error) if calibration_error_is_seed_integrity(&error) => {
                    println!(
                        "gpu_benchmark {backend}: PBKDF2 parity failed during dispatch tuning; backend rejected ({error:#})"
                    );
                    if only_backend.is_some() {
                        return Err(error);
                    }
                    continue;
                }
                Err(error) => {
                    println!("gpu_benchmark {backend}: tuning failed ({error:#})");
                    continue;
                }
            };
            let mnemonics = gpu_dispatch_tuning_batch(dispatch_size, &cfg.cancellation)?;
            match safe_fixed_gpu_benchmark(cfg, backend, &engine, &pool, &mnemonics) {
                Ok((candidates, elapsed)) => {
                    let rate = candidates as f64 / elapsed.as_secs_f64().max(0.000_001);
                    let device = engine.backend_name();
                    println!(
                        "gpu_benchmark {backend}: engine={device} dispatch={dispatch_size} rate={rate:.1} candidates/s"
                    );
                    if best
                        .as_ref()
                        .is_none_or(|(_, _, _, best_rate)| rate > *best_rate)
                    {
                        best = Some((backend, device, dispatch_size, rate));
                    }
                }
                Err(error) if calibration_error_is_cancellation(&error) => {
                    return Err(error);
                }
                Err(error) if calibration_error_is_seed_integrity(&error) => {
                    println!(
                        "gpu_benchmark {backend}: PBKDF2 parity failed; backend rejected ({error:#})"
                    );
                    if only_backend.is_some() {
                        return Err(error);
                    }
                }
                Err(error) => println!("gpu_benchmark {backend}: run failed ({error:#})"),
            }
        }

        if let Some((backend, device, dispatch_size, rate)) = best {
            println!(
                "gpu_benchmark_best backend={backend} engine={device} dispatch={dispatch_size} rate={rate:.1} candidates/s"
            );
        } else {
            println!("gpu_benchmark_best=none (no usable GPU backend found)");
        }
        Ok(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (cfg, only_backend);
        anyhow::bail!("GPU support was not compiled; rebuild with the gpu feature");
    }
}

#[cfg(feature = "gpu")]
fn tune_gpu_dispatch_size(
    cfg: &SearchConfig,
    engine: &mut AutoGpuEngine,
    requested_max: usize,
) -> Result<usize> {
    const TARGET_MAX_DISPATCH: Duration = Duration::from_millis(1500);
    let device_limit = engine.max_batch_size().max(1);
    let maximum = if requested_max == 0 {
        device_limit
    } else {
        requested_max.min(device_limit).max(1)
    };
    let sizes = gpu_dispatch_candidates(maximum);

    let mut best_size = 1usize;
    let mut best_rate = 0.0f64;
    let mut has_verified_size = false;
    let mut previous_measurement = None;
    let mut last_measurement = None;
    let mut measurements = Vec::<(usize, Duration, f64)>::new();
    eprintln!(
        "gpu_dispatch_tuning=started device_limit={device_limit} max_candidates={maximum} target_call_ms=1500"
    );

    for size in sizes {
        ensure_not_cancelled(cfg)?;
        if let (Some(previous), Some(last)) = (previous_measurement, last_measurement)
            && let Some(projected_ms) = projected_dispatch_latency_ms(previous, last, size)
            && projected_ms > TARGET_MAX_DISPATCH.as_secs_f64() * 1000.0
        {
            eprintln!(
                "gpu_dispatch_tuning=stopped projected_latency_exceeded next_candidates={size} projected_host_ms={projected_ms:.2}"
            );
            break;
        }
        engine.set_dispatch_batch_size(size)?;
        // Use distinct deterministic inputs so a backend that repeats or
        // permutes one output across lanes cannot pass parity just because
        // every tuning candidate had the same mnemonic.
        let batch = gpu_dispatch_tuning_batch(size, &cfg.cancellation)?;
        let started = Instant::now();
        let result = engine.derive(&batch, &cfg.passphrase, &cfg.cancellation);
        let elapsed = started.elapsed();
        let seeds = match result {
            Ok(seeds) => seeds,
            Err(error) if error.downcast_ref::<SearchCancelledError>().is_some() => {
                return Err(error);
            }
            Err(error) => {
                if !has_verified_size {
                    return Err(error).context(
                        "GPU dispatch tuning failed before any dispatch size was verified",
                    );
                }
                eprintln!(
                    "gpu_dispatch_tuning=size:{size} failed; recreating backend and rechecking size {best_size}: {error:#}"
                );
                reinitialize_and_verify_dispatch(
                    cfg,
                    backend_for_engine(engine),
                    engine,
                    best_size,
                )?;
                break;
            }
        };
        if let Err(error) = validate_benchmark_seed_samples(cfg, &batch, &seeds) {
            let detail = format!("dispatch size {size} failed PBKDF2 parity ({error:#})");
            if has_verified_size {
                eprintln!(
                    "gpu_dispatch_tuning=size:{size} failed PBKDF2 parity ({detail}); recreating backend and rechecking size {best_size} at {best_rate:.1}/s"
                );
                reinitialize_and_verify_dispatch(
                    cfg,
                    backend_for_engine(engine),
                    engine,
                    best_size,
                )?;
                break;
            }
            return Err(error.context(detail));
        }
        let elapsed_seconds = elapsed.as_secs_f64().max(0.000_001);
        let rate = size as f64 / elapsed_seconds;
        eprintln!(
            "gpu_dispatch_tuning candidates={size} host_ms={:.2} rate={rate:.1}/s",
            elapsed.as_secs_f64() * 1000.0
        );
        measurements.push((size, elapsed, rate));
        previous_measurement = last_measurement;
        last_measurement = Some((size, elapsed));
        if !has_verified_size || (elapsed <= TARGET_MAX_DISPATCH && rate > best_rate) {
            best_rate = rate;
            best_size = size;
        }
        has_verified_size = true;
        if elapsed > TARGET_MAX_DISPATCH {
            eprintln!("gpu_dispatch_tuning=stopped latency_target_exceeded");
            break;
        }
    }

    let recovery_size = best_size;
    let confirmation_sizes = gpu_dispatch_confirmation_candidates(&measurements, recovery_size);
    let has_confirmation_candidates = !confirmation_sizes.is_empty();
    if has_confirmation_candidates {
        eprintln!(
            "gpu_dispatch_confirmation=started candidates={}",
            confirmation_sizes
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    let mut best_confirmed_under_target: Option<(usize, f64)> = None;
    let mut best_confirmed_any: Option<(usize, f64)> = None;
    for size in confirmation_sizes {
        let Some((_, initial_elapsed, initial_rate)) = measurements
            .iter()
            .find(|measurement| measurement.0 == size)
            .copied()
        else {
            continue;
        };
        let mut rates = vec![initial_rate];
        let mut latencies = vec![initial_elapsed];
        let mut candidate_verified = true;
        for _ in 0..2 {
            ensure_not_cancelled(cfg)?;
            engine.set_dispatch_batch_size(size)?;
            let batch = gpu_dispatch_tuning_batch(size, &cfg.cancellation)?;
            let started = Instant::now();
            let result = engine.derive(&batch, &cfg.passphrase, &cfg.cancellation);
            let elapsed = started.elapsed();
            let seeds = match result {
                Ok(seeds) => seeds,
                Err(error) if error.downcast_ref::<SearchCancelledError>().is_some() => {
                    return Err(error);
                }
                Err(error) => {
                    eprintln!(
                        "gpu_dispatch_confirmation candidates={size} failed; recovering dispatch {recovery_size}: {error:#}"
                    );
                    reinitialize_and_verify_dispatch(
                        cfg,
                        backend_for_engine(engine),
                        engine,
                        recovery_size,
                    )?;
                    candidate_verified = false;
                    break;
                }
            };
            if let Err(error) = validate_benchmark_seed_samples(cfg, &batch, &seeds) {
                eprintln!(
                    "gpu_dispatch_confirmation candidates={size} failed PBKDF2 parity; recovering dispatch {recovery_size}: {error:#}"
                );
                reinitialize_and_verify_dispatch(
                    cfg,
                    backend_for_engine(engine),
                    engine,
                    recovery_size,
                )?;
                candidate_verified = false;
                break;
            }
            rates.push(size as f64 / elapsed.as_secs_f64().max(0.000_001));
            latencies.push(elapsed);
        }
        if !candidate_verified || rates.len() != 3 {
            continue;
        }
        let confirmed_rate = median_rate(&rates).expect("three dispatch timing samples");
        let confirmed_latency = median_duration(&latencies).expect("three dispatch latencies");
        eprintln!(
            "gpu_dispatch_confirmation candidates={size} median_rate={confirmed_rate:.1}/s median_host_ms={:.2}",
            confirmed_latency.as_secs_f64() * 1000.0
        );
        if best_confirmed_any.is_none_or(|(_, rate)| confirmed_rate > rate) {
            best_confirmed_any = Some((size, confirmed_rate));
        }
        if confirmed_latency <= TARGET_MAX_DISPATCH
            && best_confirmed_under_target.is_none_or(|(_, rate)| confirmed_rate > rate)
        {
            best_confirmed_under_target = Some((size, confirmed_rate));
        }
    }
    if let Some((size, rate)) = best_confirmed_under_target.or(best_confirmed_any) {
        best_size = size;
        best_rate = rate;
    } else if has_confirmation_candidates {
        eprintln!(
            "gpu_dispatch_confirmation=unavailable; retaining initially verified dispatch {recovery_size}"
        );
    }
    engine.set_dispatch_batch_size(best_size)?;
    eprintln!(
        "gpu_dispatch_tuning=selected candidates={best_size} confirmed_rate={best_rate:.1}/s"
    );
    Ok(best_size)
}

#[cfg(feature = "gpu")]
fn projected_dispatch_latency_ms(
    previous: (usize, Duration),
    last: (usize, Duration),
    next_size: usize,
) -> Option<f64> {
    let (previous_size, previous_elapsed) = previous;
    let (last_size, last_elapsed) = last;
    if previous_size >= last_size || last_size >= next_size {
        return None;
    }

    let measured_size_delta = (last_size - previous_size) as f64;
    let next_size_delta = (next_size - last_size) as f64;
    let measured_ms_delta = (last_elapsed.as_secs_f64() - previous_elapsed.as_secs_f64()) * 1000.0;
    let marginal_ms_per_candidate = (measured_ms_delta / measured_size_delta).max(0.0);
    Some(last_elapsed.as_secs_f64() * 1000.0 + marginal_ms_per_candidate * next_size_delta)
}

#[cfg(feature = "gpu")]
fn backend_for_engine(engine: &AutoGpuEngine) -> GpuBackend {
    match engine {
        AutoGpuEngine::Wgpu(engine) => engine.backend(),
        #[cfg(feature = "opencl")]
        AutoGpuEngine::OpenCl(_) => GpuBackend::OpenCl,
        #[cfg(all(feature = "d3d11", target_os = "windows"))]
        AutoGpuEngine::Dx11(_) => GpuBackend::Dx11,
        #[cfg(feature = "cuda")]
        AutoGpuEngine::Cuda => GpuBackend::Cuda,
    }
}

#[cfg(feature = "gpu")]
fn reinitialize_and_verify_dispatch(
    cfg: &SearchConfig,
    backend: GpuBackend,
    engine: &mut AutoGpuEngine,
    dispatch_size: usize,
) -> Result<()> {
    ensure_not_cancelled(cfg)?;
    let mut replacement = AutoGpuEngine::new(cfg, backend).with_context(|| {
        format!("could not recreate GPU backend {backend} after a failed probe")
    })?;
    replacement.set_dispatch_batch_size(dispatch_size)?;
    let batch = gpu_dispatch_tuning_batch(dispatch_size, &cfg.cancellation)?;
    let seeds = replacement
        .derive(&batch, &cfg.passphrase, &cfg.cancellation)
        .with_context(|| {
            format!("GPU backend {backend} recovery probe at size {dispatch_size} failed")
        })?;
    validate_benchmark_seed_samples(cfg, &batch, &seeds).with_context(|| {
        format!(
            "GPU backend {backend} recovery probe failed parity at dispatch size {dispatch_size}"
        )
    })?;
    *engine = replacement;
    eprintln!("gpu_dispatch_recovery=passed backend={backend} verified_candidates={dispatch_size}");
    Ok(())
}

#[cfg(feature = "gpu")]
fn safe_tune_gpu_dispatch_size(
    cfg: &SearchConfig,
    backend: GpuBackend,
    engine: &mut AutoGpuEngine,
    requested_max: usize,
) -> Result<usize> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tune_gpu_dispatch_size(cfg, engine, requested_max)
    }))
    .unwrap_or_else(|_| anyhow::bail!("GPU backend {backend} panicked during dispatch tuning"))
}

#[cfg(feature = "gpu")]
fn gpu_dispatch_candidates(maximum: usize) -> Vec<usize> {
    let maximum = maximum.max(1);
    let mut sizes = Vec::new();
    let mut size = 1usize;
    loop {
        sizes.push(size);
        if size == maximum {
            break;
        }
        size = size.saturating_mul(2).min(maximum);
    }
    sizes
}

#[cfg(feature = "gpu")]
fn gpu_dispatch_confirmation_candidates(
    measurements: &[(usize, Duration, f64)],
    best_size: usize,
) -> Vec<usize> {
    let mut ranked = measurements.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.2.total_cmp(&left.2));

    let mut selected = std::collections::BTreeSet::new();
    selected.extend(ranked.into_iter().take(3).map(|measurement| measurement.0));
    if let Some(best_index) = measurements
        .iter()
        .position(|measurement| measurement.0 == best_size)
    {
        if best_index > 0 {
            selected.insert(measurements[best_index - 1].0);
        }
        if best_index + 1 < measurements.len() {
            selected.insert(measurements[best_index + 1].0);
        }
    }
    selected.into_iter().collect()
}

#[cfg(feature = "gpu")]
fn median_rate(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
}

#[cfg(feature = "gpu")]
fn median_duration(samples: &[Duration]) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
}

#[cfg(feature = "gpu")]
fn benchmark_gpu_batch(
    cfg: &SearchConfig,
    engine: &AutoGpuEngine,
    mnemonics: &[Mnemonic],
) -> Result<(Zeroizing<Vec<[u8; 64]>>, Duration)> {
    ensure_not_cancelled(cfg)?;
    let started = Instant::now();
    let seeds = engine.derive(mnemonics, &cfg.passphrase, &cfg.cancellation)?;
    Ok((seeds, started.elapsed()))
}
#[cfg(feature = "gpu")]
fn validate_benchmark_seeds(
    cfg: &SearchConfig,
    mnemonics: &[Mnemonic],
    seeds: &[[u8; 64]],
) -> Result<()> {
    if mnemonics.len() != seeds.len() {
        return Err(anyhow::Error::new(SeedIntegrityError).context(format!(
            "backend benchmark returned {} seeds for {} candidates",
            seeds.len(),
            mnemonics.len()
        )));
    }
    // Mnemonics in search benchmarks are usually distinct. A Vec lookup here
    // made final parity validation quadratic in dispatch size (65k candidates
    // meant billions of equality checks).
    let mut expected_by_mnemonic =
        std::collections::HashMap::<Mnemonic, Zeroizing<[u8; 64]>>::new();
    for (candidate_index, (mnemonic, actual)) in mnemonics.iter().zip(seeds).enumerate() {
        let expected = expected_by_mnemonic
            .entry(mnemonic.clone())
            .or_insert_with(|| derive_bip39_seed(mnemonic, &cfg.passphrase));
        if expected[..] != actual[..] {
            return Err(anyhow::Error::new(SeedIntegrityError).context(format!(
                "benchmark candidate {candidate_index} failed CPU/GPU seed parity"
            )));
        }
    }
    Ok(())
}

#[cfg(feature = "gpu")]
struct BenchmarkParitySample {
    candidate_index: usize,
    mnemonic: Mnemonic,
    seed: Zeroizing<[u8; 64]>,
}

#[cfg(feature = "gpu")]
fn benchmark_parity_sample_indices(candidate_count: usize) -> Vec<usize> {
    const EVENLY_SPACED_SAMPLES: usize = 16;
    if candidate_count <= EVENLY_SPACED_SAMPLES {
        return (0..candidate_count).collect();
    }

    let mut indices = std::collections::BTreeSet::new();
    for sample in 0..EVENLY_SPACED_SAMPLES {
        indices.insert(sample * (candidate_count - 1) / (EVENLY_SPACED_SAMPLES - 1));
    }
    // Exercise lanes immediately around the dispatch boundaries as well as
    // the regularly spaced interior samples.
    indices.insert(1);
    indices.insert(candidate_count - 2);
    indices.into_iter().collect()
}

#[cfg(feature = "gpu")]
fn benchmark_parity_samples(
    mnemonics: &[Mnemonic],
    seeds: &[[u8; 64]],
) -> Result<Vec<BenchmarkParitySample>> {
    if mnemonics.len() != seeds.len() {
        return Err(anyhow::Error::new(SeedIntegrityError).context(format!(
            "backend benchmark returned {} seeds for {} candidates",
            seeds.len(),
            mnemonics.len()
        )));
    }
    Ok(benchmark_parity_sample_indices(mnemonics.len())
        .into_iter()
        .map(|candidate_index| BenchmarkParitySample {
            candidate_index,
            mnemonic: mnemonics[candidate_index].clone(),
            seed: Zeroizing::new(seeds[candidate_index]),
        })
        .collect())
}

#[cfg(feature = "gpu")]
fn validate_benchmark_parity_samples(
    cfg: &SearchConfig,
    samples: &[BenchmarkParitySample],
) -> Result<()> {
    for sample in samples {
        let expected = derive_bip39_seed(&sample.mnemonic, &cfg.passphrase);
        if expected[..] != sample.seed[..] {
            return Err(anyhow::Error::new(SeedIntegrityError).context(format!(
                "benchmark candidate {} failed CPU/GPU seed parity",
                sample.candidate_index
            )));
        }
    }
    Ok(())
}

#[cfg(feature = "gpu")]
fn validate_benchmark_seed_samples(
    cfg: &SearchConfig,
    mnemonics: &[Mnemonic],
    seeds: &[[u8; 64]],
) -> Result<()> {
    let samples = benchmark_parity_samples(mnemonics, seeds)?;
    validate_benchmark_parity_samples(cfg, &samples)
}

#[cfg(feature = "gpu")]
fn benchmark_gpu_for_window(
    cfg: &SearchConfig,
    engine: &AutoGpuEngine,
    pool: &rayon::ThreadPool,
    batch_len: usize,
) -> Result<(u64, Duration)> {
    let mut warmup_index = u64::MAX.saturating_sub(batch_len as u64);
    let warmup_mnemonics =
        benchmark_candidate_batch(&mut warmup_index, batch_len, &cfg.cancellation)?;
    let (seeds, _) = benchmark_gpu_batch(cfg, engine, &warmup_mnemonics)?;
    validate_benchmark_seed_samples(cfg, &warmup_mnemonics, &seeds)
        .context("GPU-only benchmark warmup")?;
    benchmark_wallet_stage_parallel(cfg, pool, &seeds)?;
    let started = Instant::now();
    let mut candidates = 0u64;
    let mut completed_batches = 0usize;
    let mut last_batch = None;
    let mut next_candidate = 0u64;
    while started.elapsed() < ROUTE_BENCHMARK_WINDOW
        || completed_batches < ROUTE_BENCHMARK_MIN_GPU_BATCHES
    {
        ensure_not_cancelled(cfg)?;
        let mnemonics =
            benchmark_candidate_batch(&mut next_candidate, batch_len, &cfg.cancellation)?;
        let (seeds, _) = benchmark_gpu_batch(cfg, engine, &mnemonics)?;
        if seeds.len() != mnemonics.len() {
            anyhow::bail!(
                "GPU-only benchmark returned {} seeds for {} candidates",
                seeds.len(),
                mnemonics.len()
            );
        }
        benchmark_wallet_stage_parallel(cfg, pool, &seeds)?;
        let batch_candidates = mnemonics.len() as u64;
        last_batch = Some((mnemonics, seeds));
        candidates = candidates.saturating_add(batch_candidates);
        completed_batches += 1;
    }
    let elapsed = started.elapsed();
    let (last_mnemonics, last_seeds) =
        last_batch.ok_or_else(|| anyhow!("GPU benchmark did no timed work"))?;
    validate_benchmark_seed_samples(cfg, &last_mnemonics, &last_seeds)
        .context("GPU-only benchmark final parity check")?;
    Ok((candidates, elapsed))
}

#[cfg(feature = "gpu")]
fn benchmark_fixed_gpu_for_window(
    cfg: &SearchConfig,
    engine: &AutoGpuEngine,
    pool: &rayon::ThreadPool,
    mnemonics: &[Mnemonic],
) -> Result<(u64, Duration)> {
    let (seeds, _) = benchmark_gpu_batch(cfg, engine, mnemonics)?;
    validate_benchmark_seeds(cfg, mnemonics, &seeds).context("GPU benchmark warmup")?;
    benchmark_wallet_stage_parallel(cfg, pool, &seeds)?;
    let started = Instant::now();
    let mut candidates = 0u64;
    let mut last_seeds = None;
    while started.elapsed() < ROUTE_BENCHMARK_WINDOW {
        ensure_not_cancelled(cfg)?;
        let (seeds, _) = benchmark_gpu_batch(cfg, engine, mnemonics)?;
        if seeds.len() != mnemonics.len() {
            anyhow::bail!(
                "GPU benchmark returned {} seeds for {} candidates",
                seeds.len(),
                mnemonics.len()
            );
        }
        benchmark_wallet_stage_parallel(cfg, pool, &seeds)?;
        last_seeds = Some(seeds);
        candidates = candidates.saturating_add(mnemonics.len() as u64);
    }
    let elapsed = started.elapsed();
    let last_seeds = last_seeds.ok_or_else(|| anyhow!("GPU benchmark did no timed work"))?;
    validate_benchmark_seeds(cfg, mnemonics, &last_seeds)
        .context("GPU benchmark final parity check")?;
    Ok((candidates, elapsed))
}

#[cfg(feature = "gpu")]
fn benchmark_hybrid_for_window(
    cfg: &SearchConfig,
    engine: &AutoGpuEngine,
    cpu_pool: &rayon::ThreadPool,
    gpu_result_pool: Option<&rayon::ThreadPool>,
    schedule: HybridSchedule,
    gpu_batch_len: usize,
    warmup_seeds: &[[u8; 64]],
) -> Result<(u64, u64, Duration)> {
    benchmark_cpu_batch(cfg, cpu_pool, 32)?;
    if warmup_seeds.len() != gpu_batch_len {
        anyhow::bail!(
            "Hybrid benchmark warmup returned {} seeds for {gpu_batch_len} candidates",
            warmup_seeds.len()
        );
    }
    benchmark_wallet_stage_with_pool(cfg, warmup_seeds, gpu_result_pool)?;

    let started = Instant::now();
    let stop_cpu = AtomicBool::new(false);
    let (cpu_candidates, gpu_candidates, parity_samples) = std::thread::scope(|scope| {
        let cpu_worker = scope.spawn(|| {
            benchmark_cpu_workers_until(cfg, cpu_pool, schedule.cpu_workers, 0, None, &stop_cpu)
        });
        let gpu_result = (|| -> Result<(u64, Vec<BenchmarkParitySample>)> {
            let mut gpu_candidates = 0u64;
            let mut completed_batches = 0usize;
            let mut pending_wallet_check: Option<(
                std::thread::ScopedJoinHandle<'_, Result<()>>,
                u64,
            )> = None;
            let mut last_parity_samples = None;
            // The CPU lane owns the lower half of the deterministic corpus;
            // GPU calibration uses the upper half so the measured lanes never
            // redo the same candidate work in parallel.
            let mut next_candidate = HYBRID_GPU_BENCHMARK_INDEX_START;
            while started.elapsed() < ROUTE_BENCHMARK_WINDOW
                || completed_batches < ROUTE_BENCHMARK_MIN_GPU_BATCHES
            {
                ensure_not_cancelled(cfg)?;
                let mnemonics = benchmark_candidate_batch(
                    &mut next_candidate,
                    gpu_batch_len,
                    &cfg.cancellation,
                )?;
                let (seeds, _) = benchmark_gpu_batch(cfg, engine, &mnemonics)?;
                if seeds.len() != mnemonics.len() {
                    anyhow::bail!(
                        "Hybrid benchmark GPU returned {} seeds for {} candidates",
                        seeds.len(),
                        mnemonics.len()
                    );
                }

                // Retire the previous CPU result stage only after this
                // dispatch has completed, so GPU PBKDF2 and address
                // derivation overlap just as they do during the search.
                if let Some((task, candidate_count)) = pending_wallet_check.take() {
                    task.join()
                        .map_err(|_| anyhow!("Hybrid benchmark GPU-result worker panicked"))??;
                    gpu_candidates = gpu_candidates.saturating_add(candidate_count);
                }

                last_parity_samples = Some(benchmark_parity_samples(&mnemonics, &seeds)?);
                let candidate_count = mnemonics.len() as u64;
                pending_wallet_check = Some((
                    scope.spawn(move || {
                        benchmark_wallet_stage_with_pool(cfg, &seeds, gpu_result_pool)
                    }),
                    candidate_count,
                ));
                completed_batches += 1;
            }
            if let Some((task, candidate_count)) = pending_wallet_check.take() {
                task.join()
                    .map_err(|_| anyhow!("Hybrid benchmark GPU-result worker panicked"))??;
                gpu_candidates = gpu_candidates.saturating_add(candidate_count);
            }
            let parity_samples = last_parity_samples
                .ok_or_else(|| anyhow!("Hybrid benchmark did no timed GPU work"))?;
            Ok((gpu_candidates, parity_samples))
        })();
        // Keep the CPU lane running for the full GPU measurement, including
        // any final batch that completes after the minimum time window.
        stop_cpu.store(true, Ordering::Release);
        let cpu_candidates = cpu_worker
            .join()
            .map_err(|_| anyhow!("CPU hybrid benchmark worker panicked"))?;
        let (gpu_candidates, parity_samples) = gpu_result?;
        let cpu_candidates = cpu_candidates?;
        Ok::<_, anyhow::Error>((cpu_candidates, gpu_candidates, parity_samples))
    })?;
    let elapsed = started.elapsed();
    validate_benchmark_parity_samples(cfg, &parity_samples)
        .context("Hybrid benchmark final parity check")?;
    Ok((cpu_candidates, gpu_candidates, elapsed))
}

#[cfg(feature = "gpu")]
fn benchmark_hybrid_gpu_warmup(
    cfg: &SearchConfig,
    engine: &AutoGpuEngine,
    gpu_batch_len: usize,
) -> Result<Zeroizing<Vec<[u8; 64]>>> {
    let mut warmup_index = u64::MAX.saturating_sub(gpu_batch_len as u64);
    let warmup_mnemonics =
        benchmark_candidate_batch(&mut warmup_index, gpu_batch_len, &cfg.cancellation)?;
    let (seeds, _) = benchmark_gpu_batch(cfg, engine, &warmup_mnemonics)?;
    validate_benchmark_seed_samples(cfg, &warmup_mnemonics, &seeds)
        .context("Hybrid benchmark GPU warmup")?;
    Ok(seeds)
}

#[cfg(feature = "gpu")]
fn safe_hybrid_gpu_warmup(
    cfg: &SearchConfig,
    backend: GpuBackend,
    engine: &AutoGpuEngine,
    gpu_batch_len: usize,
) -> Result<Zeroizing<Vec<[u8; 64]>>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        benchmark_hybrid_gpu_warmup(cfg, engine, gpu_batch_len)
    }))
    .unwrap_or_else(|_| anyhow::bail!("GPU backend {backend} panicked during Hybrid warmup"))
}
#[cfg(feature = "gpu")]
fn safe_fixed_gpu_benchmark(
    cfg: &SearchConfig,
    backend: GpuBackend,
    engine: &AutoGpuEngine,
    pool: &rayon::ThreadPool,
    mnemonics: &[Mnemonic],
) -> Result<(u64, Duration)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        benchmark_fixed_gpu_for_window(cfg, engine, pool, mnemonics)
    }))
    .unwrap_or_else(|_| anyhow::bail!("GPU backend {backend} panicked during fixed benchmark"))
}

#[cfg(feature = "gpu")]
fn safe_auto_gpu_benchmark(
    cfg: &SearchConfig,
    backend: GpuBackend,
    engine: &AutoGpuEngine,
    pool: &rayon::ThreadPool,
    batch_len: usize,
) -> Result<(u64, Duration)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        benchmark_gpu_for_window(cfg, engine, pool, batch_len)
    }))
    .unwrap_or_else(|_| anyhow::bail!("GPU backend {backend} panicked during Auto calibration"))
}

#[cfg(feature = "gpu")]
struct AutoHybridBenchmark<'a> {
    cfg: &'a SearchConfig,
    backend: GpuBackend,
    engine: &'a AutoGpuEngine,
    cpu_pool: &'a rayon::ThreadPool,
    gpu_result_pool: Option<&'a rayon::ThreadPool>,
    schedule: HybridSchedule,
    gpu_batch_len: usize,
    warmup_seeds: &'a [[u8; 64]],
}

#[cfg(feature = "gpu")]
fn safe_auto_hybrid_benchmark(request: AutoHybridBenchmark<'_>) -> Result<(u64, u64, Duration)> {
    let AutoHybridBenchmark {
        cfg,
        backend,
        engine,
        cpu_pool,
        gpu_result_pool,
        schedule,
        gpu_batch_len,
        warmup_seeds,
    } = request;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        benchmark_hybrid_for_window(
            cfg,
            engine,
            cpu_pool,
            gpu_result_pool,
            schedule,
            gpu_batch_len,
            warmup_seeds,
        )
    }))
    .unwrap_or_else(|_| anyhow::bail!("hybrid/{backend} panicked during Auto calibration"))
}

#[cfg(feature = "gpu")]
fn benchmark_auto_route_confirmation(cfg: &SearchConfig, route: AutoRoute) -> Result<f64> {
    match route {
        AutoRoute::Cpu => {
            let pool = build_search_pool(cfg)?;
            let (candidates, elapsed) = benchmark_cpu_for_window(cfg, &pool)?;
            Ok(candidates as f64 / elapsed.as_secs_f64().max(0.000_001))
        }
        AutoRoute::Gpu(backend, batch_len) => {
            let engine = AutoGpuEngine::new(cfg, backend)?;
            let available = AutoGpuEngine::available_device_count(backend).unwrap_or(1);
            let verified = verify_additional_gpu_devices(cfg, backend, batch_len, available)?;
            let worker_count = 1 + verified.len();
            drop(verified);

            let pool = build_search_pool(cfg)?;
            let (candidates, elapsed) =
                safe_auto_gpu_benchmark(cfg, backend, &engine, &pool, batch_len)?;
            Ok(candidates as f64 / elapsed.as_secs_f64().max(0.000_001) * worker_count as f64)
        }
        AutoRoute::Hybrid(backend, batch_len, schedule) => {
            let engine = AutoGpuEngine::new(cfg, backend)?;
            let available = AutoGpuEngine::available_device_count(backend).unwrap_or(1);
            let verified = verify_additional_gpu_devices(cfg, backend, batch_len, available)?;
            let worker_count = 1 + verified.len();
            drop(verified);

            let warmup_seeds = safe_hybrid_gpu_warmup(cfg, backend, &engine, batch_len)?;
            let cpu_pool = build_search_pool_with_threads(schedule.cpu_workers)?;
            let gpu_result_pool = build_search_pool_with_threads(schedule.gpu_result_workers)?;
            let (cpu_candidates, gpu_candidates, elapsed) =
                safe_auto_hybrid_benchmark(AutoHybridBenchmark {
                    cfg,
                    backend,
                    engine: &engine,
                    cpu_pool: &cpu_pool,
                    gpu_result_pool: Some(&gpu_result_pool),
                    schedule,
                    gpu_batch_len: batch_len,
                    warmup_seeds: &warmup_seeds,
                })?;
            let candidates =
                cpu_candidates.saturating_add(gpu_candidates.saturating_mul(worker_count as u64));
            Ok(candidates as f64 / elapsed.as_secs_f64().max(0.000_001))
        }
    }
}

#[cfg(feature = "gpu")]
const AUTO_MAX_CPU_SHORT_SEARCH_SECONDS: f64 = 30.0;

#[cfg(feature = "gpu")]
fn auto_should_skip_gpu(expected_candidates: u64, estimated_cpu_seconds: f64) -> bool {
    expected_candidates != u64::MAX && estimated_cpu_seconds <= AUTO_MAX_CPU_SHORT_SEARCH_SECONDS
}

/// Estimate the number of generated candidates needed for the configured match
/// criteria. This is a directional estimate for random vanity searches.
pub fn expected_search_candidates(cfg: &SearchConfig) -> u64 {
    if !cfg.matcher.exact.is_empty() && cfg.matcher.mode == MatchMode::All {
        return cfg.max_candidates.unwrap_or(u64::MAX);
    }

    let sample = crate::address::friendly_address(
        0,
        [0u8; 32],
        cfg.bounceable,
        cfg.network == Network::Testnet,
    );
    let bits_for = |pattern: &str, is_prefix: bool| -> Option<f64> {
        let constrained = if is_prefix {
            let fixed = &sample[..2];
            let fixed_count = pattern.len().min(2);
            let header_matches = if cfg.matcher.case_sensitive {
                pattern[..fixed_count] == fixed[..fixed_count]
            } else {
                pattern[..fixed_count].eq_ignore_ascii_case(&fixed[..fixed_count])
            };
            if !header_matches {
                return None;
            }
            pattern.chars().skip(2).collect::<Vec<_>>()
        } else {
            pattern.chars().collect::<Vec<_>>()
        };
        Some(
            constrained
                .iter()
                .take(46)
                .map(|character| {
                    if !cfg.matcher.case_sensitive && character.is_ascii_alphabetic() {
                        5.0
                    } else {
                        6.0
                    }
                })
                .sum(),
        )
    };

    let mut individual_bits = Vec::<f64>::new();
    for prefix in &cfg.matcher.prefix {
        if let Some(bits) = bits_for(prefix, true) {
            individual_bits.push(bits);
        } else if cfg.matcher.mode == MatchMode::All {
            return cfg.max_candidates.unwrap_or(u64::MAX);
        }
    }
    for suffix in &cfg.matcher.suffix {
        let bits = bits_for(suffix, false).unwrap_or(0.0);
        individual_bits.push(bits);
    }
    for _ in &cfg.matcher.exact {
        let exact_bits = 48.0 * 6.0;
        individual_bits.push(exact_bits);
    }
    let expected = match cfg.matcher.mode {
        // ALL requires a separate wallet for every criterion, so estimate the
        // total as the sum of the individual waiting times.
        MatchMode::All => individual_bits
            .iter()
            .map(|bits| 2f64.powf(*bits))
            .sum::<f64>(),
        // For ANY, using the rarest individual criterion is a conservative
        // estimate of the union search (overlap can only make it faster).
        MatchMode::Any => individual_bits
            .into_iter()
            .map(|bits| 2f64.powf(bits))
            .fold(f64::INFINITY, f64::min),
    };
    if !expected.is_finite() {
        return cfg.max_candidates.unwrap_or(u64::MAX);
    }
    let expected = expected.ceil().min(u64::MAX as f64) as u64;
    cfg.max_candidates
        .map_or(expected, |limit| expected.min(limit))
}

#[cfg(feature = "gpu")]
fn auto_expected_candidates(cfg: &SearchConfig) -> u64 {
    expected_search_candidates(cfg)
}

#[cfg(feature = "gpu")]
fn select_auto_route(
    best_single: (AutoRoute, f64),
    best_hybrid: Option<(AutoRoute, f64)>,
) -> (AutoRoute, f64) {
    match best_hybrid {
        Some(hybrid) if hybrid.1 > best_single.1 => hybrid,
        _ => best_single,
    }
}

#[cfg(feature = "gpu")]
fn choose_auto_route(cfg: &SearchConfig) -> Result<AutoRoute> {
    let calibration_started = Instant::now();
    if !gpu_passphrase_supported(&cfg.passphrase) {
        eprintln!(
            "auto benchmark: GPU PBKDF2 salt capacity exceeded; CPU is the only supported route"
        );
        return Ok(AutoRoute::Cpu);
    }

    let pool = build_search_pool(cfg)?;
    eprintln!(
        "auto calibration uses deterministic public BIP39 candidates; concurrent CPU and GPU lanes use disjoint streams; no random wallet candidates are generated or saved"
    );
    let (cpu_candidates, cpu_elapsed) = benchmark_cpu_for_window(cfg, &pool)?;
    let cpu_rate = cpu_candidates as f64 / cpu_elapsed.as_secs_f64().max(0.000_001);
    let expected_candidates = auto_expected_candidates(cfg);
    let estimated_cpu_seconds = expected_candidates as f64 / cpu_rate.max(0.000_001);
    if auto_should_skip_gpu(expected_candidates, estimated_cpu_seconds) {
        eprintln!(
            "auto selected CPU: estimated search is {} (short-search CPU fast path; GPU calibration is skipped only below {AUTO_MAX_CPU_SHORT_SEARCH_SECONDS:.0}s)",
            format_estimated_duration(expected_candidates, cpu_rate)
        );
        return Ok(AutoRoute::Cpu);
    }
    drop(pool);
    let hybrid_profiles = hybrid_schedules(cfg);
    let mut best_single_route = AutoRoute::Cpu;
    let mut best_single_rate = cpu_rate;
    let mut best_hybrid: Option<(AutoRoute, f64)> = None;
    eprintln!(
        "auto benchmark CPU: {cpu_rate:.1} addresses/s over {:.2}s",
        cpu_elapsed.as_secs_f64()
    );

    for backend in [
        GpuBackend::Vulkan,
        GpuBackend::Dx12,
        GpuBackend::Dx11,
        GpuBackend::Metal,
        GpuBackend::OpenCl,
        GpuBackend::OpenGl,
    ] {
        ensure_not_cancelled(cfg)?;
        let setup_started = Instant::now();
        let initialized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            AutoGpuEngine::new(cfg, backend)
        }));
        let mut engine = match initialized {
            Ok(Ok(engine)) => engine,
            Ok(Err(error)) => {
                eprintln!("auto benchmark {backend}: unavailable ({error:#})");
                continue;
            }
            Err(_) => {
                eprintln!("auto benchmark {backend}: initialization panicked; skipping this route");
                continue;
            }
        };
        let setup_elapsed = setup_started.elapsed();
        eprintln!(
            "auto benchmark {backend}: initialized in {:.2}s engine={}",
            setup_elapsed.as_secs_f64(),
            engine.backend_name()
        );

        let device_batch_limit = engine.max_batch_size();
        let configured_batch_limit = if cfg.gpu_batch == 0 {
            device_batch_limit
        } else {
            cfg.gpu_batch.min(device_batch_limit)
        };
        let tuner_maximum = cfg
            .max_candidates
            .map(|maximum| {
                configured_batch_limit
                    .min(usize::try_from(maximum.saturating_div(2).max(1)).unwrap_or(usize::MAX))
            })
            .unwrap_or(configured_batch_limit);
        let dispatch_batch_len = match safe_tune_gpu_dispatch_size(
            cfg,
            backend,
            &mut engine,
            tuner_maximum,
        ) {
            Ok(size) => size,
            Err(error) if calibration_error_is_cancellation(&error) => {
                return Err(error);
            }
            Err(error) if calibration_error_is_seed_integrity(&error) => {
                eprintln!(
                    "auto benchmark {backend}: PBKDF2 parity failed during dispatch tuning; rejecting this backend ({error:#})"
                );
                continue;
            }
            Err(error) => {
                eprintln!("auto benchmark {backend}: dispatch tuning failed ({error:#})");
                continue;
            }
        };
        let available = AutoGpuEngine::available_device_count(backend).unwrap_or(1);
        let verified_devices =
            verify_additional_gpu_devices(cfg, backend, dispatch_batch_len, available)?;
        let worker_count = 1 + verified_devices.len();
        drop(verified_devices);
        let gpu_benchmark = build_search_pool(cfg).and_then(|gpu_pool| {
            let result =
                safe_auto_gpu_benchmark(cfg, backend, &engine, &gpu_pool, dispatch_batch_len);
            drop(gpu_pool);
            result
        });
        match gpu_benchmark {
            Ok((candidates, elapsed)) => {
                let rate = candidates as f64 / elapsed.as_secs_f64().max(0.000_001);
                let effective_rate = rate * worker_count as f64;
                eprintln!(
                    "auto benchmark {backend}: {rate:.1} addresses/s per device over {:.2}s; configured GPU workers={worker_count}, projected aggregate={effective_rate:.1}/s",
                    elapsed.as_secs_f64()
                );
                if effective_rate > best_single_rate {
                    best_single_rate = effective_rate;
                    best_single_route = AutoRoute::Gpu(backend, dispatch_batch_len);
                }
            }
            Err(error) if calibration_error_is_cancellation(&error) => {
                return Err(error);
            }
            Err(error) if calibration_error_is_seed_integrity(&error) => {
                eprintln!(
                    "auto benchmark {backend}: PBKDF2 parity failed; rejecting this backend ({error:#})"
                );
                continue;
            }
            Err(error) => eprintln!("auto benchmark {backend}: unavailable ({error:#})"),
        }

        let hybrid_warmup_seeds =
            match safe_hybrid_gpu_warmup(cfg, backend, &engine, dispatch_batch_len) {
                Ok(seeds) => seeds,
                Err(error) if calibration_error_is_cancellation(&error) => return Err(error),
                Err(error) if calibration_error_is_seed_integrity(&error) => {
                    eprintln!("auto benchmark hybrid/{backend}: warmup parity failed ({error:#})");
                    continue;
                }
                Err(error) => {
                    eprintln!("auto benchmark hybrid/{backend}: warmup failed ({error:#})");
                    continue;
                }
            };
        for schedule in &hybrid_profiles {
            let schedule = *schedule;
            let profile_cpu_pool = match build_search_pool_with_threads(schedule.cpu_workers) {
                Ok(pool) => pool,
                Err(error) => {
                    eprintln!(
                        "auto benchmark hybrid/{backend}: CPU search pool unavailable ({} workers; {error:#})",
                        schedule.cpu_workers
                    );
                    continue;
                }
            };
            let profile_gpu_result_pool = match build_search_pool_with_threads(
                schedule.gpu_result_workers,
            ) {
                Ok(pool) => pool,
                Err(error) => {
                    eprintln!(
                        "auto benchmark hybrid/{backend}: skipping GPU-result CPU pool of {} workers ({error:#})",
                        schedule.gpu_result_workers
                    );
                    continue;
                }
            };
            match safe_auto_hybrid_benchmark(AutoHybridBenchmark {
                cfg,
                backend,
                engine: &engine,
                cpu_pool: &profile_cpu_pool,
                gpu_result_pool: Some(&profile_gpu_result_pool),
                schedule,
                gpu_batch_len: dispatch_batch_len,
                warmup_seeds: &hybrid_warmup_seeds,
            }) {
                Ok((cpu_candidates, gpu_candidates, elapsed)) => {
                    let projected_candidates = cpu_candidates
                        .saturating_add(gpu_candidates.saturating_mul(worker_count as u64));
                    let rate = projected_candidates as f64 / elapsed.as_secs_f64().max(0.000_001);
                    eprintln!(
                        "auto benchmark hybrid/{backend}: projected {rate:.1} candidates/s over {:.2}s (CPU {cpu_candidates} on {} CPU search worker(s), one GPU {gpu_candidates}, GPU-result CPU workers={}, gpu_workers={worker_count})",
                        elapsed.as_secs_f64(),
                        schedule.cpu_workers,
                        schedule.gpu_result_workers
                    );
                    if best_hybrid.is_none_or(|(_, previous_rate)| rate > previous_rate) {
                        best_hybrid = Some((
                            AutoRoute::Hybrid(backend, dispatch_batch_len, schedule),
                            rate,
                        ));
                    }
                }
                Err(error) if calibration_error_is_cancellation(&error) => {
                    return Err(error);
                }
                Err(error) if calibration_error_is_seed_integrity(&error) => {
                    eprintln!(
                        "auto benchmark hybrid/{backend}: PBKDF2 parity failed; rejecting this worker split ({error:#})"
                    );
                    continue;
                }
                Err(error) => eprintln!(
                    "auto benchmark hybrid/{backend} (CPU {}, GPU address {}): unavailable ({error:#})",
                    schedule.cpu_workers, schedule.gpu_result_workers
                ),
            }
        }
    }
    let confirmation_started = Instant::now();
    let (mut route, mut rate) =
        select_auto_route((best_single_route, best_single_rate), best_hybrid);
    if let Some((hybrid_route, initial_hybrid_rate)) = best_hybrid {
        eprintln!(
            "auto calibration confirmation: remeasuring the leading single route and leading hybrid route"
        );
        let confirmed_single_rate = match benchmark_auto_route_confirmation(cfg, best_single_route)
        {
            Ok(confirmed_rate) => {
                eprintln!(
                    "auto confirmation {:?}: initial={best_single_rate:.1}/s confirmed={confirmed_rate:.1}/s",
                    best_single_route
                );
                (best_single_rate + confirmed_rate) / 2.0
            }
            Err(error) if calibration_error_is_cancellation(&error) => return Err(error),
            Err(error) => {
                eprintln!(
                    "auto confirmation {:?} failed ({error:#}); keeping its initial measurement",
                    best_single_route
                );
                best_single_rate
            }
        };
        let confirmed_hybrid_rate = match benchmark_auto_route_confirmation(cfg, hybrid_route) {
            Ok(confirmed_rate) => {
                eprintln!(
                    "auto confirmation {:?}: initial={initial_hybrid_rate:.1}/s confirmed={confirmed_rate:.1}/s",
                    hybrid_route
                );
                (initial_hybrid_rate + confirmed_rate) / 2.0
            }
            Err(error) if calibration_error_is_cancellation(&error) => return Err(error),
            Err(error) => {
                eprintln!(
                    "auto confirmation {:?} failed ({error:#}); keeping its initial measurement",
                    hybrid_route
                );
                initial_hybrid_rate
            }
        };
        (route, rate) = select_auto_route(
            (best_single_route, confirmed_single_rate),
            Some((hybrid_route, confirmed_hybrid_rate)),
        );
    }
    eprintln!(
        "auto_confirmation_elapsed={:.2}s",
        confirmation_started.elapsed().as_secs_f64()
    );
    eprintln!(
        "auto_calibration_elapsed={:.2}s",
        calibration_started.elapsed().as_secs_f64()
    );
    let route_note = match route {
        AutoRoute::Cpu => "CPU route selected",
        AutoRoute::Gpu(_, _) => "GPU-assisted single route selected",
        AutoRoute::Hybrid(_, _, _) => "CPU and GPU remain active together",
    };
    eprintln!(
        "auto selected {route:?} at {rate:.1} measured candidates/s; {route_note}; estimated_search_after_calibration={}",
        format_estimated_duration(expected_candidates, rate)
    );
    Ok(route)
}

#[cfg(feature = "gpu")]
fn choose_hybrid_gpu_route(
    cfg: &SearchConfig,
) -> Result<Option<(GpuBackend, usize, HybridSchedule)>> {
    let calibration_started = Instant::now();
    if !gpu_passphrase_supported(&cfg.passphrase) {
        return Ok(None);
    }
    let hybrid_profiles = hybrid_schedules(cfg);
    let mut best = None;
    let mut best_rate = 0.0f64;

    eprintln!("hybrid calibration: measuring sustained CPU+GPU throughput for available adapters");
    eprintln!(
        "hybrid calibration uses deterministic public BIP39 candidates; concurrent CPU and GPU lanes use disjoint streams; no random wallet candidates are generated or saved"
    );
    for backend in [
        GpuBackend::Vulkan,
        GpuBackend::Dx12,
        GpuBackend::Dx11,
        GpuBackend::Metal,
        GpuBackend::OpenCl,
        GpuBackend::OpenGl,
    ] {
        ensure_not_cancelled(cfg)?;
        let initialized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            AutoGpuEngine::new(cfg, backend)
        }));
        let mut engine = match initialized {
            Ok(Ok(engine)) => engine,
            Ok(Err(error)) => {
                eprintln!("hybrid benchmark {backend}: unavailable ({error:#})");
                continue;
            }
            Err(_) => {
                eprintln!("hybrid benchmark {backend}: initialization panicked; skipping");
                continue;
            }
        };

        let device_batch_limit = engine.max_batch_size();
        let configured_batch_limit = if cfg.gpu_batch == 0 {
            device_batch_limit
        } else {
            cfg.gpu_batch.min(device_batch_limit)
        };
        let tuner_maximum = cfg
            .max_candidates
            .map(|maximum| {
                configured_batch_limit
                    .min(usize::try_from(maximum.saturating_div(2).max(1)).unwrap_or(usize::MAX))
            })
            .unwrap_or(configured_batch_limit);
        let dispatch_batch_len = match safe_tune_gpu_dispatch_size(
            cfg,
            backend,
            &mut engine,
            tuner_maximum,
        ) {
            Ok(size) => size,
            Err(error) if calibration_error_is_cancellation(&error) => {
                return Err(error);
            }
            Err(error) if calibration_error_is_seed_integrity(&error) => {
                eprintln!(
                    "hybrid benchmark {backend}: PBKDF2 parity failed during dispatch tuning; rejecting this backend ({error:#})"
                );
                continue;
            }
            Err(error) => {
                eprintln!("hybrid benchmark {backend}: dispatch tuning failed ({error:#})");
                continue;
            }
        };
        let available = AutoGpuEngine::available_device_count(backend).unwrap_or(1);
        let verified_devices =
            verify_additional_gpu_devices(cfg, backend, dispatch_batch_len, available)?;
        let worker_count = 1 + verified_devices.len();
        drop(verified_devices);
        let hybrid_warmup_seeds =
            match safe_hybrid_gpu_warmup(cfg, backend, &engine, dispatch_batch_len) {
                Ok(seeds) => seeds,
                Err(error) if calibration_error_is_cancellation(&error) => return Err(error),
                Err(error) if calibration_error_is_seed_integrity(&error) => {
                    eprintln!("hybrid benchmark {backend}: warmup parity failed ({error:#})");
                    continue;
                }
                Err(error) => {
                    eprintln!("hybrid benchmark {backend}: warmup failed ({error:#})");
                    continue;
                }
            };
        for schedule in &hybrid_profiles {
            let schedule = *schedule;
            let profile_cpu_pool = match build_search_pool_with_threads(schedule.cpu_workers) {
                Ok(pool) => pool,
                Err(error) => {
                    eprintln!(
                        "hybrid benchmark {backend}: CPU search pool unavailable ({} workers; {error:#})",
                        schedule.cpu_workers
                    );
                    continue;
                }
            };
            let profile_gpu_result_pool = match build_search_pool_with_threads(
                schedule.gpu_result_workers,
            ) {
                Ok(pool) => pool,
                Err(error) => {
                    eprintln!(
                        "hybrid benchmark {backend}: skipping GPU-result CPU pool of {} workers ({error:#})",
                        schedule.gpu_result_workers
                    );
                    continue;
                }
            };
            match safe_auto_hybrid_benchmark(AutoHybridBenchmark {
                cfg,
                backend,
                engine: &engine,
                cpu_pool: &profile_cpu_pool,
                gpu_result_pool: Some(&profile_gpu_result_pool),
                schedule,
                gpu_batch_len: dispatch_batch_len,
                warmup_seeds: &hybrid_warmup_seeds,
            }) {
                Ok((cpu_candidates, gpu_candidates, elapsed)) => {
                    let projected_candidates = cpu_candidates
                        .saturating_add(gpu_candidates.saturating_mul(worker_count as u64));
                    let rate = projected_candidates as f64 / elapsed.as_secs_f64().max(0.000_001);
                    eprintln!(
                        "hybrid benchmark {backend}: projected {rate:.1} candidates/s over {:.2}s (CPU {cpu_candidates} on {} CPU search worker(s), one GPU {gpu_candidates}, GPU-result CPU workers={}, gpu_workers={worker_count}, dispatch {dispatch_batch_len})",
                        elapsed.as_secs_f64(),
                        schedule.cpu_workers,
                        schedule.gpu_result_workers,
                    );
                    if rate > best_rate {
                        best_rate = rate;
                        best = Some((backend, dispatch_batch_len, schedule));
                    }
                }
                Err(error) if calibration_error_is_cancellation(&error) => {
                    return Err(error);
                }
                Err(error) if calibration_error_is_seed_integrity(&error) => {
                    eprintln!(
                        "hybrid benchmark {backend}: PBKDF2 parity failed for CPU search={} GPU-result CPU workers={} ({error:#})",
                        schedule.cpu_workers, schedule.gpu_result_workers
                    );
                    continue;
                }
                Err(error) => eprintln!(
                    "hybrid benchmark {backend} (CPU search {}, GPU-result CPU {}): unavailable ({error:#})",
                    schedule.cpu_workers, schedule.gpu_result_workers
                ),
            }
        }
    }
    eprintln!(
        "hybrid_calibration_elapsed={:.2}s",
        calibration_started.elapsed().as_secs_f64()
    );
    if let Some((backend, dispatch, schedule)) = best {
        let expected_candidates = expected_search_candidates(cfg);
        eprintln!(
            "hybrid selected {backend} dispatch={dispatch} CPU search workers={} GPU-result CPU workers={} at {best_rate:.1} measured candidates/s; estimated_search_after_calibration={}",
            schedule.cpu_workers,
            schedule.gpu_result_workers,
            format_estimated_duration(expected_candidates, best_rate)
        );
        Ok(Some((backend, dispatch, schedule)))
    } else {
        Ok(None)
    }
}
pub fn search_auto(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
) -> Result<FoundWallet> {
    cfg.validate()?;
    ensure_not_cancelled(cfg)?;
    if matcher_matches_all_addresses_from_header(cfg) {
        return search_one_header_match(cfg, counter, progress);
    }
    #[cfg(feature = "gpu")]
    {
        match choose_auto_route(cfg)? {
            AutoRoute::Cpu => search_cpu(cfg, counter, progress),
            AutoRoute::Gpu(backend, dispatch_batch_size) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    search_gpu_with_backend(
                        cfg,
                        counter,
                        progress,
                        backend,
                        Some(dispatch_batch_size),
                        false,
                        0,
                    )
                }));
                match result {
                    Ok(Ok(found)) => Ok(found),
                    Ok(Err(error))
                        if error.downcast_ref::<SeedIntegrityError>().is_some()
                            || error.downcast_ref::<SearchLimitError>().is_some()
                            || error.downcast_ref::<SearchCancelledError>().is_some() =>
                    {
                        Err(error)
                    }
                    Ok(Err(error)) => {
                        eprintln!(
                            "auto-selected {backend} failed after calibration ({error:#}); falling back to CPU"
                        );
                        search_cpu(cfg, counter, progress)
                    }
                    Err(_) => {
                        eprintln!(
                            "auto-selected {backend} panicked after calibration; falling back to CPU"
                        );
                        search_cpu(cfg, counter, progress)
                    }
                }
            }
            AutoRoute::Hybrid(backend, dispatch_batch_size, schedule) => {
                search_hybrid_with_preferred_gpu(
                    cfg,
                    counter,
                    progress,
                    Some(backend),
                    Some(dispatch_batch_size),
                    Some(schedule),
                )
            }
        }
    }
    #[cfg(not(feature = "gpu"))]
    {
        eprintln!("GPU feature not compiled; auto mode uses CPU search");
        search_cpu(cfg, counter, progress)
    }
}
pub fn search_hybrid(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
) -> Result<FoundWallet> {
    #[cfg(feature = "gpu")]
    {
        search_hybrid_with_preferred_gpu(cfg, counter, progress, None, None, None)
    }
    #[cfg(not(feature = "gpu"))]
    {
        search_hybrid_with_preferred_gpu(cfg, counter, progress, None, None)
    }
}

#[cfg(feature = "gpu")]
fn search_hybrid_with_preferred_gpu(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
    preferred_gpu: Option<GpuBackend>,
    preferred_dispatch: Option<usize>,
    preferred_schedule: Option<HybridSchedule>,
) -> Result<FoundWallet> {
    cfg.validate()?;
    ensure_not_cancelled(cfg)?;
    if matcher_matches_all_addresses_from_header(cfg) {
        eprintln!(
            "requested prefix is fixed by the TON address header; generating one wallet candidate"
        );
        return search_one_header_match(cfg, counter, progress);
    }
    #[cfg(feature = "gpu")]
    if !gpu_passphrase_supported(&cfg.passphrase) {
        eprintln!(
            "BIP39 passphrase exceeds GPU PBKDF2's 99-byte normalized salt capacity; hybrid mode uses CPU search for this run"
        );
        return search_cpu(cfg, counter, progress);
    }
    #[cfg(feature = "gpu")]
    {
        let (preferred_gpu, preferred_dispatch, schedule) = if let Some(backend) = preferred_gpu {
            (
                Some(backend),
                preferred_dispatch,
                preferred_schedule.unwrap_or_else(|| hybrid_schedule(cfg)),
            )
        } else if let Some((backend, dispatch, schedule)) = choose_hybrid_gpu_route(cfg)? {
            (Some(backend), Some(dispatch), schedule)
        } else {
            eprintln!("hybrid calibration found no usable GPU backend; continuing with CPU search");
            return search_cpu(cfg, counter, progress);
        };
        let backend_order = hybrid_gpu_backend_order(preferred_gpu);
        let worker_policy = "the CPU search retains all configured workers; GPU-result processing uses an independently tuned CPU pool";
        log_after_clearing_live_status(format_args!(
            "hybrid mode: CPU search workers={} + GPU-result CPU workers={}; {}; GPU fallback order={}",
            schedule.cpu_workers,
            schedule.gpu_result_workers,
            worker_policy,
            backend_order
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
        let (sender, receiver) = std::sync::mpsc::channel::<Result<FoundWallet>>();
        std::thread::scope(|scope| {
            let cpu_sender = sender.clone();
            let gpu_sender = sender.clone();
            scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    search_cpu_with_threads(cfg, counter, progress, schedule.cpu_workers)
                }))
                .unwrap_or_else(|_| Err(anyhow!("hybrid CPU worker panicked")));
                if result.is_ok() {
                    cfg.cancellation.store(true, Ordering::Release);
                }
                let _ = cpu_sender.send(result);
            });
            scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    search_hybrid_gpu_lane_with_order(
                        cfg,
                        counter,
                        progress,
                        &backend_order,
                        preferred_gpu.zip(preferred_dispatch),
                        schedule,
                    )
                }))
                .unwrap_or_else(|_| Err(anyhow!("hybrid GPU worker panicked")));
                if result.is_ok() {
                    cfg.cancellation.store(true, Ordering::Release);
                } else if let Err(error) = &result
                    && error.downcast_ref::<SearchCancelledError>().is_none()
                    && error.downcast_ref::<SearchLimitError>().is_none()
                {
                    eprintln!("hybrid GPU worker stopped ({error:#}); CPU worker continues");
                }
                let _ = gpu_sender.send(result);
            });
            drop(sender);

            let mut found = None;
            let mut failures = Vec::new();
            for result in receiver {
                match result {
                    Ok(wallet) => {
                        if found.is_none() {
                            found = Some(wallet);
                        }
                    }
                    Err(error) => failures.push(error),
                }
            }

            if let Some(wallet) = found {
                return Ok(wallet);
            }
            if cfg.cancellation.load(Ordering::Acquire) {
                return Err(SearchCancelledError.into());
            }
            if let Some(limit_index) = failures
                .iter()
                .position(|error| error.downcast_ref::<SearchLimitError>().is_some())
            {
                return Err(failures.remove(limit_index));
            }
            Err(failures
                .into_iter()
                .next()
                .unwrap_or_else(|| anyhow!("hybrid search stopped without a result")))
        })
    }
    #[cfg(not(feature = "gpu"))]
    {
        eprintln!("GPU feature not compiled; hybrid mode uses CPU search");
        search_cpu(cfg, counter, progress)
    }
}
#[cfg(not(feature = "gpu"))]
fn search_hybrid_with_preferred_gpu(
    cfg: &SearchConfig,
    counter: &AtomicU64,
    progress: &SearchProgress,
    _preferred_gpu: Option<GpuBackend>,
    _preferred_dispatch: Option<usize>,
) -> Result<FoundWallet> {
    cfg.validate()?;
    ensure_not_cancelled(cfg)?;
    eprintln!("GPU feature not compiled; hybrid mode uses CPU search");
    search_cpu(cfg, counter, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_result_batch_status_tracks_pending_work() {
        let progress = SearchProgress::default();
        assert_eq!(progress.current_gpu_result_batches_pending(), 0);

        progress.begin_gpu_result_batch();
        assert_eq!(progress.current_gpu_result_batches_pending(), 1);
        progress.begin_gpu_result_batch();
        assert_eq!(progress.current_gpu_result_batches_pending(), 2);

        progress.finish_gpu_result_batch();
        assert_eq!(progress.current_gpu_result_batches_pending(), 1);
        progress.finish_gpu_result_batch();
        assert_eq!(progress.current_gpu_result_batches_pending(), 0);
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn hybrid_profiles_keep_all_cpu_search_workers_and_tune_gpu_result_workers() {
        let one_thread = hybrid_schedules_for_workers(1);
        assert_eq!(one_thread.len(), 1);
        assert_eq!(one_thread[0].cpu_workers, 1);
        assert_eq!(one_thread[0].gpu_result_workers, 1);

        let four_threads = hybrid_schedules_for_workers(4);
        assert_eq!(
            four_threads
                .iter()
                .map(|schedule| (schedule.cpu_workers, schedule.gpu_result_workers))
                .collect::<Vec<_>>(),
            vec![(4, 1), (4, 2), (4, 3), (4, 4)]
        );

        let thirty_two_threads = hybrid_schedules_for_workers(32);
        assert!(thirty_two_threads.iter().all(|schedule| {
            schedule.cpu_workers == 32 && (1..=32).contains(&schedule.gpu_result_workers)
        }));
        assert!(thirty_two_threads.len() > 2);
        assert!(thirty_two_threads.len() < 32);
        assert!(
            thirty_two_threads
                .iter()
                .any(|schedule| schedule.gpu_result_workers == 32)
        );
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn hybrid_gpu_backend_order_is_vulkan_first_and_honors_auto_choice() {
        assert_eq!(
            hybrid_gpu_backend_order(None),
            vec![
                GpuBackend::Vulkan,
                GpuBackend::Dx12,
                GpuBackend::Dx11,
                GpuBackend::Metal,
                GpuBackend::OpenCl,
                GpuBackend::OpenGl
            ]
        );
        assert_eq!(
            hybrid_gpu_backend_order(Some(GpuBackend::OpenCl)),
            vec![
                GpuBackend::OpenCl,
                GpuBackend::Vulkan,
                GpuBackend::Dx12,
                GpuBackend::Dx11,
                GpuBackend::Metal,
                GpuBackend::OpenGl
            ]
        );
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_dispatch_tuner_sizes_grow_geometrically_to_the_requested_limit() {
        assert_eq!(gpu_dispatch_candidates(1), vec![1]);
        assert_eq!(gpu_dispatch_candidates(8), vec![1, 2, 4, 8]);
        assert_eq!(
            gpu_dispatch_candidates(1000),
            vec![1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1000]
        );
        assert_eq!(
            gpu_dispatch_candidates(usize::MAX).last(),
            Some(&usize::MAX)
        );
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_dispatch_confirmation_rechecks_top_sizes_and_neighbors() {
        let measurements = [
            (1, Duration::from_millis(300), 3.3),
            (2, Duration::from_millis(300), 6.6),
            (4, Duration::from_millis(300), 13.3),
            (8, Duration::from_millis(300), 26.6),
            (16, Duration::from_millis(300), 53.3),
            (32, Duration::from_millis(900), 35.5),
        ];
        assert_eq!(
            gpu_dispatch_confirmation_candidates(&measurements, 16),
            vec![8, 16, 32]
        );
        assert_eq!(median_rate(&[53.3, 35.5, 1_000.0]), Some(53.3));
        assert_eq!(
            median_duration(&[
                Duration::from_millis(300),
                Duration::from_millis(900),
                Duration::from_millis(350),
            ]),
            Some(Duration::from_millis(350))
        );
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_dispatch_tuning_batch_varies_inputs_with_bounded_generation_work() {
        use std::collections::HashSet;

        let batch = gpu_dispatch_tuning_batch(1_000, &AtomicBool::new(false)).unwrap();
        assert_eq!(batch.len(), 1_000);
        assert_eq!(
            batch.iter().take(GPU_DISPATCH_TUNING_CORPUS_SIZE).count(),
            GPU_DISPATCH_TUNING_CORPUS_SIZE
        );
        assert_eq!(
            batch.iter().collect::<HashSet<_>>().len(),
            GPU_DISPATCH_TUNING_CORPUS_SIZE
        );
        assert_eq!(batch[0], batch[GPU_DISPATCH_TUNING_CORPUS_SIZE]);
        assert_ne!(batch[0], batch[1]);
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_dispatch_latency_projection_avoids_oversized_probe_but_keeps_fixed_overhead_batches() {
        let projected_opengl = projected_dispatch_latency_ms(
            (512, Duration::from_millis(698)),
            (1024, Duration::from_millis(1368)),
            2048,
        )
        .unwrap();
        assert!(projected_opengl > 1500.0);

        // Vulkan on the Android test device has a nearly fixed ~1s cost for
        // dispatches of one and two candidates; don't incorrectly skip size 2.
        let projected_vulkan = projected_dispatch_latency_ms(
            (1, Duration::from_millis(1008)),
            (2, Duration::from_millis(1009)),
            4,
        )
        .unwrap();
        assert!(projected_vulkan <= 1500.0);
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn auto_skips_gpu_calibration_only_for_short_finite_searches() {
        assert!(auto_should_skip_gpu(100, 0.5));
        assert!(!auto_should_skip_gpu(100, 30.1));
        assert!(!auto_should_skip_gpu(u64::MAX, 1.0));
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn auto_selects_the_highest_measured_route_including_gpu_only() {
        let single_gpu = (AutoRoute::Gpu(GpuBackend::OpenCl, 512), 1_200.0);
        let slower_hybrid = Some((
            AutoRoute::Hybrid(
                GpuBackend::Vulkan,
                256,
                HybridSchedule {
                    cpu_workers: 4,
                    gpu_result_workers: 2,
                },
            ),
            1_100.0,
        ));
        assert!(matches!(
            select_auto_route(single_gpu, slower_hybrid).0,
            AutoRoute::Gpu(GpuBackend::OpenCl, 512)
        ));

        let faster_hybrid = Some((
            AutoRoute::Hybrid(
                GpuBackend::Vulkan,
                256,
                HybridSchedule {
                    cpu_workers: 4,
                    gpu_result_workers: 2,
                },
            ),
            1_300.0,
        ));
        assert!(matches!(
            select_auto_route(single_gpu, faster_hybrid).0,
            AutoRoute::Hybrid(GpuBackend::Vulkan, 256, _)
        ));
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn hybrid_calibration_cpu_and_gpu_candidate_corpora_are_disjoint() {
        use std::collections::HashSet;

        let cpu_candidates = (0..128)
            .map(benchmark_mnemonic)
            .collect::<Result<HashSet<_>>>()
            .unwrap();
        let gpu_candidates = (HYBRID_GPU_BENCHMARK_INDEX_START
            ..HYBRID_GPU_BENCHMARK_INDEX_START + 128)
            .map(benchmark_mnemonic)
            .collect::<Result<HashSet<_>>>()
            .unwrap();
        assert!(cpu_candidates.is_disjoint(&gpu_candidates));
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn calibration_parity_samples_cover_small_batches_and_large_batch_edges() {
        assert_eq!(benchmark_parity_sample_indices(3), vec![0, 1, 2]);
        let indices = benchmark_parity_sample_indices(100);
        assert!(indices.contains(&0));
        assert!(indices.contains(&1));
        assert!(indices.contains(&98));
        assert!(indices.contains(&99));
        assert!(indices.len() <= 18);

        let cfg = config(WalletVersion::V4R2, 0, false, None);
        let cancelled = AtomicBool::new(false);
        let mut first = 700;
        let mnemonics = benchmark_candidate_batch(&mut first, 100, &cancelled).unwrap();
        let mut seeds = vec![[0u8; 64]; mnemonics.len()];
        for index in benchmark_parity_sample_indices(mnemonics.len()) {
            seeds[index] = *derive_bip39_seed(&mnemonics[index], &cfg.passphrase);
        }
        validate_benchmark_seed_samples(&cfg, &mnemonics, &seeds).unwrap();
        seeds[99][0] ^= 1;
        let error = validate_benchmark_seed_samples(&cfg, &mnemonics, &seeds).unwrap_err();
        assert!(format!("{error:#}").contains("candidate 99"));
    }
    #[test]
    fn native_wgpu_backend_names_parse() {
        assert_eq!("dx12".parse::<GpuBackend>().unwrap(), GpuBackend::Dx12);
        assert_eq!("d3d12".parse::<GpuBackend>().unwrap(), GpuBackend::Dx12);
        assert_eq!("dx11".parse::<GpuBackend>().unwrap(), GpuBackend::Dx11);
        assert_eq!("d3d11".parse::<GpuBackend>().unwrap(), GpuBackend::Dx11);
        assert_eq!("metal".parse::<GpuBackend>().unwrap(), GpuBackend::Metal);
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn auto_estimates_case_folding_header_and_candidate_cap() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, None);
        cfg.matcher.prefix = vec!["UQAB".into()];
        assert_eq!(auto_expected_candidates(&cfg), 1_024);

        cfg.matcher.case_sensitive = true;
        assert_eq!(auto_expected_candidates(&cfg), 4_096);

        cfg.matcher.prefix.clear();
        cfg.matcher.suffix = vec!["yy".into()];
        cfg.matcher.case_sensitive = false;
        assert_eq!(auto_expected_candidates(&cfg), 1_024);

        cfg.max_candidates = Some(500);
        assert_eq!(auto_expected_candidates(&cfg), 500);
    }
    #[cfg(feature = "gpu")]
    #[test]
    fn auto_estimates_multiple_criteria_for_all_and_any() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, Some(100_000));
        cfg.matcher.prefix = vec!["UQAB".into(), "UQABC".into()];
        assert_eq!(auto_expected_candidates(&cfg), 33_792);

        cfg.matcher.mode = MatchMode::Any;
        assert_eq!(auto_expected_candidates(&cfg), 1_024);

        cfg.matcher.prefix.clear();
        cfg.matcher.suffix = vec!["yy".into()];
        cfg.matcher.exact = vec![crate::address::friendly_address(0, [0; 32], true, false)];
        assert_eq!(auto_expected_candidates(&cfg), 1_024);
    }

    #[test]
    fn expected_search_candidates_sum_distinct_all_criteria_and_clip_to_budget() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, Some(100_000));
        cfg.matcher.prefix = vec!["UQAB".into(), "UQABC".into()];
        assert_eq!(expected_search_candidates(&cfg), 33_792);

        cfg.max_candidates = Some(10_000);
        assert_eq!(expected_search_candidates(&cfg), 10_000);

        cfg.matcher.mode = MatchMode::Any;
        assert_eq!(expected_search_candidates(&cfg), 1_024);
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_worker_count_clamps_to_available_adapters() {
        assert_eq!(selected_gpu_worker_count(1, 4), 1);
        assert_eq!(selected_gpu_worker_count(3, 2), 2);
        assert_eq!(selected_gpu_worker_count(4, 0), 1);
    }
    fn config(
        wallet: WalletVersion,
        account: u32,
        exact: bool,
        max_candidates: Option<u64>,
    ) -> SearchConfig {
        SearchConfig {
            wallet,
            network: Network::Mainnet,
            bounceable: false,
            matcher: Matcher {
                prefix: if exact { vec![] } else { vec!["UQ".into()] },
                suffix: vec![],
                exact: if exact {
                    vec![crate::address::friendly_address(0, [0; 32], false, false)]
                } else {
                    vec![]
                },
                mode: MatchMode::All,
                case_sensitive: false,
            },
            threads: 0,
            gpu_devices: 1,
            account,
            passphrase: Zeroizing::new(String::new()),
            gpu_backend: GpuBackend::Cpu,
            gpu_batch: 256,
            max_candidates,
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn all_mode_collects_one_wallet_per_unmet_criterion() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, Some(10));
        cfg.matcher.prefix = vec!["EQ".into()];
        cfg.matcher.suffix = vec!["ZZ".into()];
        let progress = SearchProgress::default();

        let first = FoundWallet {
            mnemonic: "first phrase".into(),
            address: "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c".into(),
            private_key_hex: "first-key".into(),
        };
        assert!(
            progress
                .record_matching_wallet(&cfg.matcher, first)
                .is_none()
        );
        assert_eq!(progress.matched_criteria_count(), 1);

        let second = FoundWallet {
            mnemonic: "second phrase".into(),
            address: format!("UQ{}ZZ", "A".repeat(44)),
            private_key_hex: "second-key".into(),
        };
        assert!(
            progress
                .record_matching_wallet(&cfg.matcher, second)
                .is_some()
        );
        assert_eq!(progress.matched_criteria_count(), 2);
        let wallets = progress.take_found_wallets();
        assert_eq!(wallets.len(), 2);
        assert!(wallets[0].address.starts_with("EQ"));
        assert!(wallets[1].address.ends_with("ZZ"));
    }

    #[test]
    fn all_mode_requires_a_distinct_wallet_for_overlapping_criteria() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, Some(10));
        cfg.matcher.prefix = vec!["EQ".into()];
        cfg.matcher.suffix = vec!["9c".into()];
        let progress = SearchProgress::default();
        let first = FoundWallet {
            mnemonic: "first phrase".into(),
            address: "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c".into(),
            private_key_hex: "first-key".into(),
        };
        assert!(
            progress
                .record_matching_wallet(&cfg.matcher, first)
                .is_none()
        );
        assert_eq!(progress.matched_criteria_count(), 1);

        let second = FoundWallet {
            mnemonic: "second phrase".into(),
            address: "UQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c".into(),
            private_key_hex: "second-key".into(),
        };
        assert!(
            progress
                .record_matching_wallet(&cfg.matcher, second)
                .is_some()
        );
        let results = progress.take_found_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 0);
        assert_eq!(results[1].0, 1);
        assert_ne!(results[0].1.address, results[1].1.address);
    }

    #[test]
    fn any_mode_stops_after_first_match() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, Some(10));
        cfg.matcher.prefix = vec!["EQ".into(), "UQ".into()];
        cfg.matcher.mode = MatchMode::Any;
        let progress = SearchProgress::default();
        let make_wallet = |address: &str| FoundWallet {
            mnemonic: "synthetic phrase".into(),
            address: address.into(),
            private_key_hex: "synthetic-key".into(),
        };

        assert!(
            progress
                .record_matching_wallet(
                    &cfg.matcher,
                    make_wallet("EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c")
                )
                .is_some()
        );
        assert!(
            progress
                .record_matching_wallet(
                    &cfg.matcher,
                    make_wallet("UQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c")
                )
                .is_none()
        );
        assert_eq!(progress.take_found_wallets().len(), 1);
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_calibration_rejects_bad_seed_but_only_cancellation_aborts_scan() {
        let parity_error = anyhow::Error::new(SeedIntegrityError);
        assert!(calibration_error_is_seed_integrity(&parity_error));
        assert!(!calibration_error_is_cancellation(&parity_error));

        let cancelled = anyhow::Error::new(SearchCancelledError);
        assert!(calibration_error_is_cancellation(&cancelled));
        assert!(!calibration_error_is_seed_integrity(&cancelled));
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn benchmark_parity_checks_every_candidate_and_identifies_the_bad_index() {
        let cfg = config(WalletVersion::V4R2, 0, false, Some(10));
        let mnemonic = Mnemonic::parse_in_normalized(
            Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let expected = mnemonic.to_seed(&*cfg.passphrase);
        let mnemonics = vec![mnemonic; 2];
        let mut seeds = vec![expected; 2];
        seeds[1][0] ^= 1;

        let error = validate_benchmark_seeds(&cfg, &mnemonics, &seeds).unwrap_err();
        assert!(calibration_error_is_seed_integrity(&error));
        assert!(format!("{error:#}").contains("benchmark candidate 1"));
    }

    #[test]
    fn wallettg_requires_telegram_mainnet_account_zero_and_empty_passphrase() {
        let mut cfg = config(WalletVersion::WalletTg, 0, false, Some(10));
        assert!(cfg.validate().is_ok());

        cfg.account = 1;
        assert!(cfg.validate().is_err());
        cfg.account = 0;
        cfg.passphrase = Zeroizing::new("TREZOR".to_string());
        assert!(cfg.validate().is_err());
        cfg.passphrase.clear();
        cfg.network = Network::Testnet;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn address_header_prefix_shortcut_only_accepts_guaranteed_patterns() {
        let mut cfg = config(WalletVersion::V4R2, 0, false, None);
        cfg.matcher.prefix = vec!["UQ".into()];
        assert!(matcher_matches_all_addresses_from_header(&cfg));

        cfg.matcher.prefix = vec!["u".into()];
        assert!(matcher_matches_all_addresses_from_header(&cfg));

        cfg.matcher.mode = MatchMode::Any;
        cfg.matcher.suffix = vec!["never".into()];
        assert!(matcher_matches_all_addresses_from_header(&cfg));
        cfg.matcher.mode = MatchMode::All;
        assert!(!matcher_matches_all_addresses_from_header(&cfg));
        cfg.matcher.suffix.clear();

        cfg.matcher.case_sensitive = true;
        assert!(!matcher_matches_all_addresses_from_header(&cfg));

        cfg.matcher.case_sensitive = false;
        cfg.matcher.suffix = vec!["u".into()];
        assert!(!matcher_matches_all_addresses_from_header(&cfg));

        cfg.matcher.suffix.clear();
        cfg.network = Network::Testnet;
        cfg.matcher.prefix =
            vec![crate::address::friendly_address(0, [0; 32], cfg.bounceable, true)[..2].into()];
        assert!(matcher_matches_all_addresses_from_header(&cfg));
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_seed_check_parallel_path_returns_first_matching_candidate_in_order() {
        let cfg = config(WalletVersion::V4R2, 0, false, Some(2));
        let mnemonics = [
            Mnemonic::parse_in_normalized(
                Language::English,
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            )
            .unwrap(),
            Mnemonic::parse_in_normalized(
                Language::English,
                "legal winner thank year wave sausage worth useful legal winner thank yellow",
            )
            .unwrap(),
        ];
        let seeds = mnemonics
            .iter()
            .map(|mnemonic| mnemonic.to_seed(&*cfg.passphrase))
            .collect::<Vec<_>>();
        let progress = SearchProgress::default();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();

        let found = check_seed_batch_in_order(&cfg, &mnemonics, &seeds, &progress, Some(&pool))
            .unwrap()
            .unwrap();

        assert_eq!(found.mnemonic, mnemonics[0].to_string());
        assert!(progress.checked_addresses() >= 1);
        assert!(progress.checked_addresses() <= mnemonics.len() as u64);
    }

    #[test]
    fn bounceable_and_testnet_select_the_expected_friendly_address_header() {
        for (network, testnet) in [(Network::Mainnet, false), (Network::Testnet, true)] {
            for bounceable in [false, true] {
                let mut cfg = config(WalletVersion::V4R2, 0, false, None);
                cfg.network = network;
                cfg.bounceable = bounceable;
                let expected = crate::address::friendly_address(0, [0; 32], bounceable, testnet);
                cfg.matcher.prefix = vec![expected[..2].to_string()];
                assert!(matcher_matches_all_addresses_from_header(&cfg));

                cfg.matcher.prefix = vec!["ZZ".into()];
                assert!(!matcher_matches_all_addresses_from_header(&cfg));
            }
        }
    }

    #[test]
    fn all_backends_accept_unicode_bip39_passphrases() {
        let mut cfg = config(WalletVersion::V5R1, 0, false, Some(10));
        cfg.passphrase = Zeroizing::new("passphrase-ñ".to_string());

        for backend in [
            GpuBackend::Cpu,
            GpuBackend::Auto,
            GpuBackend::Hybrid,
            GpuBackend::OpenGl,
            GpuBackend::OpenCl,
            GpuBackend::Vulkan,
            GpuBackend::Cuda,
        ] {
            cfg.gpu_backend = backend;
            assert!(
                cfg.validate().is_ok(),
                "unexpected passphrase validation result for {backend}"
            );
        }

        cfg.passphrase = Zeroizing::new("TREZOR".to_string());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn exact_search_requires_a_candidate_limit() {
        assert!(
            config(WalletVersion::V5R1, 0, true, None)
                .validate()
                .is_err()
        );
        assert!(
            config(WalletVersion::V5R1, 0, true, Some(10))
                .validate()
                .is_ok()
        );
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_candidate_budget_reservations_cap_and_refund() {
        let mut cfg = config(WalletVersion::V5R1, 0, false, Some(10));
        cfg.gpu_batch = 8;
        let counter = AtomicU64::new(0);

        assert_eq!(reserve_batch_len(&cfg, &counter, cfg.gpu_batch).unwrap(), 8);
        let failed_batch = reserve_batch_len(&cfg, &counter, cfg.gpu_batch).unwrap();
        assert_eq!(failed_batch, 2);
        assert_eq!(counter.load(Ordering::Acquire), 10);

        release_batch_len(&counter, failed_batch);
        assert_eq!(counter.load(Ordering::Acquire), 8);
        assert_eq!(reserve_batch_len(&cfg, &counter, cfg.gpu_batch).unwrap(), 2);
        assert_eq!(counter.load(Ordering::Acquire), 10);
        assert!(reserve_batch_len(&cfg, &counter, cfg.gpu_batch).is_err());
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn concurrent_gpu_batch_reservations_never_exceed_candidate_limit() {
        let mut cfg = config(WalletVersion::V5R1, 0, false, Some(101));
        cfg.gpu_batch = 7;
        let counter = AtomicU64::new(0);

        let reserved_total = std::thread::scope(|scope| {
            let handles = (0..32)
                .map(|_| scope.spawn(|| reserve_batch_len(&cfg, &counter, cfg.gpu_batch).ok()))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap().unwrap_or_default())
                .sum::<usize>()
        });

        assert_eq!(reserved_total, 101);
        assert_eq!(counter.load(Ordering::Acquire), 101);
    }
    #[test]
    fn wallet_account_ranges_are_checked_before_search() {
        assert!(
            config(WalletVersion::V5R1, 32_768, false, None)
                .validate()
                .is_err()
        );
        assert!(
            config(WalletVersion::V4R2, 0x8000_0000, false, None)
                .validate()
                .is_err()
        );
        assert!(
            config(WalletVersion::V4R2, 0x7fff_ffff, false, None)
                .validate()
                .is_ok()
        );
    }
}
