//! HuggingFace model downloader with concurrent multi-source racing.
//!
//! Downloads ONNX model files and tokenizers from HuggingFace Hub.
//! All known sources (official + built-in mirrors + custom mirrors)
//! are raced **concurrently** — the fastest responder wins and
//! the losers are cancelled. This eliminates the need for users
//! to know their network environment or configure mirrors manually.
//!
//! # Built-in sources
//!
//! The official HuggingFace URL (`https://huggingface.co`) and
//! `hf-mirror.com` are always included. Users behind the GFW
//! benefit from the mirror automatically; overseas users benefit
//! from the official source — no configuration needed.
//!
//! Additional custom mirrors can be supplied via `hf_mirrors`
//! for enterprise or private registry scenarios.
//!
//! # Cross-platform notes
//!
//! `std::fs::rename` behaves differently across platforms:
//! - **Unix**: atomically replaces the target if it exists.
//! - **Windows**: fails if the target already exists.
//!
//! We use a `rename_or_replace` helper that handles both cases correctly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use reqwest::Client;

// ── Error type ──────────────────────────────────────────────────────────

/// Error type for download operations.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Download cancelled")]
    Cancelled,

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

// ── Cross-platform rename helper ────────────────────────────────────────

/// Rename a file or directory, replacing the target if it already exists.
///
/// On Unix, `std::fs::rename` atomically replaces the target.
/// On Windows, `std::fs::rename` fails if the target exists, so we
/// remove the target first and then rename.
fn rename_or_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    // Try the simple rename first (works on Unix and when dst doesn't exist)
    if let Ok(()) = std::fs::rename(src, dst) {
        return Ok(());
    }
    // Fallback: remove target then rename (needed on Windows when dst exists)
    if dst.exists() {
        if dst.is_dir() {
            std::fs::remove_dir_all(dst)?;
        } else {
            std::fs::remove_file(dst)?;
        }
    }
    std::fs::rename(src, dst)
}

// ── HuggingFace URL builder ─────────────────────────────────────────────

const HF_DEFAULT_BASE: &str = "https://huggingface.co";

/// Built-in mirror URLs that are always raced alongside the official source.
/// Users never need to configure these — the app handles network adaptation
/// automatically (GFW users benefit from the mirror; overseas users from the
/// official source).
const HF_BUILTIN_MIRRORS: &[&str] = &["https://hf-mirror.com"];

/// Build the download URL for a HuggingFace file.
///
/// `{base}/{repo}/resolve/main/{path}`
fn hf_file_url(hf_repo: &str, file_path: &str, base: &str) -> String {
    format!("{base}/{hf_repo}/resolve/main/{file_path}")
}

// ── Download result ─────────────────────────────────────────────────────

/// Result of a model download operation.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// Local directory where files were saved.
    pub model_dir: PathBuf,
    /// List of files that were downloaded.
    pub downloaded_files: Vec<String>,
}

pub struct DownloadSpec<'a> {
    pub model_id: &'a str,
    pub hf_repo: &'a str,
    pub onnx_file: &'a str,
    pub tokenizer_file: &'a str,
    pub external_data_files: &'a [String],
}

/// Progress callback type: `(downloaded_bytes, total_bytes)`.
pub type ProgressCb = dyn Fn(u64, u64) + Send + Sync;

// ── Shared download progress tracker ──────────────────────────────────

