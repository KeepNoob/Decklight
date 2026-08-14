use crate::config::{NotesLayout, NotesMode};
use crate::document::{PageBitmap, PageView, render_page_bitmap_from_path};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};

const HIGH_PRIORITY_RENDER_QUEUE_CAPACITY: usize = 4;
const NORMAL_PRIORITY_RENDER_QUEUE_CAPACITY: usize = 4;
const LOW_PRIORITY_RENDER_QUEUE_CAPACITY: usize = 4;
const RENDER_RESULT_QUEUE_CAPACITY: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderPriority {
    High,
    Normal,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RenderJobKey {
    pub(super) cache_key: usize,
    pub(super) document_generation: u64,
}

#[derive(Debug, Clone)]
pub(super) struct RenderJob {
    pub(super) key: RenderJobKey,
    pub(super) source_path: PathBuf,
    pub(super) index: usize,
    pub(super) mode: NotesMode,
    pub(super) view: PageView,
    pub(super) layout: Option<NotesLayout>,
    pub(super) target_width: usize,
    pub(super) namespace: usize,
    pub(super) priority: RenderPriority,
}

#[derive(Debug)]
pub(super) enum RenderResult {
    Ready {
        key: RenderJobKey,
        namespace: usize,
        bitmap: PageBitmap,
    },
    Cancelled {
        key: RenderJobKey,
    },
    Failed {
        key: RenderJobKey,
        message: String,
    },
}

pub(super) struct RenderWorker {
    high_priority_jobs: Sender<RenderJob>,
    normal_priority_jobs: Sender<RenderJob>,
    low_priority_jobs: Sender<RenderJob>,
    shutdown: Sender<()>,
    results: Receiver<RenderResult>,
    queued_keys: BTreeSet<RenderJobKey>,
    latest_requested_generation: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl RenderWorker {
    pub(super) fn start() -> Self {
        let (high_priority_sender, high_priority_receiver) =
            bounded(HIGH_PRIORITY_RENDER_QUEUE_CAPACITY);
        let (normal_priority_sender, normal_priority_receiver) =
            bounded(NORMAL_PRIORITY_RENDER_QUEUE_CAPACITY);
        let (low_priority_sender, low_priority_receiver) =
            bounded(LOW_PRIORITY_RENDER_QUEUE_CAPACITY);
        let (shutdown_sender, shutdown_receiver) = bounded(1);
        let (result_sender, result_receiver) = bounded(RENDER_RESULT_QUEUE_CAPACITY);
        let latest_requested_generation = Arc::new(AtomicU64::new(0));
        let worker_latest_requested_generation = Arc::clone(&latest_requested_generation);
        let handle = match thread::Builder::new()
            .name("decklight-render-worker".to_owned())
            .spawn(move || {
                worker_loop(
                    high_priority_receiver,
                    normal_priority_receiver,
                    low_priority_receiver,
                    shutdown_receiver,
                    result_sender,
                    worker_latest_requested_generation,
                )
            }) {
            Ok(handle) => Some(handle),
            Err(error) => {
                eprintln!("failed to start render worker: {error}");
                None
            }
        };

        Self {
            high_priority_jobs: high_priority_sender,
            normal_priority_jobs: normal_priority_sender,
            low_priority_jobs: low_priority_sender,
            shutdown: shutdown_sender,
            results: result_receiver,
            queued_keys: BTreeSet::new(),
            latest_requested_generation,
            handle,
        }
    }

    pub(super) fn request(&mut self, job: RenderJob) {
        if self.queued_keys.contains(&job.key) {
            return;
        }

        let key = job.key;
        self.set_latest_generation(key.document_generation);
        let sender = match job.priority {
            RenderPriority::High => &self.high_priority_jobs,
            RenderPriority::Normal => &self.normal_priority_jobs,
            RenderPriority::Low => &self.low_priority_jobs,
        };

        match sender.try_send(job) {
            Ok(()) => {
                self.queued_keys.insert(key);
            }
            Err(TrySendError::Full(_job)) => {}
            Err(TrySendError::Disconnected(_job)) => {}
        }
    }

    pub(super) fn drain_results(&mut self) -> Vec<RenderResult> {
        let mut results = Vec::with_capacity(1);
        if let Ok(result) = self.results.try_recv() {
            self.queued_keys.remove(&result.key());
            results.push(result);
        }
        results
    }

    pub(super) fn set_latest_generation(&self, generation: u64) {
        self.latest_requested_generation
            .store(generation, Ordering::Release);
    }

    pub(super) fn has_pending_work(&self) -> bool {
        !self.queued_keys.is_empty()
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        if self.shutdown.try_send(()).is_err() {
            eprintln!("render worker shutdown signal was not delivered");
        }
        let (replacement_sender, _replacement_receiver) = bounded(0);
        let old_high_priority_sender =
            std::mem::replace(&mut self.high_priority_jobs, replacement_sender.clone());
        let old_normal_priority_sender =
            std::mem::replace(&mut self.normal_priority_jobs, replacement_sender.clone());
        let old_low_priority_sender =
            std::mem::replace(&mut self.low_priority_jobs, replacement_sender);
        drop(old_high_priority_sender);
        drop(old_normal_priority_sender);
        drop(old_low_priority_sender);
        let (_replacement_result_sender, replacement_result_receiver) = bounded(0);
        let old_result_receiver = std::mem::replace(&mut self.results, replacement_result_receiver);
        drop(old_result_receiver);
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            eprintln!("render worker thread panicked during shutdown");
        }
    }
}

impl RenderResult {
    fn key(&self) -> RenderJobKey {
        match self {
            Self::Ready { key, .. } | Self::Cancelled { key } | Self::Failed { key, .. } => *key,
        }
    }
}

fn generation_is_current(job_generation: u64, latest_generation: &AtomicU64) -> bool {
    latest_generation.load(Ordering::Acquire) == job_generation
}

fn worker_loop(
    high_priority_receiver: Receiver<RenderJob>,
    normal_priority_receiver: Receiver<RenderJob>,
    low_priority_receiver: Receiver<RenderJob>,
    shutdown_receiver: Receiver<()>,
    result_sender: Sender<RenderResult>,
    latest_generation: Arc<AtomicU64>,
) {
    loop {
        if shutdown_receiver.try_recv().is_ok() {
            break;
        }

        let message = high_priority_receiver
            .try_recv()
            .or_else(|_| normal_priority_receiver.try_recv())
            .or_else(|_| low_priority_receiver.try_recv());
        if let Ok(job) = message {
            let result = render_job(&job, latest_generation.as_ref());
            if !send_render_result(&job, result, latest_generation.as_ref(), &result_sender) {
                break;
            }
            continue;
        }

        select! {
            recv(shutdown_receiver) -> _message => break,
            recv(high_priority_receiver) -> message => {
                if !handle_job_message(message, latest_generation.as_ref(), &result_sender) {
                    break;
                }
            },
            recv(normal_priority_receiver) -> message => {
                if !handle_job_message(message, latest_generation.as_ref(), &result_sender) {
                    break;
                }
            },
            recv(low_priority_receiver) -> message => {
                if !handle_job_message(message, latest_generation.as_ref(), &result_sender) {
                    break;
                }
            },
        }
    }
}

fn handle_job_message(
    message: Result<RenderJob, crossbeam_channel::RecvError>,
    latest_generation: &AtomicU64,
    result_sender: &Sender<RenderResult>,
) -> bool {
    let Ok(job) = message else {
        return false;
    };
    let result = render_job(&job, latest_generation);
    send_render_result(&job, result, latest_generation, result_sender)
}

fn render_job(job: &RenderJob, latest_generation: &AtomicU64) -> RenderResult {
    if !generation_is_current(job.key.document_generation, latest_generation) {
        return RenderResult::Cancelled { key: job.key };
    }

    match render_page_bitmap_from_path(
        &job.source_path,
        job.index,
        job.view,
        job.layout,
        job.target_width,
    ) {
        Ok(bitmap) if generation_is_current(job.key.document_generation, latest_generation) => {
            RenderResult::Ready {
                key: job.key,
                namespace: job.namespace,
                bitmap,
            }
        }
        Ok(_bitmap) => RenderResult::Cancelled { key: job.key },
        Err(_error) if !generation_is_current(job.key.document_generation, latest_generation) => {
            RenderResult::Cancelled { key: job.key }
        }
        Err(error) => RenderResult::Failed {
            key: job.key,
            message: format!(
                "render failed for page {} {:?} mode {:?}: {error}",
                job.index + 1,
                job.view,
                job.mode
            ),
        },
    }
}

fn send_render_result(
    job: &RenderJob,
    mut result: RenderResult,
    latest_generation: &AtomicU64,
    result_sender: &Sender<RenderResult>,
) -> bool {
    if !generation_is_current(job.key.document_generation, latest_generation) {
        result = RenderResult::Cancelled { key: job.key };
    }
    result_sender.send(result).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_queue_capacities_limit_bitmap_residency() {
        assert_eq!(HIGH_PRIORITY_RENDER_QUEUE_CAPACITY, 4);
        assert_eq!(NORMAL_PRIORITY_RENDER_QUEUE_CAPACITY, 4);
        assert_eq!(LOW_PRIORITY_RENDER_QUEUE_CAPACITY, 4);
        assert_eq!(RENDER_RESULT_QUEUE_CAPACITY, 1);
    }

    fn test_job(document_generation: u64) -> RenderJob {
        RenderJob {
            key: RenderJobKey {
                cache_key: 1,
                document_generation,
            },
            source_path: PathBuf::from("missing.pdf"),
            index: 0,
            mode: NotesMode::Auto,
            view: PageView::Full,
            layout: None,
            target_width: 800,
            namespace: 0,
            priority: RenderPriority::Normal,
        }
    }

    #[test]
    fn obsolete_jobs_cancel_before_rendering() {
        let latest_generation = AtomicU64::new(2);
        let job = test_job(1);

        assert!(matches!(
            render_job(&job, &latest_generation),
            RenderResult::Cancelled { key } if key == job.key
        ));
    }

    #[test]
    fn stale_ready_results_are_replaced_before_send() {
        let latest_generation = AtomicU64::new(2);
        let job = test_job(1);
        let bitmap = PageBitmap {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        let result = RenderResult::Ready {
            key: job.key,
            namespace: job.namespace,
            bitmap,
        };
        let (sender, receiver) = bounded(1);

        assert!(send_render_result(
            &job,
            result,
            &latest_generation,
            &sender
        ));
        assert!(matches!(
            receiver.try_recv().expect("cancelled result should be queued"),
            RenderResult::Cancelled { key } if key == job.key
        ));
    }

    #[test]
    fn cancelled_results_preserve_queue_bookkeeping_key() {
        let key = RenderJobKey {
            cache_key: 7,
            document_generation: 3,
        };
        assert_eq!(RenderResult::Cancelled { key }.key(), key);
    }
}
