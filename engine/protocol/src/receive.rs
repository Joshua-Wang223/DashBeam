use crate::send::METADATA_ALPN;
use crate::progress::{
    duration_ms, split_child_sizes, EmitThrottle, SpeedMeter, PROGRESS_MIN_BYTES,
    PROGRESS_MIN_SECS, SPEED_WINDOW_SECS,
};
use crate::time_compat::{sleep, timeout, Duration, Instant};
use crate::types::{get_or_create_secret, AppHandle, DiscoveryModeOption, FileMetadata, ReceiveOptions, SplitStrategy};
use iroh::endpoint::presets;
#[cfg(not(target_arch = "wasm32"))]
use iroh::{
    address_lookup::{dns::DnsAddressLookup, pkarr::PkarrResolver},
    Endpoint, TransportAddr, EndpointId,
};
#[cfg(target_arch = "wasm32")]
use iroh::{Endpoint, TransportAddr};
use iroh_blobs::{
    api::{remote::GetProgressItem, Store},
    format::collection::Collection,
    get::{request::get_hash_seq_and_sizes, GetError, Stats},
    ticket::BlobTicket,
};
#[cfg(not(target_arch = "wasm32"))]
use iroh_blobs::{
    api::remote::Remote,
    protocol::{ChunkRanges, ChunkRangesExt, GetRequest as BlobGetRequest},
};
#[cfg(not(target_arch = "wasm32"))]
use n0_future::FuturesUnordered;
use n0_future::StreamExt;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;
use std::str::FromStr;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
use tokio::io::AsyncReadExt;
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::mpsc as tokio_mpsc;

// Helper function to emit events through the app handle
fn emit_event(app_handle: &AppHandle, event_name: &str) {
    if let Some(handle) = app_handle {
        if let Err(e) = handle.emit_event(event_name) {
            tracing::warn!("Failed to emit event {}: {}", event_name, e);
        }
    }
}

// Helper function to emit progress events with payload
fn emit_progress_event(
    app_handle: &AppHandle,
    bytes_transferred: u64,
    total_bytes: u64,
    speed_bps: f64,
    concurrency: usize,
) {
    if let Some(handle) = app_handle {
        let event_name = "receive-progress";

        let payload = crate::progress::format_progress_payload(
            bytes_transferred,
            total_bytes,
            speed_bps,
            concurrency,
        );

        // Emit the event with appropriate payload
        if let Err(e) = handle.emit_event_with_payload(event_name, &payload) {
            tracing::warn!("Failed to emit progress event: {}", e);
        }
    }
}

// Helper function to emit events with payload
fn emit_event_with_payload(app_handle: &AppHandle, event_name: &str, payload: &str) {
    if let Some(handle) = app_handle {
        if let Err(e) = handle.emit_event_with_payload(event_name, payload) {
            tracing::warn!("Failed to emit event {} with payload: {}", event_name, e);
        }
    }
}

/// # Description
/// Receives metadata. This function will connect to the sender, request metadata, and return it without downloading
/// the file data.
/// # Returns
/// A `FileMetadata` struct containing the file name, size, thumbnail (if any), and MIME type (if any).
async fn receive_metadata<S: AsyncReadExt + Unpin>(
    stream: &mut S,
    app_handle: &AppHandle,
) -> anyhow::Result<FileMetadata> {
    // Read the length of the metadata (first 4 bytes)
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow::anyhow!("metadata read length failed: {e}"))?;
    let meta_len = u32::from_be_bytes(len_buf) as usize;
    tracing::debug!(meta_len, "receive_metadata: length prefix received");

    const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
    anyhow::ensure!(
        meta_len > 0 && meta_len <= MAX_METADATA_BYTES,
        "invalid metadata length: {meta_len}"
    );

    // Read the metadata JSON based on the length
    let mut meta_buf = vec![0u8; meta_len];
    stream
        .read_exact(&mut meta_buf)
        .await
        .map_err(|e| anyhow::anyhow!("metadata read body failed: {e}"))?;
    tracing::debug!(bytes = meta_buf.len(), "receive_metadata: body received");

    // Deserialize the metadata from JSON
    let metadata: FileMetadata = serde_json::from_slice(&meta_buf)
        .map_err(|e| anyhow::anyhow!("metadata json decode failed: {e}"))?;

    // Emit event with file metadata
    if let Some(emitter) = app_handle {
        if let Ok(payload) = serde_json::to_string(&metadata) {
            if let Err(e) = emitter.emit_event_with_payload("receive-file-metadata", &payload) {
                tracing::warn!("Failed to emit file metadata event: {}", e);
            }
        } else {
            tracing::warn!("Failed to serialize file metadata for event payload");
        }
    }

    Ok(metadata)
}

pub struct DownloadToStoreResult {
    pub collection: Collection,
    pub total_files: u64,
    pub payload_size: u64,
    pub stats: Stats,
    /// Wire time for the payload, excluding connection setup and disk export.
    pub download_duration_ms: u64,
}

