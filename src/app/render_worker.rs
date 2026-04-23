use crate::config::{NotesLayout, NotesMode};
use crate::document::{DocumentBackend, PageBitmap, PageView, open_document};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

const HIGH_PRIORITY_RENDER_QUEUE_CAPACITY: usize = 8;
const NORMAL_PRIORITY_RENDER_QUEUE_CAPACITY: usize = 12;
const LOW_PRIORITY_RENDER_QUEUE_CAPACITY: usize = 16;
const RENDER_RESULT_QUEUE_CAPACITY: usize = 4;

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
        let handle = match thread::Builder::new()
            .name("decklight-render-worker".to_owned())
            .spawn(move || {
                worker_loop(
                    high_priority_receiver,
                    normal_priority_receiver,
                    low_priority_receiver,
                    shutdown_receiver,
                    result_sender,
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
            handle,
        }
    }

    pub(super) fn request(&mut self, job: RenderJob) {
        if self.queued_keys.contains(&job.key) {
            return;
        }

        let key = job.key;
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
        let mut results = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            self.queued_keys.remove(&result.key());
            results.push(result);
        }
        results
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
            Self::Ready { key, .. } | Self::Failed { key, .. } => *key,
        }
    }
}

fn worker_loop(
    high_priority_receiver: Receiver<RenderJob>,
    normal_priority_receiver: Receiver<RenderJob>,
    low_priority_receiver: Receiver<RenderJob>,
    shutdown_receiver: Receiver<()>,
    result_sender: Sender<RenderResult>,
) {
    let mut document_cache = None;

    loop {
        if shutdown_receiver.try_recv().is_ok() {
            break;
        }

        let message = high_priority_receiver
            .try_recv()
            .or_else(|_| normal_priority_receiver.try_recv())
            .or_else(|_| low_priority_receiver.try_recv());
        if let Ok(job) = message {
            let result = render_job(&job, &mut document_cache);
            if result_sender.send(result).is_err() {
                break;
            }
            continue;
        }

        select! {
            recv(shutdown_receiver) -> _message => break,
            recv(high_priority_receiver) -> message => {
                if !handle_job_message(message, &mut document_cache, &result_sender) {
                    break;
                }
            },
            recv(normal_priority_receiver) -> message => {
                if !handle_job_message(message, &mut document_cache, &result_sender) {
                    break;
                }
            },
            recv(low_priority_receiver) -> message => {
                if !handle_job_message(message, &mut document_cache, &result_sender) {
                    break;
                }
            },
        }
    }
}

fn handle_job_message(
    message: Result<RenderJob, crossbeam_channel::RecvError>,
    document_cache: &mut Option<WorkerDocumentCache>,
    result_sender: &Sender<RenderResult>,
) -> bool {
    let Ok(job) = message else {
        return false;
    };
    let result = render_job(&job, document_cache);
    result_sender.send(result).is_ok()
}

struct WorkerDocumentCache {
    source_path: PathBuf,
    document_generation: u64,
    backend: Box<dyn DocumentBackend>,
}

fn render_job(job: &RenderJob, document_cache: &mut Option<WorkerDocumentCache>) -> RenderResult {
    let cache_matches = document_cache.as_ref().is_some_and(|cached| {
        cached.source_path == job.source_path
            && cached.document_generation == job.key.document_generation
    });

    if !cache_matches {
        match open_document(&job.source_path) {
            Ok(backend) => {
                *document_cache = Some(WorkerDocumentCache {
                    source_path: job.source_path.clone(),
                    document_generation: job.key.document_generation,
                    backend,
                });
            }
            Err(error) => {
                return RenderResult::Failed {
                    key: job.key,
                    message: format!("failed to open {}: {error}", job.source_path.display()),
                };
            }
        }
    }

    let Some(cached) = document_cache.as_ref() else {
        return RenderResult::Failed {
            key: job.key,
            message: "render worker document cache is unavailable".to_owned(),
        };
    };

    match cached
        .backend
        .render_page_bitmap(job.index, job.view, job.layout, job.target_width)
    {
        Ok(bitmap) => RenderResult::Ready {
            key: job.key,
            namespace: job.namespace,
            bitmap,
        },
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