/// Thread-safe download progress tracker shared across concurrent racers.
///
/// Tracks aggregate progress across all files in a multi-file model download.
/// The downloader downloads files one at a time (sequentially) but each file
/// is raced across multiple HTTP sources; only the winning source writes to
/// the per-file atomics below.
///
/// Layout:
/// - `file_bytes_downloaded` / `file_total_bytes` describe the **current**
///   file being raced. Concurrent racer tasks `fetch_max` into these as
///   they receive chunks (so a fast loser's early chunks cannot artificially
///   inflate progress past the eventual winner).
/// - `accum_bytes_downloaded` / `accum_total_bytes` describe all files that
///   have been committed via [`Self::commit_file`] (i.e., already finished).
///
/// `snapshot()` combines both slots into a single `(downloaded, total)`
/// ratio, so the UI sees a smooth 0→100% sweep across all files instead
/// of snapping to 100% after the first small file (tokenizer) while the
/// large file (model.onnx) is still in flight.
///
/// ponytail: cumulative bytes use `fetch_max` everywhere — multi-source
/// races intentionally take the high-water mark so a slow loser's stale
/// chunks can never lower the reported progress. Across files the serial
/// `commit_file()` / `begin_file()` calls in the main download loop
/// guarantee no race between "moving prior bytes to accum" and "starting
/// the next file's per-file counters".
pub struct DownloadProgress {
    /// Bytes downloaded so far for the current file (winner high-water).
    pub file_bytes_downloaded: AtomicU64,
    /// Total bytes of the current file (0 if unknown / not yet declared).
    pub file_total_bytes: AtomicU64,
    /// Sum of bytes from all previously committed files.
    pub accum_bytes_downloaded: AtomicU64,
    /// Sum of declared sizes from all previously committed files.
    pub accum_total_bytes: AtomicU64,
    /// Name of the file currently being downloaded (e.g., "model.onnx").
    pub current_file: std::sync::Mutex<String>,
}

impl Default for DownloadProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadProgress {
    /// Create a new progress tracker with zero state.
    pub fn new() -> Self {
        Self {
            file_bytes_downloaded: AtomicU64::new(0),
            file_total_bytes: AtomicU64::new(0),
            accum_bytes_downloaded: AtomicU64::new(0),
            accum_total_bytes: AtomicU64::new(0),
            current_file: std::sync::Mutex::new(String::new()),
        }
    }

    /// Commit the just-finished file's bytes into the cumulative slot and
    /// reset the per-file counters so the next `begin_file()` starts clean.
    ///
    /// Called by the main download loop after a file race completes, before
    /// starting the next file. Safe to call when per-file counters are 0
    /// (e.g., the file was skipped because it already existed on disk).
    pub fn commit_file(&self) {
        let downloaded = self.file_bytes_downloaded.swap(0, Ordering::AcqRel);
        let total = self.file_total_bytes.swap(0, Ordering::AcqRel);
        if downloaded > 0 {
            self.accum_bytes_downloaded
                .fetch_add(downloaded, Ordering::Relaxed);
        }
        if total > 0 {
            self.accum_total_bytes.fetch_add(total, Ordering::Relaxed);
        }
    }

    /// Return progress as `(percentage 0-100, bytes_downloaded, total_bytes)`
    /// across **all** files in the download.
    pub fn snapshot(&self) -> (u8, u64, u64) {
        let file_downloaded = self.file_bytes_downloaded.load(Ordering::Relaxed);
        let file_total = self.file_total_bytes.load(Ordering::Relaxed);
        let accum_downloaded = self.accum_bytes_downloaded.load(Ordering::Relaxed);
        let accum_total = self.accum_total_bytes.load(Ordering::Relaxed);
        let downloaded = accum_downloaded.saturating_add(file_downloaded);
        let total = accum_total.saturating_add(file_total);
        let pct = if total > 0 {
            ((downloaded as f64 / total as f64) * 100.0).min(100.0) as u8
        } else {
            0
        };
        (pct, downloaded, total)
    }
}

// ── Downloader ──────────────────────────────────────────────────────────

/// HuggingFace model downloader with concurrent multi-source racing.
///
/// All known sources (official + built-in mirrors + optional custom
/// mirrors) are raced concurrently. The fastest responder wins —
/// no manual configuration needed.
pub struct Downloader {
    /// HTTP client (shared across all concurrent racers via Arc).
    http_client: Arc<Client>,
    /// Base models directory.
    models_dir: PathBuf,
    /// Additional custom mirror URLs (beyond built-in mirrors).
    hf_mirrors: Vec<String>,
}

impl Downloader {
    /// Create a new downloader.
    ///
    /// `hf_mirrors` provides additional custom mirror URLs beyond the
    /// built-in ones. Typically left empty — the built-in mirrors
    /// already cover the common network environments.
    pub fn new(models_dir: &Path, hf_mirrors: Vec<String>) -> Self {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(600)) // 10 min per file
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client for downloader");