/// Download ticket payload into the blob store (no filesystem export).
///
/// When `ReceiveOptions.split_strategy` is `Split`, fans out one request per
/// child blob; otherwise a single request streams the whole hash sequence.
pub async fn download_to_store(
    ticket: BlobTicket,
    addr: iroh::EndpointAddr,
    endpoint: &Endpoint,
    db: &Store,
    app_handle: &AppHandle,
    split_strategy: SplitStrategy,
    max_concurrency: Option<usize>,
) -> anyhow::Result<DownloadToStoreResult> {
    let hash_and_format = ticket.hash_and_format();
    let local = db.remote().local(hash_and_format).await?;

    let (stats, total_files, payload_size, download_duration_ms) = if !local.is_complete() {
        emit_event(app_handle, "receive-started");

        if split_strategy == SplitStrategy::Split {
            #[cfg(not(target_arch = "wasm32"))]
            {
                download_parallel(ticket, endpoint, db, app_handle, max_concurrency, &addr).await?
            }
            #[cfg(target_arch = "wasm32")]
            {
                // Browsers cap per-host streams and the wasm-io path cannot
                // spawn freely; stay sequential there.
                let _ = max_concurrency;
                download_sequential(&addr, endpoint, db, app_handle, hash_and_format, local).await?
            }
        } else {
            download_sequential(&addr, endpoint, db, app_handle, hash_and_format, local).await?
        }
    } else {
        let total_files = local.children().unwrap() - 1;
        let payload_bytes = 0;

        emit_event(app_handle, "receive-started");
        emit_event(app_handle, "receive-completed");

        (Stats::default(), total_files, payload_bytes, 0)
    };

    let collection = Collection::load(hash_and_format.hash, db).await?;

    let mut file_names: Vec<String> = Vec::new();
    for (name, _hash) in collection.iter() {
        file_names.push(name.to_string());
    }

    if !file_names.is_empty() {
        let file_names_json =
            serde_json::to_string(&file_names).unwrap_or_else(|_| "[]".to_string());
        emit_event_with_payload(app_handle, "receive-file-names", &file_names_json);
    }

    Ok(DownloadToStoreResult {
        collection,
        total_files,
        payload_size,
        stats,
        download_duration_ms,
    })
}

/// Sequential download — the original single-connection implementation.
async fn download_sequential(
    addr: &iroh::EndpointAddr,
    endpoint: &Endpoint,
    db: &Store,
    app_handle: &AppHandle,
    hash_and_format: iroh_blobs::HashAndFormat,
    local: iroh_blobs::api::remote::LocalInfo,
) -> anyhow::Result<(Stats, u64, u64, u64)> {
    let connection = match endpoint
        .connect(addr.clone(), iroh_blobs::protocol::ALPN)
        .await
    {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!("Connection failed: {}", e);
            tracing::error!("Error details: {:?}", e);
            tracing::error!("Tried to connect to node: {}", addr.id);
            tracing::error!("With relay: {:?}", addr.relay_urls().collect::<Vec<_>>());
            tracing::error!(
                "With direct addrs: {:?}",
                addr.ip_addrs().collect::<Vec<_>>()
            );
            return Err(anyhow::anyhow!("Connection failed: {}", e));
        }
    };

    let sizes_result =
        get_hash_seq_and_sizes(&connection, &hash_and_format.hash, 1024 * 1024 * 32, None)
            .await;

    let (hash_seq, sizes) = match sizes_result {
        Ok((hash_seq, sizes)) => (hash_seq, sizes),
        Err(e) => {
            tracing::error!("Failed to get sizes: {:?}", e);
            tracing::error!("Error type: {}", std::any::type_name_of_val(&e));
            return Err(show_get_error(e).into());
        }
    };
    // `sizes` holds children only (entry 0 is the collection metadata), but
    // the get stream counts the hash-seq root and that blob too — subtract
    // them to leave the same "file bytes" the sender reports.
    let root_bytes = (hash_seq.len() as u64).saturating_mul(32);
    let split = split_child_sizes(root_bytes, &sizes);
    let payload_size = split.payload_bytes;
    let total_files = (sizes.len().saturating_sub(1)) as u64;

    emit_progress_event(app_handle, 0, payload_size, 0.0, 1);

    let get = db.remote().execute_get(connection, local.missing());
    let mut stats = Stats::default();
    let mut stream = get.stream();
    let transfer_start_time = Instant::now();
    let mut speed = SpeedMeter::new(SPEED_WINDOW_SECS);
    let mut throttle = EmitThrottle::new(PROGRESS_MIN_BYTES, PROGRESS_MIN_SECS);
    let mut download_duration_ms = 0u64;

    while let Some(item) = stream.next().await {
        match item {
            GetProgressItem::Progress(offset) => {
                let elapsed = transfer_start_time.elapsed().as_secs_f64();
                let received = offset
                    .saturating_sub(split.overhead_bytes)
                    .min(payload_size);
                speed.record(elapsed, received);
                if throttle.should_emit(elapsed, received) {
                    emit_progress_event(
                        app_handle,
                        received,
                        payload_size,
                        speed.bytes_per_sec_at(elapsed),
                        1,
                    );
                }
            }
            GetProgressItem::Done(value) => {
                stats = value;

                let elapsed = transfer_start_time.elapsed().as_secs_f64();
                download_duration_ms = duration_ms(elapsed);
                speed.record(elapsed, payload_size);
                emit_progress_event(
                    app_handle,
                    payload_size,
                    payload_size,
                    speed.bytes_per_sec_at(elapsed),
                    1,
                );

                break;
            }
            GetProgressItem::Error(cause) => {
                tracing::error!("Download error: {:?}", cause);
                anyhow::bail!(show_get_error(cause));
            }
        }
    }

    Ok((stats, total_files, payload_size, download_duration_ms))
}

