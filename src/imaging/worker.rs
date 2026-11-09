//! CPU-bound image scaling, panning, and resizing worker pools.

use fast_image_resize as fir;
use image::DynamicImage;
use ratatui_image::picker::Picker;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use super::decoder::{
    CancellationToken, decode_image_bytes_with_header_cancellable,
    decode_thumbnail_and_dimensions_for_mode,
};
use super::kitty_shm::{KittyShmProtocol, ViewportProtocol};
use super::raw::{
    RAW_EXIF_PROBE_LIMIT, probe_raw_header, read_source_bytes_limited,
    read_source_bytes_with_header,
};
#[cfg(test)]
use super::thumbcache::cache_key_for_path;
use super::thumbcache::{
    has_valid_cached_thumbnail, load_cached_thumbnail_with_metadata,
    save_cached_thumbnail_with_dims,
};
use super::types::*;

/// A request payload sent to the resizing worker threads.
pub struct ResizeRequest {
    /// Shared reference to the decoded dynamic image.
    pub img: Arc<DynamicImage>,
    /// Expected full-resolution dimensions of the image.
    /// If the actual image buffer dimensions differ, it indicates a thumbnail placeholder.
    pub original_size: (u32, u32),
    /// Calculated scaling factor.
    pub scale: f64,
    /// Crop box geometry.
    pub crop: CropBox,
    /// Clamped image intersection region.
    pub intersection: ImageIntersection,
    /// Resized target width.
    pub target_w: u32,
    /// Resized target height.
    pub target_h: u32,
    /// Desired scaling filter.
    pub filter_type: FilterType,
    /// Protocol generator picker.
    pub picker: Picker,
    /// Brightness correction value.
    pub brightness: Brightness,
    /// Contrast correction percentage.
    pub contrast: Contrast,
    /// Output terminal grid cell dimensions.
    pub rendered_size_cells: (u16, u16),
    /// Sequence identifier to filter out stale requests.
    pub sequence: u64,
    /// Whether POSIX shared memory (`t=s`) should be used for Kitty protocol rendering.
    pub use_kitty_shm: bool,
}

/// A response payload sent by the resizing worker thread.
pub struct ResizeResponse {
    /// Rendered viewport protocol.
    pub protocol: ViewportProtocol,
    /// Terminal cells occupied by the rendered image `(columns, rows)`.
    pub rendered_cells: (u16, u16),
    /// Wall-clock time spent processing/cropping/resizing the image.
    pub process_duration: std::time::Duration,
    /// Wall-clock time spent creating the graphics protocol payload.
    pub protocol_duration: std::time::Duration,
    /// Target width in pixels.
    pub target_width: u32,
    /// Target height in pixels.
    pub target_height: u32,
    /// Generation sequence number.
    pub sequence: u64,
}

#[cfg(test)]
pub use super::types::{fast_lanczos3_kernel, fast_mitchell_kernel};
pub use super::types::{maybe_reclaim_resizer_scratch, to_fir_resize_alg};

/// Spawns the persistent background viewport resizer worker thread.
pub fn spawn_resize_worker(
    resize_rx: Receiver<ResizeRequest>,
    protocol_tx: Sender<ResizeResponse>,
    resize_active_seq: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || {
        let mut resizer = fir::Resizer::new();
        while let Ok(req) = resize_rx.recv() {
            let mut latest_req = req;
            while let Ok(next_req) = resize_rx.try_recv() {
                latest_req = next_req;
            }
            if (latest_req.sequence as usize) < resize_active_seq.load(Ordering::Acquire) {
                continue;
            }
            let rendered_cells = latest_req.rendered_size_cells;
            let target_w = latest_req.target_w;
            let target_h = latest_req.target_h;
            let sequence = latest_req.sequence;
            let (protocol, proc_dur, proto_dur) = process_resize(latest_req, &mut resizer);
            if (sequence as usize) >= resize_active_seq.load(Ordering::Acquire) {
                let _ = protocol_tx.send(ResizeResponse {
                    protocol,
                    rendered_cells,
                    process_duration: proc_dur,
                    protocol_duration: proto_dur,
                    target_width: target_w,
                    target_height: target_h,
                    sequence,
                });
            }
        }
    });
}

/// Maximum number of concurrent full-image decode worker threads in the loader pool.
pub const MAX_DECODE_WORKERS: usize = 5;

/// Maximum capacity for the non-blocking background thumbnail writer queue.
pub const MAX_THUMB_WRITE_QUEUE: usize = 8;

/// Work item for background Freedesktop thumbnail writing.
struct ThumbWriteJob {
    path: PathBuf,
    image: Arc<DynamicImage>,
    source_dims: (u32, u32),
}

/// Shared state for the background thumbnail-writer thread.
struct ThumbWriteQueue {
    pending: VecDeque<ThumbWriteJob>,
    shutdown: bool,
}

/// Enqueues a thumbnail generation job into the bounded writer queue.
///
/// Discards images whose dimensions are already within standard thumbnail boundaries (`<= 256x256`),
/// deduplicates jobs matching existing pending paths, and applies non-blocking backpressure
/// by popping the oldest unwritten job when `queue.pending.len() >= max_capacity`.
fn enqueue_thumb_write_job(queue: &mut ThumbWriteQueue, job: ThumbWriteJob, max_capacity: usize) {
    if queue.shutdown || (job.image.width() <= 256 && job.image.height() <= 256) {
        return;
    }
    if queue
        .pending
        .iter()
        .any(|existing| existing.path == job.path)
    {
        return;
    }
    if queue.pending.len() >= max_capacity {
        queue.pending.pop_front();
    }
    queue.pending.push_back(job);
}

/// Returns the number of parallel full-image decode worker threads to spawn (`2..=5`).
pub fn decode_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, MAX_DECODE_WORKERS))
        .unwrap_or(2)
}

/// Internal pending decode job enqueued for parallel decode workers.
struct DecodeJob {
    idx: usize,
    source: ImageSource,
    mode: LoaderMode,
    sequence: u64,
    bytes_opt: Option<Vec<u8>>,
    raw_header_opt: Option<Vec<u8>>,
    local_path: Option<PathBuf>,
    has_thumbnail: bool,
}

/// Represents an active decode job executing in a parallel worker slot.
struct InFlightSlot {
    idx: usize,
    sequence: Arc<AtomicU64>,
    mode: LoaderMode,
    has_thumbnail: bool,
    cancel_flag: Arc<AtomicBool>,
}

/// Shared coordinator-worker synchronization state.
struct DecodeQueueState {
    pending: VecDeque<DecodeJob>,
    in_flight: Vec<Option<InFlightSlot>>,
    shutdown: bool,
}

/// Predicate checking if the next job in `pending` can be popped by an idle worker.
///
/// Ensures only 1 foreground decode executes at any given time so intermediate scroll steps
/// coalesce in `pending` in 0ns, while reserving at least 1 idle worker for foreground execution
/// by limiting active prefetch threads to `max_workers.saturating_sub(1).max(1)`.
fn can_pop_job(state: &DecodeQueueState, max_workers: usize) -> bool {
    let Some(front) = state.pending.front() else {
        return false;
    };
    let any_foreground_in_flight = state
        .in_flight
        .iter()
        .flatten()
        .any(|slot| !slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire));
    if any_foreground_in_flight {
        return false;
    }
    if !front.mode.is_prefetch() {
        true
    } else {
        let active_prefetches = state
            .in_flight
            .iter()
            .flatten()
            .filter(|slot| slot.mode.is_prefetch())
            .count();
        active_prefetches < max_workers.saturating_sub(1).max(1)
    }
}