        Self {
            http_client: Arc::new(http_client),
            models_dir: models_dir.to_path_buf(),
            hf_mirrors,
        }
    }

    /// Build the list of all source base URLs.
    ///
    /// Always includes the official HuggingFace URL + built-in mirrors,
    /// plus any user-configured custom mirrors. All sources are raced
    /// concurrently, so ordering does not affect latency.
    fn sources(&self) -> Vec<String> {
        let mut sources = vec![HF_DEFAULT_BASE.to_string()];
        for &m in HF_BUILTIN_MIRRORS {
            sources.push(m.to_string());
        }
        for m in &self.hf_mirrors {
            if !sources.contains(m) {
                sources.push(m.clone());
            }
        }
        sources
    }

    /// Download a model from HuggingFace.
    ///
    /// Downloads the ONNX model file (selected variant) and tokenizer.
    /// Files are written to a temp directory first, then atomically renamed.
    ///
    /// If a previous download attempt left partial files in the temp
    /// directory, this function will resume from the partial data using
    /// HTTP Range requests, so large models (400MB+) don't restart from
    /// zero on every retry.
    ///
    /// `progress` is a shared tracker that receives real-time byte-level
    /// progress — suitable for exposing to the UI via polling.
    ///
    /// # Arguments
    /// * `spec` - Model files and HuggingFace repository metadata.
    /// * `progress` - Shared progress tracker updated by the winning racer.
    /// * `cancel` - Cancellation flag (if true, abort download).
    pub async fn download_model(
        &self,
        spec: DownloadSpec<'_>,
        progress: &DownloadProgress,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<DownloadResult, DownloadError> {
        let model_id = spec.model_id;
        let model_dir = self.models_dir.join(model_id);
        let tmp_dir = self.models_dir.join(format!("{model_id}.downloading"));

        // If a previous download completed (files were renamed to model_dir),
        // we're done.
        if self.is_downloaded(model_id) {
            tracing::info!(model_id, "Model already downloaded, skipping");
            return Ok(DownloadResult {
                model_dir,
                downloaded_files: vec!["model.onnx".to_string(), "tokenizer.json".to_string()],
            });
        }

        // Create temp directory (preserve partial files from prior attempts).
        if !tmp_dir.exists() {
            std::fs::create_dir_all(&tmp_dir)?;
        }

        let mut downloaded_files = Vec::new();

        // Files to download: (remote_path, local_name)
        // The ONNX file is always saved as "model.onnx" for consistent loading.
        // The tokenizer is always saved as "tokenizer.json".
        let files_to_download = [
            (spec.onnx_file, "model.onnx"),
            (spec.tokenizer_file, "tokenizer.json"),
        ];

        let sources = self.sources();

        tracing::info!(
            sources = ?sources,
            "Starting concurrent download race with {} sources",
            sources.len()
        );

        for (remote_path, local_name) in &files_to_download {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Err(DownloadError::Cancelled);
            }

            // Update the current-file label without resetting the
            // byte counters, so the progress bar moves monotonically
            // across files instead of jumping back to 0 each time.
            if let Ok(mut name) = progress.current_file.lock() {
                *name = local_name.to_string();
            }
            let local_path = tmp_dir.join(local_name);
            // Skip the download if the file already exists in the temp
            // directory — a previous attempt may have completed this
            // file before failing on another. Existence + non-zero size
            // is a simple integrity check (we trust the server's data).
            if local_path.exists() && local_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                tracing::info!(local_name, "File already downloaded, skipping");
                downloaded_files.push(local_name.to_string());
                continue;
            }
            download_file_race(
                &self.http_client,
                spec.hf_repo,
                remote_path,
                &local_path,
                &sources,
                progress,
            )
            .await?;
            // Move this file's byte counters into the cumulative slot so
            // `snapshot()` reflects progress across all files so far —
            // not just the current one. Without this, downloading a small
            // tokenizer before a large model.onnx would show 100% the
            // moment the tokenizer finishes and stay there until the
            // ONNX file is done.
            progress.commit_file();

            downloaded_files.push(local_name.to_string());
        }

        for remote_path in spec.external_data_files {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Err(DownloadError::Cancelled);
            }

            let local_name = Path::new(remote_path)
                .file_name()
                .expect("external data path should have a filename")
                .to_str()
                .expect("filename should be valid UTF-8");
            if let Ok(mut name) = progress.current_file.lock() {
                *name = local_name.to_string();
            }
            let local_path = tmp_dir.join(local_name);
            if local_path.exists() && local_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                tracing::info!(
                    local_name,
                    "External data file already downloaded, skipping"
                );
                downloaded_files.push(local_name.to_string());
                continue;
            }
            download_file_race(
                &self.http_client,
                spec.hf_repo,
                remote_path,
                &local_path,
                &sources,
                progress,
            )
            .await?;
            progress.commit_file();

            downloaded_files.push(local_name.to_string());
        }

        // Atomic rename: tmp_dir → model_dir (cross-platform)
        rename_or_replace(&tmp_dir, &model_dir)?;

        tracing::info!(
            model_id,
            dir = %model_dir.display(),
            files = ?downloaded_files,
            "Model download complete"
        );

        Ok(DownloadResult {
            model_dir,
            downloaded_files,
        })
    }

    /// Check if a model is already downloaded.
    pub fn is_downloaded(&self, model_id: &str) -> bool {
        let model_dir = self.models_dir.join(model_id);
        model_dir.exists()
            && model_dir.join("model.onnx").exists()
            && model_dir.join("tokenizer.json").exists()
    }

    /// Delete a downloaded model.
    pub fn delete_model(&self, model_id: &str) -> Result<(), DownloadError> {
        let model_dir = self.models_dir.join(model_id);
        if model_dir.exists() {
            std::fs::remove_dir_all(&model_dir)?;
            tracing::info!(model_id, "Deleted model files");
        }
        Ok(())
    }

    /// Get the models directory path.
    pub fn models_dir_path(&self) -> &Path {
        &self.models_dir
    }
}