/// Extract provider endpoint IDs from a ticket's `EndpointAddr`.
///
/// The sender's endpoint ID (`addr.id`) is the only provider a ticket can name
/// today; relay and direct addresses both route to that same endpoint.
/// Kept as a seam so DHT/pkarr-based multi-source discovery can be added
/// without touching `download_parallel`.
#[cfg(not(target_arch = "wasm32"))]
fn extract_providers_from_addr(addr: &iroh::EndpointAddr) -> Vec<EndpointId> {
    vec![addr.id]
}

/// One blob-range download task for the parallel path.
#[cfg(not(target_arch = "wasm32"))]
struct BlobTask {
    /// Offset in the hash sequence: 0 is the root, 1 the collection metadata,
    /// 2.. are payload blobs.
    offset: u64,
    /// Chunk range to request. Whole-blob tasks use the full range; a large
    /// file is split into several disjoint range tasks.
    range: ChunkRanges,
    /// Payload bytes this task is expected to deliver, used for progress.
    size: u64,
}

/// BLAKE3 chunk size — the addressing unit of every `ChunkRanges`.
#[cfg(not(target_arch = "wasm32"))]
const BLOB_CHUNK_BYTES: u64 = 1024;
/// Large blobs are cut into pieces of roughly this size so one file can use
/// several parallel streams.
#[cfg(not(target_arch = "wasm32"))]
const BLOB_SPLIT_TARGET_BYTES: u64 = 16 * 1024 * 1024;
/// Cap pieces per blob: more streams than this stop helping and only add
/// scheduler and sender-side contention.
#[cfg(not(target_arch = "wasm32"))]
const BLOB_SPLIT_MAX_PIECES: u64 = 16;

/// Turn hash-seq child sizes into download tasks.
///
/// One task per child blob; children larger than `BLOB_SPLIT_TARGET_BYTES` are
/// cut into disjoint chunk-range tasks so a single huge file downloads through
/// several streams. The collection metadata (offset 1) is never split.
#[cfg(not(target_arch = "wasm32"))]
fn build_blob_tasks(sizes: &[u64]) -> Vec<BlobTask> {
    let mut tasks = Vec::new();
    for (child_idx, &size) in sizes.iter().enumerate() {
        let offset = child_idx as u64 + 1;
        if size == 0 {
            continue;
        }

        let chunks = size.div_ceil(BLOB_CHUNK_BYTES);
        let pieces = if offset >= 2 {
            chunks
                .div_ceil(BLOB_SPLIT_TARGET_BYTES / BLOB_CHUNK_BYTES)
                .clamp(1, BLOB_SPLIT_MAX_PIECES)
        } else {
            1
        };

        if pieces <= 1 {
            tasks.push(BlobTask {
                offset,
                range: ChunkRanges::all(),
                size,
            });
            continue;
        }

        let per_piece = chunks.div_ceil(pieces);
        let mut start = 0u64;
        while start < chunks {
            let end = (start + per_piece).min(chunks);
            let start_bytes = start * BLOB_CHUNK_BYTES;
            let piece_bytes = ((end - start) * BLOB_CHUNK_BYTES).min(size - start_bytes);
            tasks.push(BlobTask {
                offset,
                range: ChunkRanges::chunks(start..end),
                size: piece_bytes,
            });
            start = end;
        }
    }
    tasks
}

/// Payload bytes completed so far, excluding the hash-seq root and the
/// collection-metadata blob — the same accounting `split_child_sizes` uses.
#[cfg(not(target_arch = "wasm32"))]
fn payload_done(task_progress: &[u64], tasks: &[BlobTask], payload_size: u64) -> u64 {
    tasks
        .iter()
        .zip(task_progress.iter())
        .filter(|(task, _)| task.offset >= 2)
        .map(|(task, progress)| (*progress).min(task.size))
        .sum::<u64>()
        .min(payload_size)
}

/// Dynamic concurrency controller that adjusts parallelism based on observed
/// throughput. Samples are aggregate bytes/s over a fixed interval.
#[cfg(not(target_arch = "wasm32"))]
struct DynamicConcurrency {
    current: usize,
    min: usize,
    max: usize,
    /// Recent aggregate throughput samples (bytes/s), oldest first.
    throughput_history: VecDeque<u64>,
    /// Minimum time between adjustments, so a noisy sample can't thrash.
    min_adjustment_interval: Duration,
    last_adjustment: Option<Instant>,
}