/// Spawns the 2-stage image loader pool: 1 fast-thumbnail coordinator thread and
/// `decode_worker_count()` parallel full-resolution decode worker threads.
pub fn spawn_loader_pool(
    loader_rx: Receiver<LoaderRequest>,
    response_tx: Sender<LoaderResponse>,
    active_seq: Arc<AtomicUsize>,
    disable_thumbnail: bool,
    thumb_cache_root: Option<PathBuf>,
) {
    let num_workers = decode_worker_count();
    let queue_state = Arc::new((
        Mutex::new(DecodeQueueState {
            pending: VecDeque::new(),
            in_flight: (0..num_workers).map(|_| None).collect(),
            shutdown: false,
        }),
        Condvar::new(),
    ));

    let thumb_writer_queue: Option<Arc<(Mutex<ThumbWriteQueue>, Condvar)>> =
        if !disable_thumbnail && let Some(ref cache_root) = thumb_cache_root {
            let queue = Arc::new((
                Mutex::new(ThumbWriteQueue {
                    pending: VecDeque::new(),
                    shutdown: false,
                }),
                Condvar::new(),
            ));
            let queue_clone = Arc::clone(&queue);
            let cache_root = cache_root.clone();

            std::thread::spawn(move || {
                let mut thumb_resizer = fir::Resizer::new();
                let (lock, cvar) = &*queue_clone;
                loop {
                    let job = {
                        let mut state = lock.lock().unwrap();
                        while state.pending.is_empty() && !state.shutdown {
                            state = cvar.wait(state).unwrap();
                        }
                        if state.pending.is_empty() && state.shutdown {
                            break;
                        }
                        state.pending.pop_front()
                    };

                    if let Some(job) = job
                        && !has_valid_cached_thumbnail(&cache_root, &job.path)
                    {
                        save_cached_thumbnail_with_dims(
                            &cache_root,
                            &mut thumb_resizer,
                            &job.path,
                            &job.image,
                            job.source_dims,
                        );
                    }
                }
            });

            Some(queue)
        } else {
            None
        };

    // Spawn Stage 2 parallel decode workers
    for worker_id in 0..num_workers {
        let queue_pair = Arc::clone(&queue_state);
        let response_tx = response_tx.clone();
        let active_seq = Arc::clone(&active_seq);
        let thumb_writer_queue = thumb_writer_queue.clone();

        std::thread::spawn(move || {
            loop {
                let (job, cancel_flag, slot_seq) = {
                    let (lock, cvar) = &*queue_pair;
                    let mut state = lock.lock().unwrap();
                    while !can_pop_job(&state, num_workers) && !state.shutdown {
                        state = cvar.wait(state).unwrap();
                    }
                    if state.shutdown {
                        break;
                    }
                    let job = state.pending.pop_front().unwrap();
                    let cancel_flag = Arc::new(AtomicBool::new(false));
                    let slot_seq = Arc::new(AtomicU64::new(job.sequence));
                    state.in_flight[worker_id] = Some(InFlightSlot {
                        idx: job.idx,
                        sequence: Arc::clone(&slot_seq),
                        mode: job.mode,
                        has_thumbnail: job.has_thumbnail,
                        cancel_flag: Arc::clone(&cancel_flag),
                    });
                    (job, cancel_flag, slot_seq)
                };

                let cancel = CancellationToken::from_flag(&cancel_flag);

                let is_aborted = || {
                    cancel.is_cancelled()
                        || (slot_seq.load(Ordering::Acquire) as usize)
                            < active_seq.load(Ordering::Acquire)
                };

                if is_aborted() {
                    let (lock, cvar) = &*queue_pair;
                    let mut state = lock.lock().unwrap();
                    state.in_flight[worker_id] = None;
                    if !state.pending.is_empty() && can_pop_job(&state, num_workers) {
                        cvar.notify_all();
                    }
                    continue;
                }

                let decode_start = Instant::now();
                let read_result = if let Some(bytes) = job.bytes_opt {
                    Ok(bytes)
                } else {
                    read_source_bytes_with_header(&job.source, job.raw_header_opt.as_deref())
                };

                let res = if is_aborted() {
                    Err("Cancelled".to_string())
                } else {
                    match read_result {
                        Ok(ref bytes) => decode_image_bytes_with_header_cancellable(
                            bytes,
                            &job.source,
                            job.raw_header_opt.as_deref(),
                            &cancel,
                        ),
                        Err(e) => Err(e),
                    }
                };
                let decode_duration = decode_start.elapsed();

                let (final_seq, final_mode, has_thumb) = {
                    let (lock, cvar) = &*queue_pair;
                    let mut state = lock.lock().unwrap();
                    let finished_slot = state.in_flight[worker_id].take();
                    let final_seq = finished_slot
                        .as_ref()
                        .map(|s| s.sequence.load(Ordering::Acquire))
                        .unwrap_or(job.sequence);
                    let final_mode = finished_slot.as_ref().map(|s| s.mode).unwrap_or(job.mode);
                    let has_thumb = finished_slot
                        .as_ref()
                        .map(|s| s.has_thumbnail)
                        .unwrap_or(job.has_thumbnail);
                    if !state.pending.is_empty() && can_pop_job(&state, num_workers) {
                        cvar.notify_all();
                    }
                    (final_seq, final_mode, has_thumb)
                };

                let is_cancelled = cancel.is_cancelled()
                    || (final_seq as usize) < active_seq.load(Ordering::Acquire);

                if !is_cancelled {
                    if let Ok(ref decoded) = res {
                        let thumb_job = if !has_thumb
                            && !disable_thumbnail
                            && (decoded.width > 256 || decoded.height > 256)
                            && let Some(ref path) = job.local_path
                            && thumb_writer_queue.is_some()
                        {
                            Some(ThumbWriteJob {
                                path: path.clone(),
                                image: Arc::clone(&decoded.image),
                                source_dims: decoded
                                    .raw_dimensions
                                    .unwrap_or((decoded.width, decoded.height)),
                            })
                        } else {
                            None
                        };

                        let _ = response_tx.send(LoaderResponse {
                            idx: job.idx,
                            result: res,
                            is_prefetch: final_mode.is_prefetch(),
                            sequence: final_seq,
                            decode_duration,
                            thumbnail_source: None,
                        });

                        if let Some(t_job) = thumb_job
                            && let Some(ref queue_pair) = thumb_writer_queue
                        {
                            let (ref lock, ref cvar) = **queue_pair;
                            let mut q = lock.lock().unwrap();
                            enqueue_thumb_write_job(&mut q, t_job, MAX_THUMB_WRITE_QUEUE);
                            cvar.notify_one();
                        }
                    } else if let Err(e) = res
                        && e != "Cancelled"
                    {
                        let _ = response_tx.send(LoaderResponse {
                            idx: job.idx,
                            result: Err(e),
                            is_prefetch: final_mode.is_prefetch(),
                            sequence: final_seq,
                            decode_duration,
                            thumbnail_source: None,
                        });
                    }
                }
            }
        });
    }

    // Spawn Stage 1 fast-thumbnail coordinator thread
    let queue_pair = queue_state;
    let thumb_writer_queue_coord = thumb_writer_queue;
    std::thread::spawn(move || {
        let mut pending_requests = Vec::new();
        let mut requests: Vec<LoaderRequest> = Vec::with_capacity(16);
        let mut current_requests: VecDeque<LoaderRequest> = VecDeque::with_capacity(16);
        let mut new_reqs: Vec<LoaderRequest> = Vec::with_capacity(16);
        let mut seen_indices = std::collections::HashSet::new();

        loop {
            requests.clear();
            if pending_requests.is_empty() {
                match loader_rx.recv() {
                    Ok(req) => requests.push(req),
                    Err(_) => {
                        let (lock, cvar) = &*queue_pair;
                        let mut state = lock.lock().unwrap();
                        state.shutdown = true;
                        state.pending.clear();
                        for slot in state.in_flight.iter_mut().flatten() {
                            slot.cancel_flag.store(true, Ordering::Release);
                        }
                        cvar.notify_all();
                        if let Some(ref thumb_q) = thumb_writer_queue_coord {
                            let (ref lock, ref cvar) = **thumb_q;
                            let mut state = lock.lock().unwrap();
                            state.shutdown = true;
                            cvar.notify_all();
                        }
                        break;
                    }
                }
            } else {
                requests.append(&mut pending_requests);
            }

            while let Ok(r) = loader_rx.try_recv() {
                requests.push(r);
            }

            let highest_seq = requests.iter().map(|r| r.sequence).max().unwrap_or(0);
            let mut matching: Vec<LoaderRequest> = requests
                .drain(..)
                .filter(|r| r.sequence == highest_seq)
                .collect();
            matching.sort_by_key(|r| r.mode.is_prefetch());
            seen_indices.clear();
            matching.retain(|r| seen_indices.insert(r.idx));
            current_requests.clear();
            current_requests.extend(matching);

            // Reconcile DecodeQueueState with current_requests
            {
                let (lock, cvar) = &*queue_pair;
                let mut state = lock.lock().unwrap();

                let is_fast_scroll_batch =
                    current_requests.iter().any(|r| r.mode.is_thumbnail_only());

                for slot in state.in_flight.iter_mut().flatten() {
                    if is_fast_scroll_batch {
                        slot.cancel_flag.store(true, Ordering::Release);
                    } else if !slot.cancel_flag.load(Ordering::Acquire)
                        && let Some(matching_req) =
                            current_requests.iter().find(|r| r.idx == slot.idx)
                    {
                        let prev_seq = slot.sequence.swap(highest_seq, Ordering::AcqRel);
                        if prev_seq < highest_seq || !matching_req.mode.is_prefetch() {
                            slot.mode = matching_req.mode;
                        }
                    } else if slot.sequence.load(Ordering::Acquire) < highest_seq {
                        slot.cancel_flag.store(true, Ordering::Release);
                    }
                }

                if !is_fast_scroll_batch
                    && current_requests.iter().any(|r| !r.mode.is_prefetch())
                    && !state.in_flight.iter().flatten().any(|slot| {
                        !slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire)
                    })
                {
                    let max_prefetch = num_workers.saturating_sub(1).max(1);
                    let mut active_prefetches = state
                        .in_flight
                        .iter()
                        .flatten()
                        .filter(|slot| {
                            slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire)
                        })
                        .count();
                    if active_prefetches > max_prefetch {
                        for slot in state.in_flight.iter_mut().flatten() {
                            if active_prefetches <= max_prefetch {
                                break;
                            }
                            if slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire)
                            {
                                slot.cancel_flag.store(true, Ordering::Release);
                                active_prefetches -= 1;
                            }
                        }
                    }
                }

                if is_fast_scroll_batch {
                    state.pending.clear();
                } else {
                    state.pending.retain(|job| {
                        job.sequence == highest_seq
                            || current_requests.iter().any(|r| r.idx == job.idx)
                    });

                    for job in &mut state.pending {
                        if let Some(matching_req) =
                            current_requests.iter().find(|r| r.idx == job.idx)
                        {
                            if job.sequence < highest_seq || !matching_req.mode.is_prefetch() {
                                job.mode = matching_req.mode;
                            }
                            job.sequence = highest_seq;
                        }
                    }

                    let (foreground, prefetch): (VecDeque<_>, VecDeque<_>) = state
                        .pending
                        .drain(..)
                        .partition(|job| !job.mode.is_prefetch());
                    state.pending.extend(foreground);
                    state.pending.extend(prefetch);
                }

                if can_pop_job(&state, num_workers) {
                    cvar.notify_all();
                }
            }

            while let Some(r) = current_requests.pop_front() {
                let cancel = CancellationToken::with_sequence(r.sequence as usize, &active_seq);
                if cancel.is_cancelled() {
                    continue;
                }

                new_reqs.clear();
                while let Ok(nr) = loader_rx.try_recv() {
                    new_reqs.push(nr);
                }
                if !new_reqs.is_empty() {
                    let new_max_seq = new_reqs.iter().map(|nr| nr.sequence).max().unwrap_or(0);
                    let has_foreground = new_reqs.iter().any(|nr| !nr.mode.is_prefetch());
                    pending_requests.append(&mut new_reqs);
                    if new_max_seq > highest_seq {
                        current_requests.clear();
                        break;
                    } else if has_foreground {
                        pending_requests.extend(current_requests.drain(..));
                        break;
                    }
                }

                if cancel.is_cancelled() {
                    continue;
                }

                let (already_queued, thumb_already_extracted) = {
                    let (lock, _) = &*queue_pair;
                    let state = lock.lock().unwrap();
                    let in_flight_slot = state
                        .in_flight
                        .iter()
                        .flatten()
                        .find(|s| s.idx == r.idx && !s.cancel_flag.load(Ordering::Acquire));
                    let pending_job = state.pending.iter().find(|job| job.idx == r.idx);

                    let already_queued = in_flight_slot.is_some() || pending_job.is_some();
                    let thumb_already_extracted =
                        in_flight_slot.map(|s| s.has_thumbnail).unwrap_or(false)
                            || pending_job.map(|j| j.has_thumbnail).unwrap_or(false);
                    (already_queued, thumb_already_extracted)
                };

                let should_extract =
                    r.mode.should_extract_thumbnail(disable_thumbnail) && !thumb_already_extracted;

                if already_queued && !should_extract {
                    continue;
                }

                let start = Instant::now();
                let mut bytes_opt: Option<Vec<u8>> = None;
                let mut raw_header_opt: Option<Vec<u8>> = None;
                let is_raw = r.source.is_raw();
                let limit = if is_raw {
                    RAW_EXIF_PROBE_LIMIT
                } else {
                    256 * 1024
                };

                let local_path = match &r.source {
                    ImageSource::Local(p) => Some(p.clone()),
                    _ => None,
                };

                let mut has_thumbnail = false;

                if r.mode.should_extract_thumbnail(disable_thumbnail) {
                    if cancel.is_cancelled() {
                        continue;
                    }
                    let read_res = read_source_bytes_limited(&r.source, limit);
                    if let Ok(partial_bytes) = read_res {
                        if cancel.is_cancelled() {
                            continue;
                        }
                        if let Some((thumb_img, real_w, real_h)) =
                            decode_thumbnail_and_dimensions_for_mode(
                                &partial_bytes,
                                Some(&r.source),
                                r.mode.is_thumbnail_only(),
                            )
                            && !cancel.is_cancelled()
                        {
                            let thumb_dur = start.elapsed();
                            let _ = response_tx.send(LoaderResponse {
                                idx: r.idx,
                                result: Ok(DecodedImage {
                                    image: Arc::new(thumb_img),
                                    width: real_w,
                                    height: real_h,
                                    format: Some(image::ImageFormat::Jpeg),
                                    disk_size: 0,
                                    raw_dimensions: Some((real_w, real_h)),
                                }),
                                is_prefetch: r.mode.is_prefetch(),
                                sequence: r.sequence,
                                decode_duration: thumb_dur,
                                thumbnail_source: Some(ThumbnailSource::Exif),
                            });
                            has_thumbnail = true;
                        }
                        if !is_raw && partial_bytes.len() < limit {
                            bytes_opt = Some(partial_bytes);
                        } else if is_raw {
                            raw_header_opt = Some(partial_bytes);
                        }
                    }

                    if !has_thumbnail
                        && !cancel.is_cancelled()
                        && let Some(ref path) = local_path
                        && let Some(ref cache_root) = thumb_cache_root
                        && let Some(cached) = load_cached_thumbnail_with_metadata(cache_root, path)
                    {
                        let (mut real_w, mut real_h) = if let Some(dims) = cached.source_dims {
                            dims
                        } else if is_raw && let Some(ref hb) = raw_header_opt {
                            let probe = probe_raw_header(hb, Some(&r.source));
                            if let Some((w, h)) = probe.sensor_dims {
                                if orientation_swaps_dimensions(probe.orientation) {
                                    (h, w)
                                } else {
                                    (w, h)
                                }
                            } else {
                                image::image_dimensions(path)
                                    .unwrap_or((cached.image.width(), cached.image.height()))
                            }
                        } else {
                            image::image_dimensions(path)
                                .unwrap_or((cached.image.width(), cached.image.height()))
                        };
                        if cached.image.width() != cached.image.height()
                            && (real_w > real_h) != (cached.image.width() > cached.image.height())
                        {
                            std::mem::swap(&mut real_w, &mut real_h);
                        }
                        let thumb_dur = start.elapsed();
                        let _ = response_tx.send(LoaderResponse {
                            idx: r.idx,
                            result: Ok(DecodedImage {
                                image: Arc::new(cached.image),
                                width: real_w,
                                height: real_h,
                                format: Some(image::ImageFormat::Png),
                                disk_size: 0,
                                raw_dimensions: Some((real_w, real_h)),
                            }),
                            is_prefetch: r.mode.is_prefetch(),
                            sequence: r.sequence,
                            decode_duration: thumb_dur,
                            thumbnail_source: Some(ThumbnailSource::XdgCache),
                        });
                        has_thumbnail = true;
                    }
                }

                if r.mode.is_thumbnail_only() && has_thumbnail {
                    continue;
                }

                if cancel.is_cancelled() || r.sequence < active_seq.load(Ordering::Acquire) as u64 {
                    continue;
                }

                let (lock, cvar) = &*queue_pair;
                let mut state = lock.lock().unwrap();

                let mut already_in_flight = false;
                for slot in state.in_flight.iter_mut().flatten() {
                    if slot.idx == r.idx && !slot.cancel_flag.load(Ordering::Acquire) {
                        already_in_flight = true;
                        slot.has_thumbnail |= has_thumbnail;
                        let prev_seq = slot.sequence.swap(r.sequence, Ordering::AcqRel);
                        if prev_seq < r.sequence || !r.mode.is_prefetch() {
                            slot.mode = r.mode;
                        }
                    }
                }

                let mut already_pending = false;
                for job in &mut state.pending {
                    if job.idx == r.idx {
                        already_pending = true;
                        job.has_thumbnail |= has_thumbnail;
                        if job.sequence < r.sequence || !r.mode.is_prefetch() {
                            job.mode = r.mode;
                        }
                        job.sequence = r.sequence;
                    }
                }

                if already_pending && !r.mode.is_prefetch() {
                    let (foreground, prefetch): (VecDeque<_>, VecDeque<_>) = state
                        .pending
                        .drain(..)
                        .partition(|job| !job.mode.is_prefetch());
                    state.pending.extend(foreground);
                    state.pending.extend(prefetch);
                    cvar.notify_one();
                }

                if !already_in_flight && !already_pending {
                    let job = DecodeJob {
                        idx: r.idx,
                        source: r.source,
                        mode: r.mode,
                        sequence: r.sequence,
                        bytes_opt,
                        raw_header_opt,
                        local_path,
                        has_thumbnail,
                    };
                    if !r.mode.is_prefetch() {
                        state.pending.push_front(job);
                        cvar.notify_one();
                    } else {
                        state.pending.push_back(job);
                        if can_pop_job(&state, num_workers) {
                            cvar.notify_all();
                        }
                    }
                }
            }

            requests.clear();
            if requests.capacity() > 64 {
                requests.shrink_to(16);
            }
            current_requests.clear();
            if current_requests.capacity() > 64 {
                current_requests.shrink_to(16);
            }
            new_reqs.clear();
            if new_reqs.capacity() > 64 {
                new_reqs.shrink_to(16);
            }
            if pending_requests.capacity() > 64 {
                pending_requests.shrink_to(16);
            }
        }
    });
}