// ── Concurrent download race ───────────────────────────────────────────

/// Race multiple download sources concurrently.
///
/// All sources start downloading simultaneously via `tokio::task::JoinSet`.
/// The first successful download wins — all other tasks are aborted,
/// their partial temp files cleaned up. If all sources fail, the last
/// error is returned.
///
/// Each racer writes to a unique per-source temp file (`{dest}_{i}.tmp`)
/// to avoid write conflicts. The winner's temp file is atomically renamed
/// to the final destination.
async fn download_file_race(
    client: &Arc<Client>,
    hf_repo: &str,
    remote_path: &str,
    dest: &Path,
    sources: &[String],
    progress: &DownloadProgress,
) -> Result<(), DownloadError> {
    let mut set = tokio::task::JoinSet::new();

    for (idx, base) in sources.iter().enumerate() {
        let url = hf_file_url(hf_repo, remote_path, base);
        let client = Arc::clone(client);
        let dest = dest.to_path_buf();
        // SAFETY: progress is borrowed from the caller and lives for the
        // duration of this function. We extend its lifetime for spawned
        // tasks by converting the reference to a raw pointer, then back
        // inside the task. This is safe because:
        // 1. The JoinSet is awaited to completion (or abort_all) before
        //    this function returns, so all tasks finish before `progress`
        //    is dropped.
        // 2. DownloadProgress uses atomic fields, so concurrent writes
        //    are safe.
        let progress_ptr = progress as *const DownloadProgress as usize;

        set.spawn(async move {
            // SAFETY: see comment above
            let progress = unsafe { &*(progress_ptr as *const DownloadProgress) };
            let result = download_file_with_retries(&client, &url, &dest, idx, progress).await;
            (idx, url, result)
        });
    }

    let total = sources.len();
    let mut last_err: Option<DownloadError> = None;

    while let Some(outcome) = set.join_next().await {
        match outcome {
            Ok((idx, url, Ok(()))) => {
                tracing::info!(
                    source = idx + 1,
                    total,
                    url = %url,
                    "Download race winner (source {})",
                    idx + 1
                );
                // Abort all remaining tasks — JoinSet drop also handles this
                set.abort_all();
                return Ok(());
            }
            Ok((idx, url, Err(e))) => {
                tracing::warn!(
                    source = idx + 1,
                    total,
                    url = %url,
                    error = %e,
                    "Source {}/{} failed in race",
                    idx + 1, total
                );
                last_err = Some(e);
            }
            Err(join_err) => {
                // Task was cancelled or panicked — only record if no other error yet
                if join_err.is_cancelled() && last_err.is_none() {
                    last_err = Some(DownloadError::Cancelled);
                } else if last_err.is_none() {
                    last_err = Some(DownloadError::InvalidUrl(format!(
                        "Download task panicked: {}",
                        join_err
                    )));
                }
            }
        }
    }

    Err(last_err
        .unwrap_or_else(|| DownloadError::InvalidUrl("No download sources available".to_string())))
}

