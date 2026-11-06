//! Evaluates on a thread of its own, so that requests need not wait: a large
//! project takes a while, and an editor asks at every key.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};
use lsp_types::Uri;

use crate::Settings;
use crate::convert::Encoding;
use crate::workspace::{self, Checks, RemoteStatus, Snapshot};

/// What to evaluate.
pub(crate) struct Job {
    pub(crate) settings: Settings,
    /// The open buffers.
    pub(crate) open: HashMap<PathBuf, String>,
    pub(crate) remote: RemoteStatus,
    pub(crate) encoding: Encoding,
}

/// An evaluation, with each file's diagnostics.
pub(crate) struct Evaluated {
    pub(crate) snapshot: Snapshot,
    pub(crate) diagnostics: Vec<(Uri, Vec<lsp_types::Diagnostic>)>,
}

/// The thread. It ends when this is dropped.
pub(crate) struct Worker {
    jobs: Sender<Job>,
    pub(crate) results: Receiver<Evaluated>,
}

impl Worker {
    pub(crate) fn start(root: Option<PathBuf>, checks: Option<Checks>) -> Self {
        let (jobs, job_receiver) = crossbeam_channel::unbounded::<Job>();
        let (result_sender, results) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for job in job_receiver {
                let snapshot = workspace::evaluate(
                    root.as_deref(),
                    &job.settings,
                    &job.open,
                    checks.as_ref(),
                    &job.remote,
                );
                let diagnostics = snapshot.diagnostics(job.encoding);
                let evaluated = Evaluated {
                    snapshot,
                    diagnostics,
                };
                if result_sender.send(evaluated).is_err() {
                    return;
                }
            }
        });
        Self { jobs, results }
    }

    /// Starts an evaluation. If the thread is gone, the server sees that
    /// from its results.
    pub(crate) fn evaluate(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}