/// Helper resizing function leveraging fast_image_resize for sub-millisecond execution speeds.
pub fn fast_resize(
    resizer: &mut fir::Resizer,
    src_img: &DynamicImage,
    dst_w: u32,
    dst_h: u32,
    filter_type: FilterType,
    crop_rect: Option<(f64, f64, f64, f64)>,
) -> Result<DynamicImage, Box<dyn std::error::Error>> {
    use fast_image_resize::images::Image as FirImage;

    let resize_alg = to_fir_resize_alg(filter_type);

    let (crop_w, crop_h) = match crop_rect {
        Some((_, _, w, h)) => (w, h),
        None => (src_img.width() as f64, src_img.height() as f64),
    };
    let is_magnified = dst_w as f64 >= crop_w && dst_h as f64 >= crop_h;
    let needs_anti_ringing =
        is_magnified && matches!(filter_type, FilterType::Lanczos | FilterType::Mitchell);

    match src_img {
        DynamicImage::ImageRgb8(rgb_src) => {
            let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x3);

            let mut options = fir::ResizeOptions::new();
            options.algorithm = resize_alg;
            options.mul_div_alpha = false;
            if let Some((left, top, width, height)) = crop_rect {
                options = options.crop(left, top, width, height);
            }

            resizer.resize(rgb_src, &mut dst_image, Some(&options))?;
            maybe_reclaim_resizer_scratch(resizer, src_img.width(), src_img.height(), dst_w, dst_h);

            let rgb_dst = image::RgbImage::from_raw(dst_w, dst_h, dst_image.into_vec())
                .ok_or("Failed to construct RgbImage from resized buffer")?;

            let rgba_raw = unpack_rgb24_to_rgba32(&rgb_dst);
            let mut rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, rgba_raw)
                .ok_or("Failed to construct RgbaImage from unpacked buffer")?;

            if needs_anti_ringing {
                clamp_anti_ringing_rgb(&mut rgba_dst, rgb_src, crop_rect);
            }

            Ok(DynamicImage::ImageRgba8(rgba_dst))
        }
        _ => {
            let rgba_src = to_rgba8_fast(src_img);

            let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x4);

            let mut options = fir::ResizeOptions::new();
            options.algorithm = resize_alg;
            options.mul_div_alpha = true;
            if let Some((left, top, width, height)) = crop_rect {
                options = options.crop(left, top, width, height);
            }

            resizer.resize(rgba_src.as_ref(), &mut dst_image, Some(&options))?;
            maybe_reclaim_resizer_scratch(resizer, src_img.width(), src_img.height(), dst_w, dst_h);

            let buffer = dst_image.into_vec();
            let mut rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, buffer)
                .ok_or("Failed to construct RgbaImage from resized buffer")?;

            if needs_anti_ringing {
                clamp_anti_ringing_rgba(&mut rgba_dst, &rgba_src, crop_rect);
            }

            Ok(DynamicImage::ImageRgba8(rgba_dst))
        }
    }
}

/// Renders the scaled and panned image onto the destination canvas, applying
/// offscreen boundary margins, centering, and brightness/contrast adjustments.
pub fn render_resize_canvas(req: &ResizeRequest, resizer: &mut fir::Resizer) -> DynamicImage {
    // Map input crop/intersection coordinates from the full image space to the thumbnail space
    // if the loaded image buffer is a thumbnail placeholder of different dimensions.
    let (scale, crop, intersection) =
        if req.img.width() != req.original_size.0 || req.img.height() != req.original_size.1 {
            let factor_x = req.img.width() as f64 / req.original_size.0 as f64;
            let factor_y = req.img.height() as f64 / req.original_size.1 as f64;

            let crop_x1 = (req.crop.x1 as f64 * factor_x).round() as i64;
            let crop_y1 = (req.crop.y1 as f64 * factor_y).round() as i64;
            let crop_x2 = (req.crop.x2 as f64 * factor_x).round() as i64;
            let crop_y2 = (req.crop.y2 as f64 * factor_y).round() as i64;
            let scaled_crop = CropBox::new(crop_x1, crop_y1, crop_x2, crop_y2);

            let inter_x1 =
                ((req.intersection.x1 as f64 * factor_x).round() as u32).min(req.img.width());
            let inter_y1 =
                ((req.intersection.y1 as f64 * factor_y).round() as u32).min(req.img.height());
            let inter_x2 =
                ((req.intersection.x2 as f64 * factor_x).round() as u32).min(req.img.width());
            let inter_y2 =
                ((req.intersection.y2 as f64 * factor_y).round() as u32).min(req.img.height());
            let scaled_inter = ImageIntersection::new(inter_x1, inter_y1, inter_x2, inter_y2);

            let new_scale = req.target_w as f64 / scaled_crop.width().max(1) as f64;

            (new_scale, scaled_crop, scaled_inter)
        } else {
            (req.scale, req.crop, req.intersection)
        };
    let img_to_resize: &DynamicImage = &req.img;

    if intersection.x1 as i64 == crop.x1
        && intersection.x2 as i64 == crop.x2
        && intersection.y1 as i64 == crop.y1
        && intersection.y2 as i64 == crop.y2
    {
        let crop_rect = Some((
            intersection.x1 as f64,
            intersection.y1 as f64,
            intersection.width() as f64,
            intersection.height() as f64,
        ));
        let mut canvas = match fast_resize(
            resizer,
            img_to_resize,
            req.target_w,
            req.target_h,
            req.filter_type,
            crop_rect,
        ) {
            Ok(resized) => resized,
            Err(_) => {
                let cropped_part = img_to_resize.crop_imm(
                    intersection.x1,
                    intersection.y1,
                    intersection.width(),
                    intersection.height(),
                );
                cropped_part.resize(
                    req.target_w,
                    req.target_h,
                    req.filter_type.to_image_filter(),
                )
            }
        };

        if let Some(rgba_canvas) = canvas.as_mut_rgba8() {
            apply_adjustments_lut(
                rgba_canvas.as_mut(),
                req.brightness.value(),
                req.contrast.value(),
            );
        }
        canvas
    } else {
        let mut screen_canvas = image::RgbaImage::new(req.target_w, req.target_h);

        if !intersection.is_empty() {
            let target_inter_w = ((intersection.width() as f64 * scale).round() as u32).max(1);
            let target_inter_h = ((intersection.height() as f64 * scale).round() as u32).max(1);

            let crop_rect = Some((
                intersection.x1 as f64,
                intersection.y1 as f64,
                intersection.width() as f64,
                intersection.height() as f64,
            ));

            let resized_part = match fast_resize(
                resizer,
                img_to_resize,
                target_inter_w,
                target_inter_h,
                req.filter_type,
                crop_rect,
            ) {
                Ok(resized) => resized,
                Err(_) => {
                    let cropped_part = img_to_resize.crop_imm(
                        intersection.x1,
                        intersection.y1,
                        intersection.width(),
                        intersection.height(),
                    );
                    cropped_part.resize(
                        target_inter_w,
                        target_inter_h,
                        req.filter_type.to_image_filter(),
                    )
                }
            };

            let paste_x = ((intersection.x1 as i64 - crop.x1) as f64 * scale).round() as i64;
            let paste_y = ((intersection.y1 as i64 - crop.y1) as f64 * scale).round() as i64;

            let paste_x =
                paste_x.clamp(0, (req.target_w as i64 - target_inter_w as i64).max(0)) as u32;
            let paste_y =
                paste_y.clamp(0, (req.target_h as i64 - target_inter_h as i64).max(0)) as u32;

            let copy_w = target_inter_w.min(req.target_w.saturating_sub(paste_x));
            let copy_h = target_inter_h.min(req.target_h.saturating_sub(paste_y));

            if copy_w > 0 && copy_h > 0 {
                let mut src_rgba = match resized_part {
                    DynamicImage::ImageRgba8(rgba) => rgba,
                    other => into_rgba8_fast(other),
                };
                let src_stride = (src_rgba.width() as usize) * 4;
                let row_bytes = (copy_w as usize) * 4;
                let active_src_len = ((copy_h as usize) - 1) * src_stride + row_bytes;
                apply_adjustments_lut(
                    &mut src_rgba.as_mut()[..active_src_len],
                    req.brightness.value(),
                    req.contrast.value(),
                );
                let src_raw = src_rgba.as_raw();
                let dest_stride = (req.target_w as usize) * 4;
                let dest_raw = screen_canvas.as_mut();

                for y in 0..(copy_h as usize) {
                    let src_start = y * src_stride;
                    let src_slice = &src_raw[src_start..src_start + row_bytes];
                    let dest_start =
                        ((paste_y as usize) + y) * dest_stride + (paste_x as usize) * 4;
                    dest_raw[dest_start..dest_start + row_bytes].copy_from_slice(src_slice);
                }
            }
        }
        DynamicImage::ImageRgba8(screen_canvas)
    }
}

/// Processes a scaling and panning request in the background, creating/rendering
/// the final scaled viewport on a screen-pixel canvas block to support offscreen panning boundaries.
pub fn process_resize(
    req: ResizeRequest,
    resizer: &mut fir::Resizer,
) -> (ViewportProtocol, std::time::Duration, std::time::Duration) {
    let rendered_cells = req.rendered_size_cells;
    let start_process = std::time::Instant::now();
    let canvas = render_resize_canvas(&req, resizer);
    let process_duration = start_process.elapsed();

    let start_protocol = std::time::Instant::now();
    let target_size = ratatui::layout::Size::new(rendered_cells.0, rendered_cells.1);
    let protocol = if req.use_kitty_shm
        && req.picker.protocol_type() == ratatui_image::picker::ProtocolType::Kitty
    {
        match KittyShmProtocol::new(
            &canvas,
            req.picker.font_size(),
            target_size,
            req.picker.tmux_detected(),
        ) {
            Ok(shm_proto) => ViewportProtocol::KittyShm(shm_proto),
            Err(_) => {
                let mut p = req.picker.new_resize_protocol(canvas);
                ratatui_image::ResizeEncodeRender::resize_encode(
                    &mut p,
                    &ratatui_image::Resize::Crop(None),
                    target_size,
                );
                ViewportProtocol::Standard(p)
            }
        }
    } else {
        let mut p = req.picker.new_resize_protocol(canvas);
        ratatui_image::ResizeEncodeRender::resize_encode(
            &mut p,
            &ratatui_image::Resize::Crop(None),
            target_size,
        );
        ViewportProtocol::Standard(p)
    };
    let protocol_duration = start_protocol.elapsed();

    (protocol, process_duration, protocol_duration)
}