#[cfg(not(target_arch = "wasm32"))]
impl DynamicConcurrency {
    fn new(min: usize, max: usize) -> Self {
        Self {
            current: max.min(16),
            min,
            max,
            throughput_history: VecDeque::with_capacity(6),
            min_adjustment_interval: Duration::from_secs(5),
            last_adjustment: None,
        }
    }

    /// Record a throughput sample and return a new concurrency target when the
    /// trend warrants a change. `None` means "keep the current value".
    fn maybe_adjust(&mut self, throughput_bps: f64) -> Option<usize> {
        let now = Instant::now();

        self.throughput_history
            .push_back(throughput_bps.max(0.0) as u64);
        while self.throughput_history.len() > 6 {
            self.throughput_history.pop_front();
        }

        if let Some(last) = self.last_adjustment {
            if now.duration_since(last) < self.min_adjustment_interval {
                return None;
            }
        }
        if self.throughput_history.len() < 6 {
            return None;
        }

        let samples: Vec<f64> = self
            .throughput_history
            .iter()
            .map(|bps| *bps as f64)
            .collect();
        let len = samples.len();
        let recent_avg = samples[len - 3..].iter().sum::<f64>() / 3.0;
        let older_avg = samples[len - 6..len - 3].iter().sum::<f64>() / 3.0;
        let trend = if older_avg > 0.0 {
            recent_avg / older_avg
        } else {
            1.0
        };

        if trend > 1.05 && self.current < self.max {
            self.current = (self.current + 1).min(self.max);
            self.last_adjustment = Some(now);
            tracing::debug!(
                concurrency = self.current,
                trend,
                throughput_bps = recent_avg,
                "parallel download: raising concurrency"
            );
            return Some(self.current);
        }

        if trend < 0.95 && recent_avg > 0.0 && self.current > self.min {
            self.current = (self.current - 1).max(self.min);
            self.last_adjustment = Some(now);
            tracing::debug!(
                concurrency = self.current,
                trend,
                throughput_bps = recent_avg,
                "parallel download: lowering concurrency"
            );
            return Some(self.current);
        }

        None
    }
}

/// Fetch one blob range request into the store, streaming progress updates
/// through `progress_tx` as `(task_index, bytes_read)`.
#[cfg(not(target_arch = "wasm32"))]
async fn run_blob_request(
    remote: Remote,
    connection: iroh::endpoint::Connection,
    request: BlobGetRequest,
    index: usize,
    progress_tx: tokio_mpsc::UnboundedSender<(usize, u64)>,
) -> (usize, anyhow::Result<Stats>) {
    let mut stream = remote.execute_get(connection, request).stream();
    while let Some(item) = stream.next().await {
        match item {
            GetProgressItem::Progress(offset) => {
                // A dropped receiver just means the transfer was cancelled.
                let _ = progress_tx.send((index, offset));
            }
            GetProgressItem::Done(stats) => return (index, Ok(stats)),
            GetProgressItem::Error(cause) => {
                return (index, Err(show_get_error(cause).into()));
            }
        }
    }
    (
        index,
        Err(anyhow::anyhow!("blob stream ended without a result")),
    )
}