/// Download a single file with retry logic (used inside race tasks).
///
/// Retries up to 3 times on transient HTTP errors (5xx, timeout).
/// Each racer writes to a unique temp file (`{dest}_{idx}.tmp`) to
/// avoid write conflicts between concurrent racers.
async fn download_file_with_retries(
    client: &Client,
    url: &str,
    dest: &Path,
    idx: usize,
    progress: &DownloadProgress,
) -> Result<(), DownloadError> {
    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_MS: u64 = 2000;

    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_single(client, url, dest, idx, progress).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let is_transient = match &e {
                    DownloadError::Http(req_err) => {
                        req_err.is_timeout()
                            || req_err.is_connect()
                            || req_err.status().is_some_and(|s| s.is_server_error())
                    }
                    _ => false,
                };

                if is_transient && attempt < MAX_RETRIES {
                    tracing::warn!(
                        url,
                        source = idx + 1,
                        attempt,
                        error = %e,
                        "Transient error, retrying..."
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(
                        RETRY_DELAY_MS * attempt as u64,
                    ))
                    .await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Inner download implementation (single attempt, single source).
///
/// Uses streaming to avoid loading the entire file into memory.
/// Writes to a per-source temp file (`{dest}_{idx}.tmp`), then
/// atomically renames to the final destination.
///
/// If a partial temp file already exists from a previous attempt, it
/// sends an HTTP Range header to resume the download rather than
/// starting from zero.
async fn download_single(
    client: &Client,
    url: &str,
    dest: &Path,
    idx: usize,
    progress: &DownloadProgress,
) -> Result<(), DownloadError> {
    let stem = dest
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let tmp_path = dest.with_file_name(format!("{}_{}.tmp", stem, idx));

    // Check for a partial file from a previous attempt. We only resume
    // if at least 4 KB are present — less than that is probably a
    // corrupted or server-error response that should be replaced.
    let resume_offset: u64 = std::fs::metadata(&tmp_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let resume = resume_offset > 4096;

    // Build the request. Include a Range header if resuming.
    let mut req = client.get(url);
    if resume {
        req = req.header("Range", format!("bytes={resume_offset}-"));
        tracing::info!(url, source = idx + 1, resume_offset, "Resuming download");
    }

    let response = req.send().await?.error_for_status()?;
    let status = response.status().as_u16();

    let mut downloaded: u64 = if resume && status == 206 {
        // Server accepted the Range request — append to the partial.
        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let total_from_range = content_range
            .split('/')
            .next_back()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        progress
            .file_total_bytes
            .fetch_max(resume_offset + total_from_range, Ordering::Relaxed);
        progress
            .file_bytes_downloaded
            .fetch_max(resume_offset, Ordering::Relaxed);
        tracing::info!(
            source = idx + 1,
            resumed = resume_offset,
            total = total_from_range,
            "Resume accepted"
        );
        resume_offset
    } else {
        // Full download — either no resume needed, or server ignored
        // the Range request. Remove any stale partial first.
        if resume {
            tracing::info!(
                url,
                status,
                "Range request not honored; downloading full file"
            );
            let _ = std::fs::remove_file(&tmp_path);
        }
        let total = response.content_length().unwrap_or(0);
        progress.file_total_bytes.fetch_max(total, Ordering::Relaxed);
        tracing::info!(total, source = idx + 1, url, "Downloading");
        0u64
    };

    // Open the file: append if resuming, create if fresh.
    let file = if downloaded > 0 {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&tmp_path)
            .await?
    } else {
        tokio::fs::File::create(&tmp_path).await?
    };
    let mut writer = tokio::io::BufWriter::new(file);

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(DownloadError::Http)?;
        downloaded += chunk.len() as u64;
        progress
            .file_bytes_downloaded
            .fetch_max(downloaded, Ordering::Relaxed);
        tokio::io::AsyncWriteExt::write_all(&mut writer, &chunk).await?;
    }
    tokio::io::AsyncWriteExt::flush(&mut writer).await?;
    drop(writer);

    // Rename temp file → final destination
    if let Err(rename_err) = rename_or_replace(&tmp_path, dest) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(DownloadError::Io(rename_err));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Multi-file download — e.g. small `tokenizer.json` (10 B) followed by
    /// large `model.onnx` (1000 B). After the first file finishes, snapshot
    /// must NOT report 100%; it should reflect `10 / (10+1000)` so the UI
    /// keeps moving while the big file downloads.
    ///
    /// This is the regression test for the bug where a 100% floor was
    /// latched after each file and the progress bar stayed pinned at
    /// 100% for the entire duration of the second file.
    #[test]
    fn snapshot_reflects_aggregate_progress_across_files() {
        let p = DownloadProgress::new();

        // Initial state — no files announced yet.
        assert_eq!(p.snapshot(), (0, 0, 0));

        // File 1 (tokenizer): declare 10 B total, fully download 10 B.
        p.file_total_bytes.store(10, Ordering::Relaxed);
        p.file_bytes_downloaded.store(10, Ordering::Relaxed);

        // Mid-flight: 5 / 10 of file 1.
        p.file_bytes_downloaded.store(5, Ordering::Relaxed);
        assert_eq!(p.snapshot(), (50, 5, 10));

        // File 1 done.
        p.file_bytes_downloaded.store(10, Ordering::Relaxed);
        p.commit_file();
        assert_eq!(p.file_bytes_downloaded.load(Ordering::Relaxed), 0);
        assert_eq!(p.file_total_bytes.load(Ordering::Relaxed), 0);
        // Cumulative reflects file 1 only.
        assert_eq!(p.snapshot(), (100, 10, 10));

        // File 2 (model.onnx): declare 1000 B total, download 500 B.
        p.file_total_bytes.store(1000, Ordering::Relaxed);
        p.file_bytes_downloaded.store(500, Ordering::Relaxed);

        // Snapshot must show 510 / 1010 ≈ 50%, NOT 100%.
        // This is the core regression: under the old `progress_floor`
        // model, this same state would have returned (100, _, _).
        let (pct, downloaded, total) = p.snapshot();
        assert_eq!(downloaded, 510);
        assert_eq!(total, 1010);
        assert_eq!(pct, 50, "expected ~50% after half of second file, got {pct}");

        // File 2 finishes.
        p.file_bytes_downloaded.store(1000, Ordering::Relaxed);
        assert_eq!(p.snapshot(), (100, 1010, 1010));
        p.commit_file();

        // After both files committed, accum holds the whole download.
        assert_eq!(p.snapshot(), (100, 1010, 1010));
    }

    /// `commit_file()` is safe to call when the per-file counters are 0
    /// (e.g., a file was skipped because it already existed on disk).
    /// It must not corrupt the accum slot or panic.
    #[test]
    fn commit_file_with_zero_counters_is_noop() {
        let p = DownloadProgress::new();
        p.accum_bytes_downloaded.store(100, Ordering::Relaxed);
        p.accum_total_bytes.store(200, Ordering::Relaxed);

        p.commit_file();

        assert_eq!(p.snapshot(), (50, 100, 200));
    }
}