/// Applies brightness adjustment to RGBA pixel buffer in-place while strictly preserving the Alpha channel.
pub fn apply_brightness_simd(pixels: &mut [u8], value: i32) {
    if value == 0 {
        return;
    }
    if value > 0 {
        let delta = value.min(255) as u8;
        let mask: [u8; 16] = [
            delta, delta, delta, 0, delta, delta, delta, 0, delta, delta, delta, 0, delta, delta,
            delta, 0,
        ];
        let (chunks16, tail) = pixels.as_chunks_mut::<16>();
        for chunk in chunks16 {
            for i in 0..16 {
                chunk[i] = chunk[i].saturating_add(mask[i]);
            }
        }
        for chunk in tail.as_chunks_mut::<4>().0 {
            chunk[0] = chunk[0].saturating_add(delta);
            chunk[1] = chunk[1].saturating_add(delta);
            chunk[2] = chunk[2].saturating_add(delta);
        }
    } else {
        let delta = (-value).min(255) as u8;
        let mask: [u8; 16] = [
            delta, delta, delta, 0, delta, delta, delta, 0, delta, delta, delta, 0, delta, delta,
            delta, 0,
        ];
        let (chunks16, tail) = pixels.as_chunks_mut::<16>();
        for chunk in chunks16 {
            for i in 0..16 {
                chunk[i] = chunk[i].saturating_sub(mask[i]);
            }
        }
        for chunk in tail.as_chunks_mut::<4>().0 {
            chunk[0] = chunk[0].saturating_sub(delta);
            chunk[1] = chunk[1].saturating_sub(delta);
            chunk[2] = chunk[2].saturating_sub(delta);
        }
    }
}

/// Computes the dynamic soft-knee compression threshold based on the maximum highlight peak.
///
/// Matches `peng`'s `mix(1.0, 0.75, min(overshoot * 0.75, 1.0))` (`image.frag.glsl:746-757`):
/// when `overshoot == 0.0`, `knee == 1.0` (no highlight compression); as `overshoot` increases,
/// `knee` smoothly transitions from `1.0` down to `0.75`.
#[inline]
pub fn compute_dynamic_knee(peak: f32) -> f32 {
    let overshoot = (peak - 1.0).max(0.0);
    let t = (overshoot * 0.75).min(1.0);
    1.0 - 0.25 * t
}

/// Numerically stable continuous rational highlight soft-knee curve with parameterized knee threshold.
///
/// Smoothly compresses highlight values above `knee` towards an asymptote of `1.0`,
/// preventing harsh clipping transitions while ensuring $C^1$ continuity and numerical stability.
#[inline]
pub fn soft_knee_highlight_with_knee(x: f32, knee: f32) -> f32 {
    let knee = knee.clamp(0.75, 1.0);
    let w = 1.0 - knee;
    if w <= 1e-5 || x <= knee {
        x.max(0.0)
    } else {
        let excess = x - knee;
        knee + (w * excess) / (excess + w)
    }
}

/// Numerically stable continuous rational highlight soft-knee curve from `peng` with fixed `knee = 0.75`.
///
/// Smoothly compresses highlight values above `knee = 0.75` towards an asymptote of `1.0`,
/// preventing harsh clipping transitions while ensuring $C^1$ continuity and complete
/// numerical stability (free of NaN, Inf, or overflow even for extreme inputs).
#[inline]
pub fn soft_knee_highlight(x: f32) -> f32 {
    soft_knee_highlight_with_knee(x, 0.75)
}

/// Applies fused brightness and contrast adjustments to an RGBA pixel buffer in a single memory pass
/// using a 256-byte stack LUT, incorporating `peng`'s dynamic soft-knee highlight compression while
/// strictly preserving Alpha.
pub fn apply_adjustments_lut(pixels: &mut [u8], brightness: i32, contrast: f32) {
    if brightness == 0 && contrast == 0.0 {
        return;
    }
    if contrast == 0.0 {
        apply_brightness_simd(pixels, brightness);
        return;
    }

    let b_offset = brightness as f32 / 255.0;
    let factor = ((100.0 + contrast) / 100.0).max(0.0);
    let factor = factor * factor;
    let peak = (1.0 + b_offset - 0.5) * factor + 0.5;
    let knee = compute_dynamic_knee(peak);

    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let normalized = i as f32 / 255.0 + b_offset;
        let mut val = (normalized - 0.5) * factor + 0.5;
        if peak > 1.0 {
            val = if (knee - 0.75).abs() < 1e-5 {
                soft_knee_highlight(val)
            } else {
                soft_knee_highlight_with_knee(val, knee)
            };
        }
        *slot = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
    }

    for chunk in pixels.as_chunks_mut::<4>().0 {
        chunk[0] = lut[chunk[0] as usize];
        chunk[1] = lut[chunk[1] as usize];
        chunk[2] = lut[chunk[2] as usize];
    }
}

/// Applies contrast adjustment to an RGBA pixel buffer using a 256-byte stack LUT incorporating
/// `peng`'s photometric contrast curve and soft-knee shoulder compression while preserving Alpha.
#[cfg(test)]
pub fn apply_contrast_lut(pixels: &mut [u8], contrast: f32) {
    apply_adjustments_lut(pixels, 0, contrast);
}

/// Clamps destination pixel colors to the min/max bounding envelope of the corresponding
/// 2x2 source texel neighborhood to eliminate Gibbs ringing/overshoot artifacts from lobed filters.
pub fn clamp_anti_ringing_rgba(
    dst: &mut image::RgbaImage,
    src: &image::RgbaImage,
    crop_rect: Option<(f64, f64, f64, f64)>,
) {
    clamp_anti_ringing_span_cached::<4>(
        dst,
        src.as_raw().as_chunks::<4>().0,
        src.width(),
        src.height(),
        crop_rect,
    );
}

/// Clamps destination pixel colors to the min/max bounding envelope of the corresponding
/// 2x2 source texel neighborhood of an RGB source image to eliminate Gibbs ringing/overshoot artifacts.
pub fn clamp_anti_ringing_rgb(
    dst: &mut image::RgbaImage,
    src: &image::RgbImage,
    crop_rect: Option<(f64, f64, f64, f64)>,
) {
    clamp_anti_ringing_span_cached::<3>(
        dst,
        src.as_raw().as_chunks::<3>().0,
        src.width(),
        src.height(),
        crop_rect,
    );
}

