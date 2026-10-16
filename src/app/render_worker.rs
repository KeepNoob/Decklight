use super::presenter::CachedResolvedLayout;
use crate::config::{NotesLayout, NotesMode};
use crate::document::{
    NotesDetectionResult, PageView, detect_notes_layout_from_path, render_page_bitmap_from_path,
};
use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select};
use eframe::egui::{ColorImage, Context};
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
pub(super) enum RenderJobKind {
    Page,
    Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RenderJobKey {
    pub(super) kind: RenderJobKind,
    pub(super) cache_key: usize,
    pub(super) document_generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum RenderTask {
    Page {
        view: PageView,
        layout: Option<NotesLayout>,
        target_width: usize,
        namespace: usize,
    },
    DetectLayout {
        confidence_threshold: f32,
    },
}

#[derive(Debug, Clone)]
pub(super) struct RenderJob {
    pub(super) key: RenderJobKey,
    /// Jobs requested before the latest slide change are dropped if they have not started yet.
    pub(super) request_generation: u64,
    pub(super) source_path: PathBuf,
    pub(super) index: usize,
    pub(super) mode: NotesMode,
    pub(super) task: RenderTask,
    pub(super) priority: RenderPriority,
}

#[derive(Debug)]
pub(super) enum RenderResult {
    Ready {
        key: RenderJobKey,
        namespace: usize,
        image: ColorImage,
    },
    LayoutReady {
        key: RenderJobKey,
        resolved: CachedResolvedLayout,
    },
    Cancelled {
        key: RenderJobKey,
    },
    Failed {
        key: RenderJobKey,
        message: String,
    },
}

#[derive(Default)]
struct WorkerGenerations {
    document: AtomicU64,
    request: AtomicU64,
}

impl WorkerGenerations {
    fn document_is_current(&self, job: &RenderJob) -> bool {
        self.document.load(Ordering::Acquire) == job.key.document_generation
    }

    fn job_is_wanted(&self, job: &RenderJob) -> bool {
        self.document_is_current(job)
            && self.request.load(Ordering::Acquire) == job.request_generation
    }
}

pub(super) struct RenderWorker {
    high_priority_jobs: Sender<RenderJob>,
    normal_priority_jobs: Sender<RenderJob>,
    low_priority_jobs: Sender<RenderJob>,
    shutdown: Sender<()>,
    results: Receiver<RenderResult>,
    queued_keys: BTreeSet<RenderJobKey>,
    generations: Arc<WorkerGenerations>,
    handle: Option<JoinHandle<()>>,
}

impl RenderWorker {
    /// Starts the worker thread; it asks `repaint_context` to repaint whenever a result is ready.
    pub(super) fn start(repaint_context: Context) -> Self {
        let (high_priority_sender, high_priority_receiver) =
            bounded(HIGH_PRIORITY_RENDER_QUEUE_CAPACITY);
        let (normal_priority_sender, normal_priority_receiver) =
            bounded(NORMAL_PRIORITY_RENDER_QUEUE_CAPACITY);
        let (low_priority_sender, low_priority_receiver) =
            bounded(LOW_PRIORITY_RENDER_QUEUE_CAPACITY);
        let (shutdown_sender, shutdown_receiver) = bounded(1);
        let (result_sender, result_receiver) = bounded(RENDER_RESULT_QUEUE_CAPACITY);
        let generations = Arc::new(WorkerGenerations::default());
        let handle = match thread::Builder::new()
            .name("decklight-render-worker".to_owned())
            .spawn({
                let generations = Arc::clone(&generations);
                move || {
                    worker_loop(
                        WorkerQueues {
                            high_priority: high_priority_receiver,
                            normal_priority: normal_priority_receiver,
                            low_priority: low_priority_receiver,
                            shutdown: shutdown_receiver,
                        },
                        WorkerOutput {
                            results: result_sender,
                            repaint_context,
                        },
                        generations,
                    )
                }
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
            generations,
            handle,
        }
    }

    /// Queues `job`, failing only when the worker thread is no longer running.
    pub(super) fn request(&mut self, job: RenderJob) -> Result<()> {
        if self.queued_keys.contains(&job.key) {
            return Ok(());
        }

        let key = job.key;
        let sender = match job.priority {
            RenderPriority::High => &self.high_priority_jobs,
            RenderPriority::Normal => &self.normal_priority_jobs,
            RenderPriority::Low => &self.low_priority_jobs,
        };

        // A full queue drops the request; the UI asks again on the next frame.
        match sender.try_send(job) {
            Ok(()) => {
                self.queued_keys.insert(key);
                Ok(())
            }
            Err(TrySendError::Full(_job)) => Ok(()),
            Err(TrySendError::Disconnected(_job)) => Err(anyhow!(
                "The render worker stopped; slides can no longer be rendered. Restart Decklight."
            )),
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

    pub(super) fn set_document_generation(&self, generation: u64) {
        self.generations
            .document
            .store(generation, Ordering::Release);
    }

    pub(super) fn set_request_generation(&self, generation: u64) {
        self.generations
            .request
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
            Self::Ready { key, .. }
            | Self::LayoutReady { key, .. }
            | Self::Cancelled { key }
            | Self::Failed { key, .. } => *key,
        }
    }
}

/// Applies the confidence threshold to a detection result, matching what the UI displays.
pub(super) fn resolve_detected_layout(
    mode: NotesMode,
    confidence_threshold: f32,
    detection: Result<NotesDetectionResult>,
) -> CachedResolvedLayout {
    match detection {
        Ok(result) => {
            let mut warning = result.warning.clone();
            let layout = if mode == NotesMode::Auto {
                match result.layout {
                    Some(layout) if result.confidence >= confidence_threshold => Some(layout),
                    _ => {
                        warning = Some(format!(
                            "Auto split confidence {:.2} below threshold {:.2}; using full page.",
                            result.confidence, confidence_threshold
                        ));
                        None
                    }
                }
            } else {
                result.layout
            };
            CachedResolvedLayout {
                layout,
                reason: Some(result.reason),
                warning,
            }
        }
        Err(error) => CachedResolvedLayout {
            layout: None,
            reason: Some("Falling back to full-page rendering.".to_owned()),
            warning: Some(format!("Layout detection failed: {error}")),
        },
    }
}

struct WorkerQueues {
    high_priority: Receiver<RenderJob>,
    normal_priority: Receiver<RenderJob>,
    low_priority: Receiver<RenderJob>,
    shutdown: Receiver<()>,
}

struct WorkerOutput {
    results: Sender<RenderResult>,
    repaint_context: Context,
}

impl WorkerOutput {
    fn send(&self, result: RenderResult) -> bool {
        if self.results.send(result).is_err() {
            return false;
        }
        self.repaint_context.request_repaint();
        true
    }
}

fn worker_loop(queues: WorkerQueues, output: WorkerOutput, generations: Arc<WorkerGenerations>) {
    loop {
        if queues.shutdown.try_recv().is_ok() {
            break;
        }

        let message = queues
            .high_priority
            .try_recv()
            .or_else(|_| queues.normal_priority.try_recv())
            .or_else(|_| queues.low_priority.try_recv());
        if let Ok(job) = message {
            if !output.send(run_job(&job, &generations)) {
                break;
            }
            continue;
        }

        let message = select! {
            recv(queues.shutdown) -> _message => break,
            recv(queues.high_priority) -> message => message,
            recv(queues.normal_priority) -> message => message,
            recv(queues.low_priority) -> message => message,
        };
        let Ok(job) = message else {
            break;
        };
        if !output.send(run_job(&job, &generations)) {
            break;
        }
    }
}

fn run_job(job: &RenderJob, generations: &WorkerGenerations) -> RenderResult {
    if !generations.job_is_wanted(job) {
        return RenderResult::Cancelled { key: job.key };
    }

    let result = match job.task {
        RenderTask::Page {
            view,
            layout,
            target_width,
            namespace,
        } => render_page(job, view, layout, target_width, namespace),
        RenderTask::DetectLayout {
            confidence_threshold,
        } => RenderResult::LayoutReady {
            key: job.key,
            resolved: resolve_detected_layout(
                job.mode,
                confidence_threshold,
                detect_notes_layout_from_path(&job.source_path, job.index, job.mode),
            ),
        },
    };

    // Finished work stays useful after a slide change, but not after the document changed.
    if generations.document_is_current(job) {
        result
    } else {
        RenderResult::Cancelled { key: job.key }
    }
}

fn render_page(
    job: &RenderJob,
    view: PageView,
    layout: Option<NotesLayout>,
    target_width: usize,
    namespace: usize,
) -> RenderResult {
    match render_page_bitmap_from_path(&job.source_path, job.index, view, layout, target_width) {
        Ok(bitmap) => RenderResult::Ready {
            key: job.key,
            namespace,
            // Converting here keeps the per-pixel work off the UI thread.
            image: ColorImage::from_rgba_unmultiplied([bitmap.width, bitmap.height], &bitmap.rgba),
        },
        Err(error) => RenderResult::Failed {
            key: job.key,
            message: format!(
                "render failed for page {} {:?} mode {:?}: {error}",
                job.index + 1,
                view,
                job.mode
            ),
        },
    }
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

    fn test_job(document_generation: u64, request_generation: u64) -> RenderJob {
        RenderJob {
            key: RenderJobKey {
                kind: RenderJobKind::Page,
                cache_key: 1,
                document_generation,
            },
            request_generation,
            source_path: PathBuf::from("missing.pdf"),
            index: 0,
            mode: NotesMode::Auto,
            task: RenderTask::Page {
                view: PageView::Full,
                layout: None,
                target_width: 800,
                namespace: 0,
            },
            priority: RenderPriority::Normal,
        }
    }

    fn generations(document: u64, request: u64) -> WorkerGenerations {
        WorkerGenerations {
            document: AtomicU64::new(document),
            request: AtomicU64::new(request),
        }
    }

    #[test]
    fn obsolete_document_jobs_cancel_before_rendering() {
        let job = test_job(1, 0);

        assert!(matches!(
            run_job(&job, &generations(2, 0)),
            RenderResult::Cancelled { key } if key == job.key
        ));
    }

    #[test]
    fn jobs_from_before_a_slide_change_cancel_before_rendering() {
        let job = test_job(1, 3);

        assert!(matches!(
            run_job(&job, &generations(1, 4)),
            RenderResult::Cancelled { key } if key == job.key
        ));
    }

    #[test]
    fn current_jobs_run_and_report_failures() {
        let job = test_job(1, 3);

        assert!(matches!(
            run_job(&job, &generations(1, 3)),
            RenderResult::Failed { key, .. } if key == job.key
        ));
    }

    #[test]
    fn cancelled_results_preserve_queue_bookkeeping_key() {
        let key = RenderJobKey {
            kind: RenderJobKind::Layout,
            cache_key: 7,
            document_generation: 3,
        };
        assert_eq!(RenderResult::Cancelled { key }.key(), key);
    }

    #[test]
    fn low_confidence_auto_detection_falls_back_to_full_page() {
        let detection = NotesDetectionResult {
            layout: Some(NotesLayout {
                axis: crate::config::SplitAxis::Vertical,
                split_ratio: 0.5,
                notes_side: crate::config::NotesSide::Right,
                confidence: 0.3,
            }),
            confidence: 0.3,
            reason: "weak split".to_owned(),
            warning: None,
        };

        let resolved = resolve_detected_layout(NotesMode::Auto, 0.6, Ok(detection));

        assert_eq!(resolved.layout, None);
        assert!(
            resolved
                .warning
                .is_some_and(|warning| warning.contains("below threshold"))
        );
    }
}
