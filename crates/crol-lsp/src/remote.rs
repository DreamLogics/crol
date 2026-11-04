//! Fetching schemas named by URL or crate without blocking the server
//! (`SPEC.md` 4.1). Evaluation takes them from the cache; when one is
//! fetched, the server evaluates again.

use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(feature = "remote-schemas")]
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
#[cfg(feature = "remote-schemas")]
use crossbeam_channel::Sender;

use crate::workspace::RemoteStatus;

/// A schema's source and the directory of what names it.
pub(crate) type Key = (String, PathBuf);

/// A fetch that ended, with its error if it failed.
pub(crate) struct Fetched {
    #[cfg_attr(not(feature = "remote-schemas"), allow(dead_code))]
    key: Key,
    #[cfg_attr(not(feature = "remote-schemas"), allow(dead_code))]
    result: Result<(), String>,
}

#[cfg(feature = "remote-schemas")]
enum State {
    Fetching,
    Fetched,
    Failed(Instant, String),
}

/// How long a failed fetch waits before it is tried again.
#[cfg(feature = "remote-schemas")]
const RETRY: Duration = Duration::from_secs(60);

pub(crate) struct Fetcher {
    /// Started at the first fetch.
    #[cfg(feature = "remote-schemas")]
    runtime: Option<tokio::runtime::Runtime>,
    #[cfg(feature = "remote-schemas")]
    sender: Sender<Fetched>,
    /// Where fetches that ended arrive, for the server's loop.
    pub(crate) receiver: Receiver<Fetched>,
    #[cfg(feature = "remote-schemas")]
    states: HashMap<Key, State>,
}

impl Fetcher {
    #[cfg(feature = "remote-schemas")]
    pub(crate) fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::unbounded();
        Self {
            runtime: None,
            sender,
            receiver,
            states: HashMap::new(),
        }
    }

    /// Without the `remote-schemas` feature nothing is fetched.
    #[cfg(not(feature = "remote-schemas"))]
    pub(crate) fn new() -> Self {
        Self {
            receiver: crossbeam_channel::never(),
        }
    }

    /// What evaluation says about schemas being fetched or that failed.
    pub(crate) fn status(&self) -> RemoteStatus {
        #[cfg(feature = "remote-schemas")]
        return self
            .states
            .iter()
            .filter_map(|(key, state)| match state {
                State::Fetching => Some((key.clone(), format!("schema: fetching `{}`", key.0))),
                State::Failed(_, message) => Some((key.clone(), message.clone())),
                State::Fetched => None,
            })
            .collect();
        #[cfg(not(feature = "remote-schemas"))]
        HashMap::new()
    }

    /// Starts fetching each wanted schema not fetched yet, and those that
    /// failed a while ago. Returns whether it started any.
    pub(crate) fn fetch(&mut self, wanted: &[Key]) -> bool {
        #[cfg(feature = "remote-schemas")]
        {
            let mut started = false;
            for key in wanted {
                let due = match self.states.get(key) {
                    None => true,
                    Some(State::Failed(at, _)) => at.elapsed() >= RETRY,
                    Some(State::Fetching | State::Fetched) => false,
                };
                if !due {
                    continue;
                }
                let runtime = match &self.runtime {
                    Some(runtime) => runtime,
                    None => match tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .thread_name("crol-fetch")
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => self.runtime.insert(runtime),
                        Err(e) => {
                            let message = format!("schema: cannot start fetching: {e}");
                            self.states
                                .insert(key.clone(), State::Failed(Instant::now(), message));
                            started = true;
                            continue;
                        }
                    },
                };
                self.states.insert(key.clone(), State::Fetching);
                started = true;
                let sender = self.sender.clone();
                let key = key.clone();
                runtime.spawn(async move {
                    let result = crol::schema::resolve_remote_async(&key.0, &key.1, true)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    let _ = sender.send(Fetched { key, result });
                });
            }
            started
        }
        #[cfg(not(feature = "remote-schemas"))]
        {
            let _ = wanted;
            false
        }
    }

    /// Records a fetch that ended.
    pub(crate) fn done(&mut self, fetched: Fetched) {
        #[cfg(feature = "remote-schemas")]
        {
            let state = match fetched.result {
                Ok(()) => State::Fetched,
                Err(message) => State::Failed(Instant::now(), message),
            };
            self.states.insert(fetched.key, state);
        }
        #[cfg(not(feature = "remote-schemas"))]
        let _ = fetched;
    }
}