fn clamp_anti_ringing_span_cached<const N: usize>(
    dst: &mut image::RgbaImage,
    src_pixels: &[[u8; N]],
    src_w: u32,
    src_h: u32,
    crop_rect: Option<(f64, f64, f64, f64)>,
) {
    let dst_w = dst.width();
    let dst_h = dst.height();

    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }

    let (crop_left, crop_top, crop_w, crop_h) =
        crop_rect.unwrap_or((0.0, 0.0, src_w as f64, src_h as f64));
    let scale_x = crop_w / dst_w as f64;
    let scale_y = crop_h / dst_h as f64;

    if scale_x > 1.0 || scale_y > 1.0 {
        return;
    }

    // Pre-tabulate 1D horizontal source bounds for each destination x coordinate
    let mut x_bounds = Vec::with_capacity(dst_w as usize);
    for x in 0..dst_w {
        let src_x_f = crop_left + (x as f64 + 0.5) * scale_x - 0.5;
        let x0 = (src_x_f.floor() as i64).clamp(0, src_w as i64 - 1) as usize;
        let x1 = (x0 + 1).min(src_w as usize - 1);
        x_bounds.push((x0, x1));
    }

    let src_w_usize = src_w as usize;
    let dst_pixels = dst.as_mut().as_chunks_mut::<4>().0;

    // Process rows with horizontal span caching
    for (y, dst_row) in dst_pixels.chunks_exact_mut(dst_w as usize).enumerate() {
        let src_y_f = crop_top + (y as f64 + 0.5) * scale_y - 0.5;
        let y0 = (src_y_f.floor() as i64).clamp(0, src_h as i64 - 1) as usize;
        let y1 = (y0 + 1).min(src_h as usize - 1);

        let row0 = &src_pixels[y0 * src_w_usize..(y0 + 1) * src_w_usize];
        let row1 = &src_pixels[y1 * src_w_usize..(y1 + 1) * src_w_usize];

        let mut prev_x0 = usize::MAX;
        let mut min_rgb = [0u8; 3];
        let mut max_rgb = [0u8; 3];

        for (dst_pixel, &(x0, x1)) in dst_row.iter_mut().zip(&x_bounds) {
            if x0 != prev_x0 {
                prev_x0 = x0;
                let p00 = row0[x0];
                let p10 = row0[x1];
                let p01 = row1[x0];
                let p11 = row1[x1];

                min_rgb[0] = p00[0].min(p10[0]).min(p01[0]).min(p11[0]);
                max_rgb[0] = p00[0].max(p10[0]).max(p01[0]).max(p11[0]);

                min_rgb[1] = p00[1].min(p10[1]).min(p01[1]).min(p11[1]);
                max_rgb[1] = p00[1].max(p10[1]).max(p01[1]).max(p11[1]);

                min_rgb[2] = p00[2].min(p10[2]).min(p01[2]).min(p11[2]);
                max_rgb[2] = p00[2].max(p10[2]).max(p01[2]).max(p11[2]);
            }

            dst_pixel[0] = dst_pixel[0].clamp(min_rgb[0], max_rgb[0]);
            dst_pixel[1] = dst_pixel[1].clamp(min_rgb[1], max_rgb[1]);
            dst_pixel[2] = dst_pixel[2].clamp(min_rgb[2], max_rgb[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::kitty_shm::cleanup_all_kitty_shm_segments;
    use super::*;
    use image::{Rgba, RgbaImage};
    use ratatui_image::picker::Picker;
    use std::sync::Arc;

    fn make_test_image(width: u32, height: u32) -> DynamicImage {
        let mut img = RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    fn make_gray_test_image(width: u32, height: u32, val: u8) -> DynamicImage {
        let mut img = RgbaImage::new(width, height);
        for pixel in img.pixels_mut() {
            *pixel = Rgba([val, val, val, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn test_fast_resize_filters_downscale_and_upscale() {
        let mut resizer = fir::Resizer::new();
        let src = make_test_image(100, 100);

        let filters = [
            FilterType::Nearest,
            FilterType::Box,
            FilterType::Linear,
            FilterType::Mitchell,
            FilterType::Lanczos,
        ];

        for filter in filters {
            // Downscale
            let downscaled =
                fast_resize(&mut resizer, &src, 40, 30, filter, None).expect("Downscale failed");
            assert_eq!(downscaled.width(), 40);
            assert_eq!(downscaled.height(), 30);

            // Upscale
            let upscaled =
                fast_resize(&mut resizer, &src, 200, 150, filter, None).expect("Upscale failed");
            assert_eq!(upscaled.width(), 200);
            assert_eq!(upscaled.height(), 150);
        }
    }

    #[test]
    fn test_fast_resize_with_crop() {
        let mut resizer = fir::Resizer::new();
        let src = make_test_image(100, 100);

        let crop = Some((20.0, 20.0, 40.0, 40.0));
        let resized = fast_resize(&mut resizer, &src, 80, 80, FilterType::Lanczos, crop)
            .expect("Crop resize failed");
        assert_eq!(resized.width(), 80);
        assert_eq!(resized.height(), 80);
    }

    #[test]
    fn test_process_resize_fully_contained() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(100, 100));
        let picker = Picker::halfblocks();

        let req = ResizeRequest {
            img: src,
            original_size: (100, 100),
            scale: 0.5,
            crop: CropBox::new(0, 0, 100, 100),
            intersection: ImageIntersection::new(0, 0, 100, 100),
            target_w: 50,
            target_h: 50,
            filter_type: FilterType::Nearest,
            picker: picker.clone(),
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (5, 3),
            sequence: 1,
            use_kitty_shm: false,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 50);
        assert_eq!(canvas.height(), 50);
        let rgba = canvas.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0)[3], 255);
        assert_eq!(rgba.get_pixel(25, 25)[3], 255);
        assert_eq!(rgba.get_pixel(49, 49)[3], 255);

        let (proto, proc_dur, proto_dur) = process_resize(req, &mut resizer);
        assert!(proc_dur.as_nanos() > 0 || proc_dur.as_secs() == 0);
        assert!(proto_dur.as_nanos() > 0 || proto_dur.as_secs() == 0);
        let target_size = ratatui::layout::Size::new(5, 3);
        assert!(
            ratatui_image::ResizeEncodeRender::needs_resize(
                &proto,
                &ratatui_image::Resize::Crop(None),
                target_size
            )
            .is_none(),
            "Expected protocol to be pre-encoded for rendered_size_cells"
        );
    }

    #[test]
    fn test_process_resize_kitty_shm_branch_and_fallback() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(80, 80));

        // 1. Non-kitty picker with use_kitty_shm = true falls back to ViewportProtocol::Standard
        let picker_halfblocks = Picker::halfblocks();
        let req_fallback = ResizeRequest {
            img: Arc::clone(&src),
            original_size: (80, 80),
            scale: 1.0,
            crop: CropBox::new(0, 0, 80, 80),
            intersection: ImageIntersection::new(0, 0, 80, 80),
            target_w: 80,
            target_h: 80,
            filter_type: FilterType::Nearest,
            picker: picker_halfblocks,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (10, 10),
            sequence: 1,
            use_kitty_shm: true,
        };
        let (proto_fallback, _, _) = process_resize(req_fallback, &mut resizer);
        assert!(matches!(proto_fallback, ViewportProtocol::Standard(_)));

        // 2. Kitty picker with use_kitty_shm = true produces ViewportProtocol::KittyShm
        let mut picker_kitty = Picker::halfblocks();
        picker_kitty.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        let req_shm = ResizeRequest {
            img: src,
            original_size: (80, 80),
            scale: 1.0,
            crop: CropBox::new(0, 0, 80, 80),
            intersection: ImageIntersection::new(0, 0, 80, 80),
            target_w: 80,
            target_h: 80,
            filter_type: FilterType::Nearest,
            picker: picker_kitty,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (10, 10),
            sequence: 2,
            use_kitty_shm: true,
        };
        let (proto_shm, _, _) = process_resize(req_shm, &mut resizer);
        assert!(matches!(proto_shm, ViewportProtocol::KittyShm(_)));

        cleanup_all_kitty_shm_segments();
    }

    #[test]
    fn test_process_resize_canvas_centering_and_margins() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(40, 40));
        let picker = Picker::halfblocks();

        // Image is 40x40, target viewport is 100x100.
        // Crop box extends beyond the image bounds (margins).
        let req = ResizeRequest {
            img: src,
            original_size: (40, 40),
            scale: 1.0,
            crop: CropBox::new(-30, -30, 70, 70),
            intersection: ImageIntersection::new(0, 0, 40, 40),
            target_w: 100,
            target_h: 100,
            filter_type: FilterType::Nearest,
            picker: picker.clone(),
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (50, 50),
            sequence: 1,
            use_kitty_shm: false,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 100);
        assert_eq!(canvas.height(), 100);

        let rgba = canvas.to_rgba8();
        // Margins outside the centered 40x40 area (which is pasted at x=30..70, y=30..70)
        // must remain completely transparent
        assert_eq!(*rgba.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(99, 99), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(29, 29), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(70, 70), Rgba([0, 0, 0, 0]));

        // Pixels inside the pasted area must be opaque (alpha == 255)
        assert_eq!(rgba.get_pixel(30, 30)[3], 255);
        assert_eq!(rgba.get_pixel(50, 50)[3], 255);
        assert_eq!(rgba.get_pixel(69, 69)[3], 255);

        let (_proto, _proc_dur, _proto_dur) = process_resize(req, &mut resizer);
    }

    #[test]
    fn test_process_resize_pan_offset_boundary() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(80, 80));
        let picker = Picker::halfblocks();

        // Panned offset resulting in negative crop coordinates and partial intersection
        let req = ResizeRequest {
            img: src,
            original_size: (80, 80),
            scale: 1.0,
            crop: CropBox::new(-20, -20, 60, 60),
            intersection: ImageIntersection::new(0, 0, 60, 60),
            target_w: 80,
            target_h: 80,
            filter_type: FilterType::Linear,
            picker: picker.clone(),
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (40, 40),
            sequence: 1,
            use_kitty_shm: false,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 80);
        assert_eq!(canvas.height(), 80);

        let rgba = canvas.to_rgba8();
        // Unpasted region at (0, 0) must be transparent
        assert_eq!(*rgba.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(19, 19), Rgba([0, 0, 0, 0]));

        // Panned region starting at (20, 20) must be opaque
        assert_eq!(rgba.get_pixel(20, 20)[3], 255);
        assert_eq!(rgba.get_pixel(25, 25)[3], 255);
        assert_eq!(rgba.get_pixel(79, 79)[3], 255);

        let (_proto, _proc_dur, _proto_dur) = process_resize(req, &mut resizer);
    }

    #[test]
    fn test_process_resize_brightness_and_contrast() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_gray_test_image(60, 60, 100));
        let picker = Picker::halfblocks();

        // Test positive brightness and contrast adjustments
        let req_pos = ResizeRequest {
            img: Arc::clone(&src),
            original_size: (60, 60),
            scale: 1.0,
            crop: CropBox::new(0, 0, 60, 60),
            intersection: ImageIntersection::new(0, 0, 60, 60),
            target_w: 60,
            target_h: 60,
            filter_type: FilterType::Nearest,
            picker: picker.clone(),
            brightness: Brightness::new(30),
            contrast: Contrast::new(20.0),
            rendered_size_cells: (30, 30),
            sequence: 1,
            use_kitty_shm: false,
        };
        let canvas_pos = render_resize_canvas(&req_pos, &mut resizer);
        assert_eq!(canvas_pos.width(), 60);
        assert_eq!(canvas_pos.height(), 60);
        let rgba_pos = canvas_pos.to_rgba8();
        let px_pos = rgba_pos.get_pixel(30, 30);
        assert!(
            px_pos[0] > 100 && px_pos[1] > 100 && px_pos[2] > 100,
            "Expected RGB channels > 100 with positive brightness and contrast, got {:?}",
            px_pos
        );
        assert!(px_pos[3] > 0);
        let (_proto, _proc_dur, _proto_dur) = process_resize(req_pos, &mut resizer);

        // Test negative brightness and contrast adjustments
        let req_neg = ResizeRequest {
            img: src,
            original_size: (60, 60),
            scale: 1.0,
            crop: CropBox::new(0, 0, 60, 60),
            intersection: ImageIntersection::new(0, 0, 60, 60),
            target_w: 60,
            target_h: 60,
            filter_type: FilterType::Nearest,
            picker: picker.clone(),
            brightness: Brightness::new(-30),
            contrast: Contrast::new(-20.0),
            rendered_size_cells: (30, 30),
            sequence: 2,
            use_kitty_shm: false,
        };
        let canvas_neg = render_resize_canvas(&req_neg, &mut resizer);
        assert_eq!(canvas_neg.width(), 60);
        assert_eq!(canvas_neg.height(), 60);
        let rgba_neg = canvas_neg.to_rgba8();
        let px_neg = rgba_neg.get_pixel(30, 30);
        assert!(
            px_neg[0] < 100 && px_neg[1] < 100 && px_neg[2] < 100,
            "Expected RGB channels < 100 with negative brightness and contrast, got {:?}",
            px_neg
        );
        assert!(px_neg[3] > 0);
        let (_proto, _proc_dur, _proto_dur) = process_resize(req_neg, &mut resizer);
    }

    #[test]
    fn test_brightness_simd_and_contrast_lut_preserve_alpha() {
        let mut pixels = vec![
            100, 100, 100, 0, // fully transparent
            100, 100, 100, 128, // semi-transparent
            100, 100, 100, 255, // opaque
        ];

        // 1. Positive and negative brightness
        apply_brightness_simd(&mut pixels, 50);
        assert_eq!(pixels[0..3], [150, 150, 150]);
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[4..7], [150, 150, 150]);
        assert_eq!(pixels[7], 128);
        assert_eq!(pixels[8..11], [150, 150, 150]);
        assert_eq!(pixels[11], 255);

        apply_brightness_simd(&mut pixels, -100);
        assert_eq!(pixels[0..3], [50, 50, 50]);
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[4..7], [50, 50, 50]);
        assert_eq!(pixels[7], 128);
        assert_eq!(pixels[8..11], [50, 50, 50]);
        assert_eq!(pixels[11], 255);

        // 2. Test buffer sizes of 1..=8 pixels to cover tail-only (1..=3), exact 16-byte SIMD
        // chunks (4, 8), and combined 16-byte chunk + tail remainders (5..=7).
        for num_pixels in 1..=8 {
            // Populate `num_pixels` RGBA pixels with distinct alpha values per pixel index `i`.
            let mut buf = Vec::with_capacity(num_pixels * 4);
            for i in 0..num_pixels {
                let alpha = ((i * 37) % 256) as u8;
                buf.extend_from_slice(&[200, 100, 50, alpha]);
            }

            // Test positive delta with saturation
            let mut test_buf = buf.clone();
            apply_brightness_simd(&mut test_buf, 70);
            for i in 0..num_pixels {
                let px = &test_buf[i * 4..(i + 1) * 4];
                assert_eq!(px[0], 255, "R must saturate to 255 at pixel {}", i);
                assert_eq!(px[1], 170, "G must be 170 at pixel {}", i);
                assert_eq!(px[2], 120, "B must be 120 at pixel {}", i);
                assert_eq!(
                    px[3],
                    ((i * 37) % 256) as u8,
                    "Alpha must be preserved at pixel {}",
                    i
                );
            }

            // Test negative delta with underflow saturation
            let mut test_buf_neg = buf.clone();
            apply_brightness_simd(&mut test_buf_neg, -150);
            for i in 0..num_pixels {
                let px = &test_buf_neg[i * 4..(i + 1) * 4];
                assert_eq!(px[0], 50, "R must be 50 at pixel {}", i);
                assert_eq!(px[1], 0, "G must saturate to 0 at pixel {}", i);
                assert_eq!(px[2], 0, "B must saturate to 0 at pixel {}", i);
                assert_eq!(
                    px[3],
                    ((i * 37) % 256) as u8,
                    "Alpha must be preserved at pixel {}",
                    i
                );
            }
        }

        // 3. Contrast LUT
        apply_contrast_lut(&mut pixels, 25.0);
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[7], 128);
        assert_eq!(pixels[11], 255);
    }

    #[test]
    fn test_dynamic_knee_and_fused_adjustments_lut() {
        // 1. Verify dynamic knee calculation
        assert_eq!(compute_dynamic_knee(1.0), 1.0);
        assert_eq!(compute_dynamic_knee(3.0), 0.75);

        // 2. Small contrast nudge keeps white at 255
        let mut white_pixel = vec![255, 255, 255, 255];
        apply_adjustments_lut(&mut white_pixel, 0, 1.0);
        assert_eq!(white_pixel[0], 255);
        assert_eq!(white_pixel[1], 255);
        assert_eq!(white_pixel[2], 255);
        assert_eq!(white_pixel[3], 255);

        // 3. Fused brightness (+40) and contrast (+30.0) maintains highlight differentiation
        // Under separate passes, 220 and 250 would both clip to 255 at the brightness stage.
        let mut pixels = vec![
            220, 220, 220, 100, // pixel 1
            250, 250, 250, 200, // pixel 2
        ];
        apply_adjustments_lut(&mut pixels, 40, 30.0);

        let out_220 = pixels[0];
        let out_250 = pixels[4];
        assert!(
            out_250 > out_220,
            "Expected smooth differentiation between bright highlights: out_250 = {}, out_220 = {}",
            out_250,
            out_220
        );
        // Alpha channels must be strictly preserved
        assert_eq!(pixels[3], 100);
        assert_eq!(pixels[7], 200);

        // 4. Verify render_resize_canvas skips offscreen transparent margins during LUT adjustments
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_gray_test_image(40, 40, 120));
        let req = ResizeRequest {
            img: src,
            original_size: (40, 40),
            scale: 1.0,
            crop: CropBox::new(-20, -20, 60, 60),
            intersection: ImageIntersection::new(0, 0, 40, 40),
            target_w: 80,
            target_h: 80,
            filter_type: FilterType::Nearest,
            picker: Picker::halfblocks(),
            brightness: Brightness::new(50),
            contrast: Contrast::new(25.0),
            rendered_size_cells: (40, 40),
            sequence: 1,
            use_kitty_shm: false,
        };
        let canvas = render_resize_canvas(&req, &mut resizer).to_rgba8();
        // Margin pixel at (0, 0) was never touched by apply_adjustments_lut
        assert_eq!(*canvas.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
        // Active image pixel at (30, 30) was brightened and contrasted
        assert!(canvas.get_pixel(30, 30)[0] > 120);
        assert_eq!(canvas.get_pixel(30, 30)[3], 255);
    }

    #[test]
    fn test_render_resize_canvas_clipped_copy_and_adjustments() {
        let mut resizer = fir::Resizer::new();
        // 50x50 gray image with pixel value 100
        let src = Arc::new(make_gray_test_image(50, 50, 100));

        // 1. Offscreen panning where intersection is 40x40, but target viewport is 25x20.
        // paste_x will be 0, paste_y will be 0, so copy_w = 25 < target_inter_w (40)
        // and copy_h = 20 < target_inter_h (40).
        let req = ResizeRequest {
            img: src.clone(),
            original_size: (50, 50),
            scale: 1.0,
            crop: CropBox::new(0, 0, 25, 20),
            intersection: ImageIntersection::new(0, 0, 40, 40),
            target_w: 25,
            target_h: 20,
            filter_type: FilterType::Nearest,
            picker: Picker::halfblocks(),
            brightness: Brightness::new(30),
            contrast: Contrast::new(10.0),
            rendered_size_cells: (12, 10),
            sequence: 1,
            use_kitty_shm: false,
        };

        let canvas = render_resize_canvas(&req, &mut resizer).to_rgba8();
        assert_eq!(canvas.width(), 25);
        assert_eq!(canvas.height(), 20);

        // Copied pixels must have brightness + contrast applied
        let px = canvas.get_pixel(10, 10);
        assert!(px[0] > 100, "Pixel must be brightened: got {}", px[0]);
        assert_eq!(px[3], 255, "Alpha must be preserved as 255");

        // 2. Asymmetric clipping with margins: target_w = 40, target_h = 40,
        // intersection at (0, 0..30, 30), scale 1.0, crop from -5, -5.
        // Margin at (0, 0) should remain transparent black (0, 0, 0, 0).
        let req_margin = ResizeRequest {
            img: src,
            original_size: (50, 50),
            scale: 1.0,
            crop: CropBox::new(-5, -5, 35, 35),
            intersection: ImageIntersection::new(0, 0, 30, 30),
            target_w: 40,
            target_h: 40,
            filter_type: FilterType::Nearest,
            picker: Picker::halfblocks(),
            brightness: Brightness::new(40),
            contrast: Contrast::new(15.0),
            rendered_size_cells: (20, 20),
            sequence: 2,
            use_kitty_shm: false,
        };

        let canvas_margin = render_resize_canvas(&req_margin, &mut resizer).to_rgba8();
        assert_eq!(*canvas_margin.get_pixel(0, 0), image::Rgba([0, 0, 0, 0]));
        // Active image pixel at (10, 10) should have adjustments applied
        let active_px = canvas_margin.get_pixel(10, 10);
        assert!(
            active_px[0] > 100,
            "Active pixel must have adjustments applied: got {}",
            active_px[0]
        );
        assert_eq!(active_px[3], 255, "Alpha must be 255");
    }

    #[test]
    fn test_soft_knee_highlight_continuity_and_stability() {
        assert_eq!(soft_knee_highlight(0.75), 0.75);

        let eps = 1e-4;
        let slope_left = (soft_knee_highlight(0.75) - soft_knee_highlight(0.75 - eps)) / eps;
        let slope_right = (soft_knee_highlight(0.75 + eps) - soft_knee_highlight(0.75)) / eps;
        assert!((slope_left - 1.0).abs() < 1e-3);
        assert!((slope_right - 1.0).abs() < 1e-3);

        let mut prev = soft_knee_highlight(0.75);
        for i in 1..=50 {
            let x = 0.75 + i as f32 * 0.1;
            let y = soft_knee_highlight(x);
            assert!(y > prev, "Monotonicity violated at x = {}", x);
            assert!(y < 1.0, "Should approach 1.0 asymptotically, got {}", y);
            prev = y;
        }

        let extreme = soft_knee_highlight(1e20);
        assert!(!extreme.is_nan());
        assert!(!extreme.is_infinite());
        assert!((extreme - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_anti_ringing_clamp_eliminates_overshoot() {
        // 4x2 source: columns 0 and 1 black, columns 2 and 3 white
        let mut src = image::RgbaImage::new(4, 2);
        for y in 0..2 {
            src.put_pixel(0, y, image::Rgba([0, 0, 0, 255]));
            src.put_pixel(1, y, image::Rgba([0, 0, 0, 255]));
            src.put_pixel(2, y, image::Rgba([255, 255, 255, 255]));
            src.put_pixel(3, y, image::Rgba([255, 255, 255, 255]));
        }

        // 1. Upscaled target (8x4 >= 4x2) with ringing overshoot at (0, 0)
        let mut dst_up = image::RgbaImage::new(8, 4);
        dst_up.put_pixel(0, 0, image::Rgba([75, 75, 75, 255]));

        clamp_anti_ringing_rgba(&mut dst_up, &src, None);

        // Pixel (0, 0) maps to local texels which are all 0, so overshoot clamped to 0
        assert_eq!(dst_up.get_pixel(0, 0), &image::Rgba([0, 0, 0, 255]));

        // 2. Downscaled target (2x1 < 4x2): should be a no-op to preserve anti-aliased averages
        let mut dst_down = image::RgbaImage::new(2, 1);
        dst_down.put_pixel(0, 0, image::Rgba([75, 75, 75, 255]));

        clamp_anti_ringing_rgba(&mut dst_down, &src, None);

        // Clamping must not modify downscaled images
        assert_eq!(dst_down.get_pixel(0, 0), &image::Rgba([75, 75, 75, 255]));

        // 3. Test clamp_anti_ringing_rgb overshoot elimination
        let mut rgb_overshoot_src = image::RgbImage::new(4, 2);
        for y in 0..2 {
            rgb_overshoot_src.put_pixel(0, y, image::Rgb([0, 0, 0]));
            rgb_overshoot_src.put_pixel(1, y, image::Rgb([0, 0, 0]));
            rgb_overshoot_src.put_pixel(2, y, image::Rgb([255, 255, 255]));
            rgb_overshoot_src.put_pixel(3, y, image::Rgb([255, 255, 255]));
        }
        let mut dst_rgb_clamped = image::RgbaImage::new(8, 4);
        dst_rgb_clamped.put_pixel(0, 0, image::Rgba([75, 75, 75, 255]));
        clamp_anti_ringing_rgb(&mut dst_rgb_clamped, &rgb_overshoot_src, None);
        assert_eq!(
            dst_rgb_clamped.get_pixel(0, 0),
            &image::Rgba([0, 0, 0, 255])
        );

        // 4. Test equivalence between clamp_anti_ringing_rgba and clamp_anti_ringing_rgb
        // across multi-pixel magnified spans and cropped sub-regions.
        let mut src_rgba_span = image::RgbaImage::new(10, 10);
        let mut src_rgb_span = image::RgbImage::new(10, 10);
        for y in 0..10 {
            for x in 0..10 {
                let r = ((x * 25) % 256) as u8;
                let g = ((y * 25) % 256) as u8;
                let b = (((x + y) * 20) % 256) as u8;
                src_rgba_span.put_pixel(x, y, image::Rgba([r, g, b, 255]));
                src_rgb_span.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }

        // 4a. Full image 5x magnification (50x50 target) with synthetic ringing
        let mut dst_rgba_span = image::RgbaImage::new(50, 50);
        let mut dst_rgb_span = image::RgbaImage::new(50, 50);
        for y in 0..50 {
            for x in 0..50 {
                let val = ((x + y * 2) % 256) as u8;
                dst_rgba_span.put_pixel(x, y, image::Rgba([val, val, val, 255]));
                dst_rgb_span.put_pixel(x, y, image::Rgba([val, val, val, 255]));
            }
        }

        clamp_anti_ringing_rgba(&mut dst_rgba_span, &src_rgba_span, None);
        clamp_anti_ringing_rgb(&mut dst_rgb_span, &src_rgb_span, None);
        assert_eq!(dst_rgba_span, dst_rgb_span);

        // 4b. Cropped sub-region (2.0, 2.0, 4.0, 4.0)
        let crop = Some((2.0, 2.0, 4.0, 4.0));
        let mut dst_rgba_crop = image::RgbaImage::new(40, 40);
        let mut dst_rgb_crop = image::RgbaImage::new(40, 40);
        for y in 0..40 {
            for x in 0..40 {
                let val = ((x * 3 + y) % 256) as u8;
                dst_rgba_crop.put_pixel(x, y, image::Rgba([val, val, val, 255]));
                dst_rgb_crop.put_pixel(x, y, image::Rgba([val, val, val, 255]));
            }
        }

        clamp_anti_ringing_rgba(&mut dst_rgba_crop, &src_rgba_span, crop);
        clamp_anti_ringing_rgb(&mut dst_rgb_crop, &src_rgb_span, crop);
        assert_eq!(dst_rgba_crop, dst_rgb_crop);
    }

    #[test]
    fn test_custom_filter_kernels_mathematical_fidelity() {
        // 1. Lanczos3 reference sinc(x) * sinc(x / 3.0)
        fn sinc(x: f64) -> f64 {
            if x == 0.0 {
                1.0
            } else {
                let px = std::f64::consts::PI * x;
                px.sin() / px
            }
        }
        fn ref_lanczos3(x: f64) -> f64 {
            if (-3.0..3.0).contains(&x) {
                sinc(x) * sinc(x / 3.0)
            } else {
                0.0
            }
        }

        assert_eq!(fast_lanczos3_kernel(0.0), 1.0);
        assert_eq!(fast_lanczos3_kernel(3.0), 0.0);
        assert_eq!(fast_lanczos3_kernel(3.5), 0.0);
        assert_eq!(fast_lanczos3_kernel(-4.2), 0.0);

        // Sample points in (-3.0, 3.0)
        for i in -2999..=2999 {
            let x = i as f64 / 1000.0;
            let expected = ref_lanczos3(x);
            let actual = fast_lanczos3_kernel(x);
            assert!(
                (actual - expected).abs() < 1e-14,
                "Lanczos3 mismatch at x = {}: actual = {}, expected = {}",
                x,
                actual,
                expected
            );
        }

        // Sample points outside [-3.0, 3.0]
        for i in [31, 35, 40, 50, -31, -35, -40, -50] {
            let x = i as f64 / 10.0;
            assert_eq!(fast_lanczos3_kernel(x), 0.0);
            assert_eq!(ref_lanczos3(x), 0.0);
        }

        // 2. Mitchell-Netravali reference formula (B = 1/3, C = 1/3)
        fn ref_mitchell(x: f64) -> f64 {
            let x = x.abs();
            let b = 1.0 / 3.0;
            let c = 1.0 / 3.0;
            if x < 1.0 {
                (1.0 / 6.0)
                    * ((12.0 - 9.0 * b - 6.0 * c) * x.powi(3)
                        + (-18.0 + 12.0 * b + 6.0 * c) * x.powi(2)
                        + (6.0 - 2.0 * b))
            } else if x < 2.0 {
                (1.0 / 6.0)
                    * ((-b - 6.0 * c) * x.powi(3)
                        + (6.0 * b + 30.0 * c) * x.powi(2)
                        + (-12.0 * b - 48.0 * c) * x
                        + (8.0 * b + 24.0 * c))
            } else {
                0.0
            }
        }

        for i in 0..=2500 {
            let x = i as f64 / 1000.0;
            let expected = ref_mitchell(x);
            let actual = fast_mitchell_kernel(x);
            assert!(
                (actual - expected).abs() < 1e-14,
                "Mitchell mismatch at x = {}: actual = {}, expected = {}",
                x,
                actual,
                expected
            );
        }
    }

    #[test]
    fn test_fast_resize_rgb8_direct_and_no_alpha_buffer() {
        let mut resizer = fir::Resizer::new();

        // 1. Create a 100x100 ImageRgb8
        let mut rgb_raw = image::RgbImage::new(100, 100);
        for (x, y, pixel) in rgb_raw.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        }
        let src_rgb = DynamicImage::ImageRgb8(rgb_raw);

        let filters = [
            FilterType::Nearest,
            FilterType::Box,
            FilterType::Linear,
            FilterType::Mitchell,
            FilterType::Lanczos,
        ];

        for filter in filters {
            // Downscale
            let downscaled = fast_resize(&mut resizer, &src_rgb, 40, 30, filter, None)
                .expect("Downscale failed");
            assert_eq!(downscaled.width(), 40);
            assert_eq!(downscaled.height(), 30);
            let down_rgba = downscaled.to_rgba8();
            assert_eq!(down_rgba.get_pixel(0, 0)[3], 255);
            assert_eq!(down_rgba.get_pixel(39, 29)[3], 255);

            // Upscale (triggers clamp_anti_ringing_rgb on lobed filters)
            let upscaled = fast_resize(&mut resizer, &src_rgb, 200, 150, filter, None)
                .expect("Upscale failed");
            assert_eq!(upscaled.width(), 200);
            assert_eq!(upscaled.height(), 150);
            let up_rgba = upscaled.to_rgba8();
            assert_eq!(up_rgba.get_pixel(0, 0)[3], 255);
            assert_eq!(up_rgba.get_pixel(199, 149)[3], 255);

            // Cropped resize
            let crop = Some((10.0, 10.0, 50.0, 50.0));
            let cropped = fast_resize(&mut resizer, &src_rgb, 80, 80, filter, crop)
                .expect("Cropped resize failed");
            assert_eq!(cropped.width(), 80);
            assert_eq!(cropped.height(), 80);
            let crop_rgba = cropped.to_rgba8();
            assert_eq!(crop_rgba.get_pixel(0, 0)[3], 255);
            assert_eq!(crop_rgba.get_pixel(79, 79)[3], 255);
        }

        // 2. Verify that resizing ImageRgb8 does NOT allocate an alpha_buffer in resizer
        // For a 500x500 image, an alpha_buffer would allocate at least 500 * 500 * 4 = 1,000,000 bytes.
        let large_rgb = DynamicImage::ImageRgb8(image::RgbImage::new(500, 500));
        let large_rgba = DynamicImage::ImageRgba8(image::RgbaImage::new(500, 500));

        resizer.reset_internal_buffers();
        assert_eq!(resizer.size_of_internal_buffers(), 0);

        fast_resize(&mut resizer, &large_rgb, 50, 50, FilterType::Box, None).unwrap();
        let rgb_internal_buf_size = resizer.size_of_internal_buffers();

        resizer.reset_internal_buffers();
        fast_resize(&mut resizer, &large_rgba, 50, 50, FilterType::Box, None).unwrap();
        let rgba_internal_buf_size = resizer.size_of_internal_buffers();

        // RGBA allocates 1 MB alpha_buffer, whereas RGB8 avoids alpha_buffer entirely
        assert!(rgba_internal_buf_size >= 1_000_000);
        assert!(rgb_internal_buf_size < 150_000);
        assert!(rgba_internal_buf_size > rgb_internal_buf_size + 800_000);

        // For Nearest, RGB8 requires 0 bytes of internal buffers
        resizer.reset_internal_buffers();
        fast_resize(&mut resizer, &large_rgb, 50, 50, FilterType::Nearest, None).unwrap();
        assert_eq!(resizer.size_of_internal_buffers(), 0);
    }

    #[test]
    fn test_maybe_reclaim_resizer_scratch() {
        let mut resizer = fir::Resizer::new();
        let test_img = DynamicImage::ImageRgb8(image::RgbImage::new(400, 400));

        // Populate internal convolution buffers
        fast_resize(&mut resizer, &test_img, 50, 50, FilterType::Mitchell, None).unwrap();
        assert!(resizer.size_of_internal_buffers() > 0);

        // Dimensions exactly at or below 16 MP threshold (16,000,000 pixels) do NOT reset
        maybe_reclaim_resizer_scratch(&mut resizer, 4000, 4000, 100, 100);
        assert!(resizer.size_of_internal_buffers() > 0);

        // Source dimensions exceeding 16 MP reset the resizer scratch buffers
        maybe_reclaim_resizer_scratch(&mut resizer, 4001, 4000, 100, 100);
        assert_eq!(resizer.size_of_internal_buffers(), 0);

        // Re-populate and test destination dimensions exceeding 16 MP
        fast_resize(&mut resizer, &test_img, 50, 50, FilterType::Mitchell, None).unwrap();
        assert!(resizer.size_of_internal_buffers() > 0);

        maybe_reclaim_resizer_scratch(&mut resizer, 100, 100, 4000, 4001);
        assert_eq!(resizer.size_of_internal_buffers(), 0);
    }

    #[test]
    fn test_spawn_loader_pool_parallel_prefetch_and_foreground_promotion() {
        let test_dir = std::path::PathBuf::from("target/tmp/loader_pool_test");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();

        // Create 16 small test images on disk
        let mut paths = Vec::new();
        for i in 0..16 {
            let p = test_dir.join(format!("test_img_{}.png", i));
            let img = image::RgbImage::new(40, 40);
            img.save(&p).unwrap();
            paths.push(p);
        }

        let (loader_tx, loader_rx) = std::sync::mpsc::channel();
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let active_seq = Arc::new(AtomicUsize::new(1));

        spawn_loader_pool(
            loader_rx,
            response_tx,
            Arc::clone(&active_seq),
            false,
            Some(test_dir.join("thumbnails")),
        );

        // 1. Dispatch foreground load (idx 0) and prefetch loads (idx 1, 2) with sequence 1
        loader_tx
            .send(LoaderRequest {
                idx: 0,
                source: ImageSource::Local(paths[0].clone()),
                mode: LoaderMode::ThumbnailAndFull,
                sequence: 1,
            })
            .unwrap();
        loader_tx
            .send(LoaderRequest {
                idx: 1,
                source: ImageSource::Local(paths[1].clone()),
                mode: LoaderMode::Prefetch,
                sequence: 1,
            })
            .unwrap();
        loader_tx
            .send(LoaderRequest {
                idx: 2,
                source: ImageSource::Local(paths[2].clone()),
                mode: LoaderMode::Prefetch,
                sequence: 1,
            })
            .unwrap();

        // Collect full decoded responses for sequence 1
        let mut seq1_responses = Vec::new();
        let timeout = std::time::Duration::from_secs(3);
        let start = std::time::Instant::now();
        while seq1_responses.len() < 3 && start.elapsed() < timeout {
            if let Ok(resp) = response_rx.recv_timeout(std::time::Duration::from_millis(500))
                && resp.thumbnail_source.is_none()
            {
                seq1_responses.push(resp);
            }
        }
        assert_eq!(seq1_responses.len(), 3);
        for resp in &seq1_responses {
            assert!(resp.result.is_ok());
            assert_eq!(resp.sequence, 1);
        }

        // 2. Test promotion and deduplication:
        // Set active sequence to 2
        active_seq.store(2, Ordering::Release);

        // Queue prefetch for multiple images so the last one is guaranteed to be pending,
        // then dispatch foreground promotion for image 15
        for (i, p) in paths.iter().enumerate().take(16).skip(3) {
            loader_tx
                .send(LoaderRequest {
                    idx: i,
                    source: ImageSource::Local(p.clone()),
                    mode: LoaderMode::Prefetch,
                    sequence: 2,
                })
                .unwrap();
        }
        loader_tx
            .send(LoaderRequest {
                idx: 15,
                source: ImageSource::Local(paths[15].clone()),
                mode: LoaderMode::ThumbnailAndFull,
                sequence: 2,
            })
            .unwrap();

        let mut got_promoted = false;
        let mut img15_full_count = 0;
        let start2 = std::time::Instant::now();
        while start2.elapsed() < timeout {
            if let Ok(resp) = response_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                if resp.idx == 15 && resp.sequence == 2 && resp.thumbnail_source.is_none() {
                    img15_full_count += 1;
                    if !resp.is_prefetch {
                        assert!(resp.result.is_ok());
                        got_promoted = true;
                    }
                }
            } else if got_promoted {
                break;
            }
        }
        assert!(got_promoted, "Promoted job should complete as foreground");
        assert_eq!(
            img15_full_count, 1,
            "Deduplication must prevent duplicate full-resolution decodes"
        );

        // 3. Test cancellation on sequence bump:
        active_seq.store(3, Ordering::Release);
        loader_tx
            .send(LoaderRequest {
                idx: 1,
                source: ImageSource::Local(paths[1].clone()),
                mode: LoaderMode::Prefetch,
                sequence: 2, // Stale sequence
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        while let Ok(resp) = response_rx.try_recv() {
            assert!(
                resp.sequence >= 3 || resp.idx != 1 || resp.thumbnail_source.is_some(),
                "Stale decode for sequence 2 should be cancelled"
            );
        }

        // 4. Verify clean shutdown when loader_tx is dropped
        drop(loader_tx);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_spawn_loader_pool_promoted_prefetch_thumbnail_and_fast_scroll_cancellation() {
        let test_dir = std::path::PathBuf::from("target/tmp/loader_pool_thumb_test");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();

        // 1. Create source image and pre-generate XDG thumbnail (> 256 px)
        let src_path = test_dir.join("thumb_test.png");
        let src_img = DynamicImage::ImageRgb8(image::RgbImage::new(400, 400));
        src_img.save(&src_path).unwrap();

        let cache_root = test_dir.join("thumbnails");
        let mut resizer = fir::Resizer::new();
        let saved_thumb = save_cached_thumbnail_with_dims(
            &cache_root,
            &mut resizer,
            &src_path,
            &src_img,
            (400, 400),
        );
        assert!(
            saved_thumb.is_some(),
            "Thumbnail must be saved for >256 image"
        );

        let (loader_tx, loader_rx) = std::sync::mpsc::channel();
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let active_seq = Arc::new(AtomicUsize::new(1));

        spawn_loader_pool(
            loader_rx,
            response_tx,
            Arc::clone(&active_seq),
            false,
            Some(cache_root),
        );

        // 2. Dispatch Prefetch first (which does not extract thumbnail)
        loader_tx
            .send(LoaderRequest {
                idx: 42,
                source: ImageSource::Local(src_path.clone()),
                mode: LoaderMode::Prefetch,
                sequence: 1,
            })
            .unwrap();

        // Immediately step onto the same index with ThumbnailAndFull
        loader_tx
            .send(LoaderRequest {
                idx: 42,
                source: ImageSource::Local(src_path.clone()),
                mode: LoaderMode::ThumbnailAndFull,
                sequence: 1,
            })
            .unwrap();

        // Verify that the thumbnail response is emitted even though the job was already queued as Prefetch
        let mut got_thumb = false;
        let mut got_full = false;
        let timeout = std::time::Duration::from_secs(3);
        let start = std::time::Instant::now();
        while (!got_thumb || !got_full) && start.elapsed() < timeout {
            if let Ok(resp) = response_rx.recv_timeout(std::time::Duration::from_millis(500))
                && resp.idx == 42
                && resp.sequence == 1
            {
                if resp.thumbnail_source.is_some() {
                    got_thumb = true;
                } else {
                    got_full = true;
                    assert!(
                        !resp.is_prefetch,
                        "Promoted job must complete as foreground"
                    );
                }
            }
        }
        assert!(
            got_thumb,
            "Promoted prefetch job must emit fast thumbnail response"
        );
        assert!(got_full, "Full resolution decode must complete");

        // 3. Test fast scrolling cancellation: dispatch ThumbnailOnly at sequence 2
        active_seq.store(2, Ordering::Release);
        loader_tx
            .send(LoaderRequest {
                idx: 42,
                source: ImageSource::Local(src_path),
                mode: LoaderMode::ThumbnailOnly,
                sequence: 2,
            })
            .unwrap();

        let mut got_seq2_thumb = false;
        let start2 = std::time::Instant::now();
        while start2.elapsed() < std::time::Duration::from_millis(500) {
            if let Ok(resp) = response_rx.recv_timeout(std::time::Duration::from_millis(50))
                && resp.sequence == 2
            {
                if resp.thumbnail_source.is_some() {
                    got_seq2_thumb = true;
                } else {
                    panic!(
                        "ThumbnailOnly during fast-scrolling must NOT emit full resolution decode"
                    );
                }
            }
        }
        assert!(got_seq2_thumb, "ThumbnailOnly must emit fast thumbnail");

        drop(loader_tx);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_can_pop_job_and_pending_demotion() {
        let mut state = DecodeQueueState {
            pending: VecDeque::new(),
            in_flight: Vec::new(),
            shutdown: false,
        };
        let max_workers = 5;

        let make_job = |idx: usize, sequence: u64, mode: LoaderMode| DecodeJob {
            idx,
            source: ImageSource::Local(std::path::PathBuf::from("dummy.png")),
            mode,
            sequence,
            bytes_opt: None,
            raw_header_opt: None,
            local_path: None,
            has_thumbnail: false,
        };

        let make_slot = |idx: usize, sequence: u64, mode: LoaderMode| InFlightSlot {
            idx,
            sequence: Arc::new(AtomicU64::new(sequence)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            mode,
            has_thumbnail: false,
        };

        // 1. Empty queue cannot pop
        assert!(!can_pop_job(&state, max_workers));

        // 2. Pending foreground job when no foreground in flight -> can pop
        state
            .pending
            .push_back(make_job(1, 1, LoaderMode::ThumbnailAndFull));
        assert!(can_pop_job(&state, max_workers));

        // 3. If a foreground job is in flight, cannot pop another foreground job
        state
            .in_flight
            .push(Some(make_slot(0, 1, LoaderMode::ThumbnailAndFull)));
        assert!(!can_pop_job(&state, max_workers));

        // 4. When foreground finishes, if front pending job is Prefetch and active prefetch < max_workers - 1, can pop
        state.in_flight.clear();
        state.pending.clear();
        state
            .pending
            .push_back(make_job(1, 1, LoaderMode::Prefetch));
        assert!(can_pop_job(&state, max_workers));

        // 5. If prefetch workers saturate reserved slots (4 out of 5), cannot pop more prefetch
        for i in 0..3 {
            state
                .in_flight
                .push(Some(make_slot(i, 1, LoaderMode::Prefetch)));
        }
        assert!(can_pop_job(&state, max_workers));
        state
            .in_flight
            .push(Some(make_slot(3, 1, LoaderMode::Prefetch)));
        assert!(!can_pop_job(&state, max_workers));

        // 6. Test pending demotion on sequence advancement
        state.pending.clear();
        state
            .pending
            .push_back(make_job(1, 1, LoaderMode::ThumbnailAndFull));
        let highest_seq = 2;
        let current_requests = [
            LoaderRequest {
                idx: 2,
                source: ImageSource::Local(std::path::PathBuf::from("dummy2.png")),
                mode: LoaderMode::ThumbnailAndFull,
                sequence: 2,
            },
            LoaderRequest {
                idx: 1,
                source: ImageSource::Local(std::path::PathBuf::from("dummy1.png")),
                mode: LoaderMode::Prefetch,
                sequence: 2,
            },
        ];

        for job in &mut state.pending {
            if let Some(matching_req) = current_requests.iter().find(|r| r.idx == job.idx) {
                if job.sequence < highest_seq || !matching_req.mode.is_prefetch() {
                    job.mode = matching_req.mode;
                }
                job.sequence = highest_seq;
            }
        }

        assert_eq!(state.pending[0].mode, LoaderMode::Prefetch);
        assert_eq!(state.pending[0].sequence, 2);
    }

    #[test]
    fn test_in_flight_foreground_demotion_to_prefetch_unblocks_new_foreground() {
        let max_workers = 5;
        let mut state = DecodeQueueState {
            pending: VecDeque::new(),
            in_flight: Vec::new(),
            shutdown: false,
        };

        let make_job = |idx: usize, sequence: u64, mode: LoaderMode| DecodeJob {
            idx,
            source: ImageSource::Local(std::path::PathBuf::from("dummy.png")),
            mode,
            sequence,
            bytes_opt: None,
            raw_header_opt: None,
            local_path: None,
            has_thumbnail: false,
        };

        let make_slot = |idx: usize, sequence: u64, mode: LoaderMode| InFlightSlot {
            idx,
            sequence: Arc::new(AtomicU64::new(sequence)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            mode,
            has_thumbnail: false,
        };

        // 1. Foreground job (idx 0) is in flight at sequence 1
        state
            .in_flight
            .push(Some(make_slot(0, 1, LoaderMode::ThumbnailAndFull)));

        // Pending foreground job (idx 1) for sequence 2
        state
            .pending
            .push_back(make_job(1, 2, LoaderMode::ThumbnailAndFull));

        // While foreground job (idx 0) is in flight, can_pop_job cannot pop idx 1
        assert!(!can_pop_job(&state, max_workers));

        // When the in-flight foreground slot is cancelled, can_pop_job can immediately pop idx 1
        state.in_flight[0]
            .as_ref()
            .unwrap()
            .cancel_flag
            .store(true, Ordering::Release);
        assert!(can_pop_job(&state, max_workers));
        state.in_flight[0]
            .as_ref()
            .unwrap()
            .cancel_flag
            .store(false, Ordering::Release);
        assert!(!can_pop_job(&state, max_workers));

        // 2. Next sequence 2 requests arrive: idx 1 is foreground, idx 0 is demoted to prefetch
        let highest_seq = 2;
        let current_requests = [
            LoaderRequest {
                idx: 1,
                source: ImageSource::Local(std::path::PathBuf::from("dummy1.png")),
                mode: LoaderMode::ThumbnailAndFull,
                sequence: 2,
            },
            LoaderRequest {
                idx: 0,
                source: ImageSource::Local(std::path::PathBuf::from("dummy0.png")),
                mode: LoaderMode::Prefetch,
                sequence: 2,
            },
        ];

        // Reconcile in_flight slots with current_requests
        for slot in state.in_flight.iter_mut().flatten() {
            if !slot.cancel_flag.load(Ordering::Acquire)
                && let Some(matching_req) = current_requests.iter().find(|r| r.idx == slot.idx)
            {
                let prev_seq = slot.sequence.swap(highest_seq, Ordering::AcqRel);
                if prev_seq < highest_seq || !matching_req.mode.is_prefetch() {
                    slot.mode = matching_req.mode;
                }
            } else if slot.sequence.load(Ordering::Acquire) < highest_seq {
                slot.cancel_flag.store(true, Ordering::Release);
            }
        }

        // Verify slot 0 is demoted to Prefetch
        let slot0 = state.in_flight[0].as_ref().unwrap();
        assert!(slot0.mode.is_prefetch());
        assert_eq!(slot0.sequence.load(Ordering::Acquire), 2);

        // Verify can_pop_job now allows popping the new foreground job (idx 1) without waiting for idx 0
        assert!(can_pop_job(&state, max_workers));

        // 3. Verify worker reservation when all workers are saturated with prefetches
        // Add 4 more prefetch slots so all 5 workers are busy with prefetches
        for i in 2..6 {
            state
                .in_flight
                .push(Some(make_slot(i, 2, LoaderMode::Prefetch)));
        }
        assert_eq!(state.in_flight.len(), 5);

        // Total active prefetches = 5, but max_prefetch = max_workers - 1 = 4
        let max_prefetch = max_workers.saturating_sub(1).max(1);
        let mut active_prefetches = state
            .in_flight
            .iter()
            .flatten()
            .filter(|slot| slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire))
            .count();
        assert_eq!(active_prefetches, 5);

        // Run worker reservation reconciliation
        if current_requests.iter().any(|r| !r.mode.is_prefetch())
            && !state
                .in_flight
                .iter()
                .flatten()
                .any(|slot| !slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire))
            && active_prefetches > max_prefetch
        {
            for slot in state.in_flight.iter_mut().flatten() {
                if active_prefetches <= max_prefetch {
                    break;
                }
                if slot.mode.is_prefetch() && !slot.cancel_flag.load(Ordering::Acquire) {
                    slot.cancel_flag.store(true, Ordering::Release);
                    active_prefetches -= 1;
                }
            }
        }

        // Exactly 1 prefetch was cancelled so active prefetch slots <= max_prefetch (4)
        let non_cancelled = state
            .in_flight
            .iter()
            .flatten()
            .filter(|slot| !slot.cancel_flag.load(Ordering::Acquire))
            .count();
        assert_eq!(non_cancelled, 4);
    }

    #[test]
    fn test_spawn_resize_worker_coalescing_and_stale_skip() {
        let (resize_tx, resize_rx) = std::sync::mpsc::channel();
        let (protocol_tx, protocol_rx) = std::sync::mpsc::channel();
        let active_seq = Arc::new(AtomicUsize::new(1));

        spawn_resize_worker(resize_rx, protocol_tx, Arc::clone(&active_seq));

        let img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(100, 100)));
        let picker = Picker::halfblocks();

        let make_req = |seq: u64| ResizeRequest {
            img: Arc::clone(&img),
            original_size: (100, 100),
            scale: 1.0,
            crop: CropBox {
                x1: 0,
                y1: 0,
                x2: 100,
                y2: 100,
            },
            intersection: ImageIntersection {
                x1: 0,
                y1: 0,
                x2: 100,
                y2: 100,
            },
            target_w: 40,
            target_h: 20,
            filter_type: FilterType::Nearest,
            picker: picker.clone(),
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (40, 20),
            sequence: seq,
            use_kitty_shm: false,
        };

        // 1. Send stale request (seq 0 < active_seq 1) -> should be skipped
        resize_tx.send(make_req(0)).unwrap();

        // 2. Send two requests rapidly (seq 1 and seq 2) -> seq 1 should coalesce into seq 2
        active_seq.store(2, Ordering::Release);
        resize_tx.send(make_req(1)).unwrap();
        resize_tx.send(make_req(2)).unwrap();

        let resp = protocol_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("Expected a coalesced resize response");
        assert_eq!(resp.sequence, 2);
        assert_eq!(resp.rendered_cells, (40, 20));

        // Drop tx to verify clean worker exit
        drop(resize_tx);
        assert!(
            protocol_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
    }

    #[test]
    fn test_thumb_writer_queue_bounded_and_async_write_no_overwrite() {
        // 1. Test enqueue_thumb_write_job bounds, deduplication, and <=256 filtering
        let mut queue = ThumbWriteQueue {
            pending: VecDeque::new(),
            shutdown: false,
        };

        // A. Filter out images <= 256x256
        let small_img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(256, 256)));
        enqueue_thumb_write_job(
            &mut queue,
            ThumbWriteJob {
                path: PathBuf::from("small.png"),
                image: small_img,
                source_dims: (256, 256),
            },
            MAX_THUMB_WRITE_QUEUE,
        );
        assert_eq!(queue.pending.len(), 0);

        // B. Enqueue image > 256
        let large_img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(400, 300)));
        enqueue_thumb_write_job(
            &mut queue,
            ThumbWriteJob {
                path: PathBuf::from("img_0.png"),
                image: Arc::clone(&large_img),
                source_dims: (400, 300),
            },
            MAX_THUMB_WRITE_QUEUE,
        );
        assert_eq!(queue.pending.len(), 1);

        // C. Deduplicate same path
        enqueue_thumb_write_job(
            &mut queue,
            ThumbWriteJob {
                path: PathBuf::from("img_0.png"),
                image: Arc::clone(&large_img),
                source_dims: (400, 300),
            },
            MAX_THUMB_WRITE_QUEUE,
        );
        assert_eq!(queue.pending.len(), 1);

        // D. Fill up to MAX_THUMB_WRITE_QUEUE (8)
        for i in 1..MAX_THUMB_WRITE_QUEUE {
            enqueue_thumb_write_job(
                &mut queue,
                ThumbWriteJob {
                    path: PathBuf::from(format!("img_{}.png", i)),
                    image: Arc::clone(&large_img),
                    source_dims: (400, 300),
                },
                MAX_THUMB_WRITE_QUEUE,
            );
        }
        assert_eq!(queue.pending.len(), MAX_THUMB_WRITE_QUEUE);
        assert_eq!(
            queue.pending.front().unwrap().path,
            PathBuf::from("img_0.png")
        );

        // E. Enqueue 9th job -> pops oldest (img_0.png)
        enqueue_thumb_write_job(
            &mut queue,
            ThumbWriteJob {
                path: PathBuf::from("img_8.png"),
                image: Arc::clone(&large_img),
                source_dims: (400, 300),
            },
            MAX_THUMB_WRITE_QUEUE,
        );
        assert_eq!(queue.pending.len(), MAX_THUMB_WRITE_QUEUE);
        assert_eq!(
            queue.pending.front().unwrap().path,
            PathBuf::from("img_1.png")
        );
        assert_eq!(
            queue.pending.back().unwrap().path,
            PathBuf::from("img_8.png")
        );

        // 2. Integration test: background thumbnail writer writes asynchronously and skips overwriting
        let test_dir = std::path::PathBuf::from("target/tmp/async_thumb_writer_test");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();

        let src_path = test_dir.join("async_test.png");
        let src_img = DynamicImage::ImageRgb8(image::RgbImage::new(400, 400));
        src_img.save(&src_path).unwrap();

        let cache_root = test_dir.join("thumbnails");
        assert!(!has_valid_cached_thumbnail(&cache_root, &src_path));

        let (loader_tx, loader_rx) = std::sync::mpsc::channel();
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let active_seq = Arc::new(AtomicUsize::new(1));

        spawn_loader_pool(
            loader_rx,
            response_tx,
            Arc::clone(&active_seq),
            false,
            Some(cache_root.clone()),
        );

        // Dispatch full decode request without pre-existing thumbnail
        loader_tx
            .send(LoaderRequest {
                idx: 0,
                source: ImageSource::Local(src_path.clone()),
                mode: LoaderMode::ThumbnailAndFull,
                sequence: 1,
            })
            .unwrap();

        // UI receives decoded response first
        let resp = response_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("Expected LoaderResponse");
        assert!(resp.result.is_ok());

        // Poll until background thumbnail writer finishes writing to cache_root
        let poll_start = std::time::Instant::now();
        let mut thumb_written = false;
        while poll_start.elapsed() < std::time::Duration::from_secs(3) {
            if has_valid_cached_thumbnail(&cache_root, &src_path) {
                thumb_written = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            thumb_written,
            "Background thumbnail writer must write thumbnail"
        );

        // Verify that existing thumbnail is not overwritten
        let (_uri, md5_name) = super::cache_key_for_path(&src_path).unwrap();
        let thumb_file = cache_root.join("large").join(&md5_name);
        assert!(thumb_file.exists());
        let mtime_first = std::fs::metadata(&thumb_file).unwrap().modified().unwrap();

        // Dispatch second request for the same image with Prefetch mode so Stage 1 skips thumbnail extraction,
        // forcing Stage 2 to enqueue a ThumbWriteJob and exercising !has_valid_cached_thumbnail on the writer thread
        active_seq.store(2, Ordering::Release);
        loader_tx
            .send(LoaderRequest {
                idx: 0,
                source: ImageSource::Local(src_path),
                mode: LoaderMode::Prefetch,
                sequence: 2,
            })
            .unwrap();

        let _ = response_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("Expected second LoaderResponse");

        std::thread::sleep(std::time::Duration::from_millis(100));
        let mtime_second = std::fs::metadata(&thumb_file).unwrap().modified().unwrap();
        assert_eq!(
            mtime_first, mtime_second,
            "Background thumbnail writer must skip overwriting already-valid thumbnail"
        );

        drop(loader_tx);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&test_dir);
    }
}
