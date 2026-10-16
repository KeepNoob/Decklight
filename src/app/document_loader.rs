use crate::document::{DocumentBackend, open_document};
use anyhow::{Context as _, Result, anyhow};
use crossbeam_channel::{Receiver, TryRecvError, bounded};
use eframe::egui::Context;
use std::path::{Path, PathBuf};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DocumentLoadKind {
    Open,
    Reload,
}

/// Parses a PDF on a background thread so large documents do not freeze the UI.
pub(super) struct DocumentLoad {
    path: PathBuf,
    kind: DocumentLoadKind,
    result: Receiver<Result<Box<dyn DocumentBackend>>>,
}

impl DocumentLoad {
    pub(super) fn start(
        path: PathBuf,
        kind: DocumentLoadKind,
        repaint_context: Context,
    ) -> Result<Self> {
        let (sender, receiver) = bounded(1);
        thread::Builder::new()
            .name("decklight-document-loader".to_owned())
            .spawn({
                let path = path.clone();
                move || {
                    let result = open_document(&path);
                    // A closed channel means a newer load replaced this one, so its result is unwanted.
                    if sender.send(result).is_ok() {
                        repaint_context.request_repaint();
                    }
                }
            })
            .context("failed to spawn document loader thread")?;

        Ok(Self {
            path,
            kind,
            result: receiver,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn kind(&self) -> DocumentLoadKind {
        self.kind
    }

    /// Returns the outcome once loading has finished.
    pub(super) fn try_finish(&self) -> Option<Result<Box<dyn DocumentBackend>>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow!(
                "document loader stopped before finishing {}",
                self.path.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn missing_documents_report_an_error_without_blocking() {
        let load = DocumentLoad::start(
            PathBuf::from("does-not-exist.pdf"),
            DocumentLoadKind::Open,
            Context::default(),
        )
        .expect("start loader");

        let deadline = Instant::now() + Duration::from_secs(10);
        let outcome = loop {
            if let Some(outcome) = load.try_finish() {
                break outcome;
            }
            assert!(Instant::now() < deadline, "loader did not finish");
            thread::yield_now();
        };

        assert!(outcome.is_err());
        assert_eq!(load.kind(), DocumentLoadKind::Open);
    }
}