/// Parallel download: one request per child blob over a shared QUIC
/// connection, with a concurrency ceiling that adapts to observed throughput.
///
/// Collection members download as independent tasks (many small files at
/// once), and a member blob larger than `BLOB_SPLIT_TARGET_BYTES` is cut into
/// disjoint chunk-range tasks, so a single huge file also uses several
/// streams. Memory stays flat: each request imports into the store as it
/// arrives, nothing is buffered in full.
#[cfg(not(target_arch = "wasm32"))]
async fn download_parallel(
    ticket: BlobTicket,
    endpoint: &Endpoint,
    db: &Store,
    app_handle: &AppHandle,
    max_concurrency: Option<usize>,
    addr: &iroh::EndpointAddr,
) -> anyhow::Result<(Stats, u64, u64, u64)> {
    let hash_and_format = ticket.hash_and_format();

    // A single connection carries every parallel bidi stream; the sender's
    // provider already serves concurrent streams per connection.
    let connection = endpoint
        .connect(addr.clone(), iroh_blobs::protocol::ALPN)
        .await
        .map_err(|e| anyhow::anyhow!("Parallel connection failed: {e}"))?;

    // Sizes probe before the fan-out: it gives per-child sizes, which make the
    // progress accounting exact instead of approximated from the wire count.
    let (_hash_seq, sizes) =
        match get_hash_seq_and_sizes(&connection, &hash_and_format.hash, 1024 * 1024 * 32, None)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to get sizes for parallel download: {:?}", e);
                return Err(show_get_error(e).into());
            }
        };

    let root_bytes = (sizes.len() as u64).saturating_mul(32);
    let split = split_child_sizes(root_bytes, &sizes);
    let payload_size = split.payload_bytes;
    let total_files = sizes.len().saturating_sub(1) as u64;

    emit_progress_event(app_handle, 0, payload_size, 0.0, 1);

    // Chunk-level resume: a partial store is finished over a single stream,
    // where `local.missing()` asks for exactly the chunks still absent. The
    // fan-out below would re-request whole ranges, so it only runs on a fresh
    // store — the common case — and on failure we fall back to this same
    // resumable path (see below).
    let local = db.remote().local(hash_and_format).await?;
    if local.is_complete() {
        return Ok((Stats::default(), total_files, payload_size, 0));
    }
    if local.local_bytes() > 0 {
        tracing::info!(
            present_bytes = local.local_bytes(),
            "parallel download: partial store present, resuming sequentially"
        );
        return download_sequential(addr, endpoint, db, app_handle, hash_and_format, local).await;
    }

    let remote = db.remote().clone();
    let provider_ids = extract_providers_from_addr(addr);
    tracing::debug!(
        providers = provider_ids.len(),
        payload_size,
        files = total_files,
        "parallel download: starting"
    );

    // 1. Root hash-seq first: child requests resolve their hashes from it in
    //    the local store, so it must be present before the fan-out.
    let root_request = BlobGetRequest::builder()
        .root(ChunkRanges::all())
        .build(hash_and_format.hash);
    {
        let mut stream = remote
            .execute_get(connection.clone(), root_request)
            .stream();
        while let Some(item) = stream.next().await {
            match item {
                GetProgressItem::Progress(_) => {}
                GetProgressItem::Done(_) => break,
                GetProgressItem::Error(cause) => {
                    tracing::warn!(
                        "parallel: root hash-seq fetch failed, falling back to sequential: {cause}"
                    );
                    let local = db.remote().local(hash_and_format).await?;
                    return download_sequential(
                        addr,
                        endpoint,
                        db,
                        app_handle,
                        hash_and_format,
                        local,
                    )
                    .await;
                }
            }
        }
    }

    // 2. Fan out one task per child blob, splitting large files into disjoint
    //    chunk-range tasks so a single huge file also downloads in parallel.
    let tasks: Vec<BlobTask> = build_blob_tasks(&sizes);

    if tasks.is_empty() {
        let elapsed_ms = duration_ms(0.0);
        emit_progress_event(app_handle, payload_size, payload_size, 0.0, 1);
        return Ok((Stats::default(), total_files, payload_size, elapsed_ms));
    }

    // Initial concurrency: small transfers don't benefit from a wide fan-out,
    // and we never open more streams than there are blobs.
    let default_concurrency = if payload_size < 10 * 1024 * 1024 {
        4
    } else if payload_size < 1024 * 1024 * 1024 {
        16
    } else {
        32
    };
    let initial_concurrency = max_concurrency
        .unwrap_or(default_concurrency)
        .clamp(2, 32)
        .min(tasks.len());

    let target = Arc::new(AtomicUsize::new(initial_concurrency));
    let mut dyn_concurrency = DynamicConcurrency::new(2, 32);
    dyn_concurrency.current = initial_concurrency;

    let (progress_tx, mut progress_rx) = tokio_mpsc::unbounded_channel::<(usize, u64)>();

    let transfer_start = Instant::now();
    let mut speed = SpeedMeter::new(SPEED_WINDOW_SECS);
    let mut throttle = EmitThrottle::new(PROGRESS_MIN_BYTES, PROGRESS_MIN_SECS);
    let mut adjust_timer = tokio::time::interval(Duration::from_secs(2));
    adjust_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    adjust_timer.tick().await; // the first tick fires immediately; consume it

    let mut in_flight = FuturesUnordered::new();
    let mut next_idx = 0usize;
    let mut task_progress: Vec<u64> = vec![0; tasks.len()];
    let mut total_stats = Stats::default();
    let mut failure: Option<anyhow::Error> = None;

    loop {
        // Top up to the current concurrency target. A lowered target simply
        // stops new work until the in-flight set drains below it.
        let target_now = target.load(Ordering::Relaxed).max(1);
        while in_flight.len() < target_now && next_idx < tasks.len() {
            let idx = next_idx;
            next_idx += 1;
            let request = BlobGetRequest::builder()
                .offset(tasks[idx].offset, tasks[idx].range.clone())
                .build(hash_and_format.hash);
            in_flight.push(run_blob_request(
                remote.clone(),
                connection.clone(),
                request,
                idx,
                progress_tx.clone(),
            ));
        }

        if in_flight.is_empty() && next_idx >= tasks.len() {
            break;
        }

        // Snapshot before `select!` pins `in_flight.next()` mutably; it still
        // tracks the live fan-out because new tasks are only spawned above.
        let active_streams = in_flight.len().max(1);

        tokio::select! {
            biased;

            Some((idx, offset)) = progress_rx.recv() => {
                if offset > task_progress[idx] {
                    task_progress[idx] = offset;
                }
                let done = payload_done(&task_progress, &tasks, payload_size);
                let elapsed = transfer_start.elapsed().as_secs_f64();
                speed.record(elapsed, done);
                if throttle.should_emit(elapsed, done) {
                    emit_progress_event(
                        app_handle,
                        done,
                        payload_size,
                        speed.bytes_per_sec_at(elapsed),
                        active_streams,
                    );
                }
            }

            Some((idx, result)) = in_flight.next() => {
                match result {
                    Ok(stats) => {
                        // A finished blob counts as fully transferred.
                        task_progress[idx] = tasks[idx].size;
                        total_stats.combine(&stats);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "parallel blob request failed, finishing sequentially: {error:#}"
                        );
                        failure = Some(error);
                        break;
                    }
                }
                let done = payload_done(&task_progress, &tasks, payload_size);
                let elapsed = transfer_start.elapsed().as_secs_f64();
                speed.record(elapsed, done);
                if throttle.should_emit(elapsed, done) {
                    emit_progress_event(
                        app_handle,
                        done,
                        payload_size,
                        speed.bytes_per_sec_at(elapsed),
                        active_streams,
                    );
                }
            }

            _ = adjust_timer.tick() => {
                let elapsed = transfer_start.elapsed().as_secs_f64();
                let throughput = speed.bytes_per_sec_at(elapsed);
                if let Some(new_target) = dyn_concurrency.maybe_adjust(throughput) {
                    // Never exceed the number of pending blobs.
                    let capped = new_target.min(tasks.len()).max(1);
                    target.store(capped, Ordering::Relaxed);
                }
            }
        }
    }

    if let Some(_error) = failure {
        // Partial blobs are already in the store; the sequential path resumes
        // from exactly where this left off.
        drop(in_flight);
        let local = db.remote().local(hash_and_format).await?;
        return download_sequential(addr, endpoint, db, app_handle, hash_and_format, local).await;
    }

    let elapsed = transfer_start.elapsed().as_secs_f64();
    let elapsed_ms = duration_ms(elapsed);
    emit_progress_event(
        app_handle,
        payload_size,
        payload_size,
        speed.bytes_per_sec_at(elapsed),
        1,
    );

    Ok((total_stats, total_files, payload_size, elapsed_ms))
}

/// # Description
/// Fetches metadata for a given ticket without downloading the file data. This is used to display file information (name, size, thumbnail) in the UI before the user decides to download.
/// # Returns
/// A `FileMetadata` struct containing the file name, size, and preview metadata (if any).
pub async fn fetch_metadata(
    ticket_str: String,
    options: ReceiveOptions,
) -> anyhow::Result<FileMetadata> {
    // parse ticket and extract address
    let ticket = BlobTicket::from_str(&ticket_str)?;
    let addr = ticket.addr().clone();

    // Create a temporary endpoint to connect and fetch metadata
    let secret_key = get_or_create_secret()?;

    let discovery_mode = options.discovery_mode.clone();
    let builder = match &discovery_mode {
        DiscoveryModeOption::Custom { .. } => Endpoint::builder(presets::Minimal),
        DiscoveryModeOption::Default => Endpoint::builder(presets::N0),
    };
    let custom_infra =
        crate::tls_config::uses_custom_infra(&discovery_mode, &options.relay_mode);
    let mut builder = crate::tls_config::with_system_ca_if_custom(builder, custom_infra)
        // METADATA_ALPN only to indicate a metadata fetch
        .alpns(vec![METADATA_ALPN.to_vec()])
        .secret_key(secret_key)
        .relay_mode(options.relay_mode.into());

    #[cfg(not(target_arch = "wasm32"))]
    {
        if ticket.addr().relay_urls().count() == 0 && ticket.addr().ip_addrs().count() == 0 {
            builder = match &discovery_mode {
                DiscoveryModeOption::Custom {
                    pkarr_relay_url,
                    dns_origin,
                } => {
                    let mut builder =
                        builder.address_lookup(PkarrResolver::builder(pkarr_relay_url.clone()));
                    if let Some(origin) = dns_origin {
                        builder =
                            builder.address_lookup(DnsAddressLookup::builder(origin.clone()));
                    }
                    builder
                }
                DiscoveryModeOption::Default => {
                    builder.address_lookup(DnsAddressLookup::n0_dns())
                }
            };
        }
        if let Some(addr) = options.magic_ipv4_addr {
            builder = builder.bind_addr(addr)?;
        }
        if let Some(addr) = options.magic_ipv6_addr {
            builder = builder.bind_addr(addr)?;
        }
    }

    let endpoint = builder.bind().await?;

    // Attempt connection and metadata fetch up to 3 times
    let mut attempt_plan: Vec<(usize, &'static str, iroh::EndpointAddr)> = vec![
        (1, "default", addr.clone()),
        (2, "default", addr.clone()),
        (3, "default", addr.clone()),
    ];

    // Relay-only attempt if relay addresses are avaliable
    let mut relay_only_addr = addr.clone();
    relay_only_addr
        .addrs
        .retain(|transport_addr| matches!(transport_addr, TransportAddr::Relay(_)));
    if !relay_only_addr.addrs.is_empty() {
        attempt_plan[2] = (3, "relay-only", relay_only_addr);
    }

    let mut last_error: Option<anyhow::Error> = None;
    let mut attempt_errors: Vec<(usize, &'static str, String)> = Vec::new();

    for (attempt, path, target_addr) in attempt_plan {
        tracing::info!(attempt, path, "fetch_metadata: connecting to sender");

        let result: anyhow::Result<FileMetadata> = async {
            let connection = timeout(
                Duration::from_secs(15),
                endpoint.connect(target_addr, METADATA_ALPN),
            )
            .await
            .map_err(|_| anyhow::anyhow!("metadata connect timeout"))??;

            tracing::debug!(attempt, path, "fetch_metadata: connection established");

            let (mut send_stream, mut recv_stream) =
                timeout(Duration::from_secs(20), connection.open_bi())
                    .await
                    .map_err(|_| anyhow::anyhow!("metadata open_bi timeout"))??;

            tracing::debug!(attempt, path, "fetch_metadata: bi stream opened");

            // Send 1 byte as a marker to indicate metadata request
            timeout(Duration::from_secs(10), send_stream.write_all(&[1]))
                .await
                .map_err(|_| anyhow::anyhow!("metadata request write timeout"))??;

            tracing::debug!(attempt, path, "fetch_metadata: request marker sent");

            let metadata = timeout(
                Duration::from_secs(20),
                receive_metadata(&mut recv_stream, &None),
            )
            .await
            .map_err(|_| anyhow::anyhow!("metadata read timeout"))??;

            // Finish send_stream only AFTER receiving the metadata.
            // signals the server that we are safely done and it can drop the connection.
            let _ = send_stream.finish();

            Ok(metadata)
        }
        .await;

        match result {
            Ok(metadata) => {
                tracing::info!(
                    attempt,
                    path,
                    retries = attempt_errors.len(),
                    file_name = %metadata.file_name,
                    size = metadata.size,
                    "fetch_metadata: received metadata"
                );
                endpoint.close().await;
                return Ok(metadata);
            }
            Err(err) => {
                let will_retry = attempt < 3;
                tracing::debug!(
                    attempt,
                    path,
                    will_retry,
                    error = %err,
                    "fetch_metadata attempt failed"
                );
                attempt_errors.push((attempt, path, err.to_string()));
                last_error = Some(err);
                if will_retry {
                    sleep(Duration::from_millis(300)).await;
                }
            }
        }
    }

    endpoint.close().await;

    if !attempt_errors.is_empty() {
        let failure_summary = attempt_errors
            .iter()
            .map(|(attempt, path, err)| format!("#{attempt}({path}): {err}"))
            .collect::<Vec<_>>()
            .join(" | ");

        if let Some(ref err) = last_error {
            tracing::warn!(
                attempts = attempt_errors.len(),
                error = %err,
                failure_summary = %failure_summary,
                "fetch_metadata: failed to connect to sender"
            );
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("metadata fetch failed")))
}

fn show_get_error(e: GetError) -> GetError {
    match &e {
        GetError::InitialNext { source, .. } => {
            tracing::error!("initial connection error: {source}");
        }
        GetError::ConnectedNext { source, .. } => {
            tracing::error!("connected error: {source}");
        }
        GetError::AtBlobHeaderNext { source, .. } => {
            tracing::error!("reading blob header error: {source}");
        }
        GetError::Decode { source, .. } => {
            tracing::error!("decoding error: {source}");
        }
        GetError::IrpcSend { source, .. } => {
            tracing::error!("error sending over irpc: {source}");
        }
        GetError::AtClosingNext { source, .. } => {
            tracing::error!("error at closing: {source}");
        }
        GetError::BadRequest { .. } => {
            tracing::error!("bad request");
        }
        GetError::LocalFailure { source, .. } => {
            tracing::error!("local failure {source:?}");
        }
    }
    e
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod parallel_tests {
    use super::*;

    fn task(offset: u64, size: u64) -> BlobTask {
        BlobTask {
            offset,
            range: ChunkRanges::all(),
            size,
        }
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn provider_list_is_the_sender_endpoint() {
        let id = iroh::SecretKey::generate().public();
        let addr = iroh::EndpointAddr {
            id,
            addrs: Default::default(),
        };

        assert_eq!(extract_providers_from_addr(&addr), vec![id]);
    }

    #[test]
    fn small_blobs_become_one_whole_blob_task_each() {
        let tasks = build_blob_tasks(&[500, 1000]);

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].offset, 1);
        assert_eq!(tasks[0].size, 500);
        assert_eq!(tasks[0].range, ChunkRanges::all());
        assert_eq!(tasks[1].offset, 2);
        assert_eq!(tasks[1].size, 1000);
        assert_eq!(tasks[1].range, ChunkRanges::all());
    }

    #[test]
    fn empty_blobs_are_skipped() {
        assert!(build_blob_tasks(&[0, 0]).is_empty());
    }

    #[test]
    fn large_file_is_split_into_disjoint_pieces_covering_all_chunks() {
        let size = 40 * MIB;
        let tasks = build_blob_tasks(&[500, size]);
        let pieces: Vec<&BlobTask> = tasks.iter().filter(|t| t.offset == 2).collect();

        assert!(pieces.len() > 1, "40 MiB file must be split, got {}", pieces.len());
        assert_eq!(
            pieces.iter().map(|t| t.size).sum::<u64>(),
            size,
            "pieces must cover exactly the file"
        );
        // Disjoint union of all piece ranges covers the file's full chunk range.
        // (`ChunkRanges::all()` is unbounded, so compare against 0..chunks.)
        let union = pieces
            .iter()
            .fold(ChunkRanges::empty(), |acc, t| acc | t.range.clone());
        let chunks = size.div_ceil(BLOB_CHUNK_BYTES);
        assert_eq!(union, ChunkRanges::chunks(0..chunks));
    }

    #[test]
    fn large_file_split_is_capped_at_max_pieces() {
        let huge = 64 * 1024 * MIB; // 64 GiB
        let tasks = build_blob_tasks(&[500, huge]);
        let pieces = tasks.iter().filter(|t| t.offset == 2).count();

        assert_eq!(pieces, BLOB_SPLIT_MAX_PIECES as usize);
        assert_eq!(
            tasks
                .iter()
                .filter(|t| t.offset == 2)
                .map(|t| t.size)
                .sum::<u64>(),
            huge
        );
    }

    #[test]
    fn collection_metadata_is_never_split() {
        let tasks = build_blob_tasks(&[64 * MIB]);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].offset, 1);
        assert_eq!(tasks[0].range, ChunkRanges::all());
    }

    #[test]
    fn payload_done_excludes_root_and_collection_metadata() {
        // offset 0 root, offset 1 metadata, offsets 2.. payload
        let tasks = [task(0, 64), task(1, 500), task(2, 1000), task(3, 2000)];
        let progress = [64, 500, 1000, 2000];

        assert_eq!(payload_done(&progress, &tasks, 3000), 3000);
    }

    #[test]
    fn payload_done_counts_partial_blobs_up_to_their_size() {
        let tasks = [task(2, 1000), task(3, 2000)];
        let progress = [400, 5000]; // second offset overshoots its blob

        assert_eq!(payload_done(&progress, &tasks, 3000), 2400);
    }

    #[test]
    fn payload_done_never_exceeds_the_payload_size() {
        let tasks = [task(2, 1000)];
        let progress = [1000];

        assert_eq!(payload_done(&progress, &tasks, 500), 500);
    }

    #[test]
    fn concurrency_rises_on_an_improving_throughput_trend() {
        let mut dc = DynamicConcurrency::new(2, 32);
        dc.current = 8;
        dc.throughput_history =
            VecDeque::from([1_000_000, 1_000_000, 1_000_000, 2_000_000, 2_000_000]);
        dc.last_adjustment = None;

        // Pushing 2 MB/s keeps the last three samples above the older ones.
        assert_eq!(dc.maybe_adjust(2_000_000.0), Some(9));
    }

    #[test]
    fn concurrency_falls_on_a_declining_throughput_trend() {
        let mut dc = DynamicConcurrency::new(2, 32);
        dc.current = 8;
        dc.throughput_history =
            VecDeque::from([2_000_000, 2_000_000, 2_000_000, 1_000_000, 1_000_000]);
        dc.last_adjustment = None;

        assert_eq!(dc.maybe_adjust(1_000_000.0), Some(7));
    }

    #[test]
    fn concurrency_does_not_thrash_between_adjustments() {
        let mut dc = DynamicConcurrency::new(2, 32);
        dc.current = 8;
        dc.throughput_history =
            VecDeque::from([1_000_000, 1_000_000, 1_000_000, 2_000_000, 2_000_000]);
        dc.last_adjustment = Some(Instant::now());

        assert_eq!(dc.maybe_adjust(2_000_000.0), None);
    }

    #[test]
    fn concurrency_stays_within_bounds() {
        let mut at_max = DynamicConcurrency::new(2, 8);
        at_max.current = 8;
        at_max.throughput_history =
            VecDeque::from([1_000_000, 1_000_000, 1_000_000, 2_000_000, 2_000_000]);
        at_max.last_adjustment = None;
        assert_eq!(at_max.maybe_adjust(2_000_000.0), None);

        let mut at_min = DynamicConcurrency::new(4, 32);
        at_min.current = 4;
        at_min.throughput_history =
            VecDeque::from([2_000_000, 2_000_000, 2_000_000, 1_000_000, 1_000_000]);
        at_min.last_adjustment = None;
        assert_eq!(at_min.maybe_adjust(1_000_000.0), None);
    }

    #[test]
    fn concurrency_needs_a_full_window_before_adjusting() {
        let mut dc = DynamicConcurrency::new(2, 32);
        dc.current = 8;
        dc.throughput_history = VecDeque::from([1_000_000, 2_000_000]);
        dc.last_adjustment = None;

        assert_eq!(dc.maybe_adjust(2_000_000.0), None);
    }
}
