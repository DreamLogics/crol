//! The CROL language server, as a library (`SPEC.md` 12.5).
//!
//! `crol lsp` runs [`serve_stdio`]. An application can build its own server
//! with [`Server::with_checks`], passing a function that receives the
//! evaluated data and returns its own diagnostics, which are published with
//! the others.
//!
//! The server evaluates the project (`SPEC.md` 12.4) found from the
//! workspace folder, or each open document on its own, on top of what it
//! extends. Open buffers replace their files, saved or not. After an edit it
//! parses the file again at
//! once, for syntax errors, and evaluates everything after a pause in
//! typing, on a thread of its own: requests are answered at once from the
//! latest evaluation, whatever is being evaluated.

#![forbid(unsafe_code)]

mod actions;
mod attributes;
mod completion;
mod convert;
mod docs;
mod features;
mod hints;
mod navigate;
mod place;
mod remote;
mod schema;
mod tokens;
mod worker;
mod workspace;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{self, Notification as _};
use lsp_types::request::{self, Request as _};
use lsp_types::{InitializeParams, PositionEncodingKind, ServerCapabilities, Uri};
use serde_json::Value as Json;

pub use workspace::{Checks, Snapshot};

use convert::Encoding;

/// Settings, from the initialization options and changeable at run time
/// (`SPEC.md` 12.5).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The context for inlay hints and the first context shown on hover,
    /// by its variants; default: the base.
    pub context: Vec<String>,
    /// Whether to show inlay hints; default: off.
    pub inlay_hints: bool,
    /// The pause in typing before evaluating, in milliseconds.
    pub debounce_ms: u64,
    /// How many contexts a hover lists values for.
    pub max_contexts_in_hover: usize,
    /// A project file, overriding the search from the workspace folder.
    pub project: Option<PathBuf>,
    /// Whether schemas named by URL or crate may be fetched; without it,
    /// only cached copies and local crates are used. Default: on.
    pub remote_schemas: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            context: Vec::new(),
            inlay_hints: false,
            debounce_ms: 150,
            max_contexts_in_hover: 8,
            project: None,
            remote_schemas: true,
        }
    }
}

impl Settings {
    fn update(&mut self, json: &Json) {
        let json = json.get("crol").unwrap_or(json);
        if let Some(context) = json.get("context").and_then(Json::as_array) {
            self.context = context
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(on) = json.get("inlay_hints").and_then(Json::as_bool) {
            self.inlay_hints = on;
        }
        if let Some(ms) = json.get("debounce_ms").and_then(Json::as_u64) {
            self.debounce_ms = ms;
        }
        if let Some(n) = json.get("max_contexts_in_hover").and_then(Json::as_u64) {
            self.max_contexts_in_hover = n as usize;
        }
        if let Some(project) = json.get("project").and_then(Json::as_str) {
            self.project = Some(PathBuf::from(project));
        }
        if let Some(on) = json.get("remote_schemas").and_then(Json::as_bool) {
            self.remote_schemas = on;
        }
    }
}

/// The language server.
pub struct Server {
    connection: Connection,
    encoding: Encoding,
    root: Option<PathBuf>,
    settings: Settings,
    /// Open buffers by path, with their text.
    open: HashMap<PathBuf, String>,
    snapshot: Option<Snapshot>,
    /// When the last edit happened, if it is not evaluated yet.
    dirty_since: Option<Instant>,
    /// The diagnostics last published, by URI, for the files that have
    /// any. (A `Uri` caches internally, so it is not a hash key.)
    published: HashMap<String, Vec<lsp_types::Diagnostic>>,
    /// Until it runs, the application's own checks.
    checks: Option<Checks>,
    /// Whether every file has had its diagnostics, even none, which clears
    /// what an editor still shows of an earlier session.
    published_all: bool,
    /// Evaluates, once the server runs.
    worker: Option<worker::Worker>,
    /// Whether the worker is evaluating.
    evaluating: bool,
    /// Whether to evaluate once the worker is done: what it is evaluating
    /// is out of date.
    evaluate_again: bool,
    /// Requests that wait for an evaluation, as nothing is known yet of the
    /// document they are about.
    waiting: Vec<Request>,
    /// Whether requests wait until the next evaluation is done.
    hold_requests: bool,
    /// Whether the client can be asked to request inlay hints again.
    refresh_hints: bool,
    /// How many requests the server has sent the client, for their ids.
    requests_sent: u64,
    /// Fetches schemas named by URL or crate.
    fetcher: remote::Fetcher,
}

/// Whether a request is about the text of a document alone.
fn answered_from_text(method: &str) -> bool {
    [
        request::DocumentSymbolRequest::METHOD,
        request::Formatting::METHOD,
        request::PrepareRenameRequest::METHOD,
    ]
    .contains(&method)
}

/// Runs `crol lsp`: the server on standard input and output.
pub fn serve_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (connection, threads) = Connection::stdio();
    Server::start(connection, None)?.run()?;
    threads.join()?;
    Ok(())
}

impl Server {
    /// Initializes a server on `connection`, with the application's own
    /// checks if any.
    pub fn start(
        connection: Connection,
        checks: Option<Checks>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (id, params) = connection.initialize_start()?;
        let params: InitializeParams = serde_json::from_value(params)?;
        let encoding = if params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_ref())
            .is_some_and(|e| e.contains(&PositionEncodingKind::UTF8))
        {
            Encoding::Utf8
        } else {
            Encoding::Utf16
        };
        #[allow(deprecated)]
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|f| f.first())
            .map(|f| &f.uri)
            .or(params.root_uri.as_ref())
            .and_then(convert::uri_to_path);
        let refresh_hints = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.inlay_hint.as_ref())
            .and_then(|h| h.refresh_support)
            .unwrap_or(false);
        let watch_files = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.did_change_watched_files.as_ref())
            .and_then(|w| w.dynamic_registration)
            .unwrap_or(false);
        let mut settings = Settings::default();
        if let Some(options) = &params.initialization_options {
            settings.update(options);
        }
        let capabilities = ServerCapabilities {
            position_encoding: Some(match encoding {
                Encoding::Utf8 => PositionEncodingKind::UTF8,
                Encoding::Utf16 => PositionEncodingKind::UTF16,
            }),
            text_document_sync: Some(lsp_types::TextDocumentSyncCapability::Kind(
                lsp_types::TextDocumentSyncKind::FULL,
            )),
            document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
            document_formatting_provider: Some(lsp_types::OneOf::Left(true)),
            hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
            definition_provider: Some(lsp_types::OneOf::Left(true)),
            references_provider: Some(lsp_types::OneOf::Left(true)),
            rename_provider: Some(lsp_types::OneOf::Right(lsp_types::RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: Default::default(),
            })),
            workspace_symbol_provider: Some(lsp_types::OneOf::Left(true)),
            inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
            code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
                lsp_types::CodeActionOptions {
                    code_action_kinds: Some(vec![
                        lsp_types::CodeActionKind::QUICKFIX,
                        lsp_types::CodeActionKind::REFACTOR_REWRITE,
                    ]),
                    ..Default::default()
                },
            )),
            semantic_tokens_provider: Some(
                lsp_types::SemanticTokensOptions {
                    legend: tokens::legend(),
                    full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                    ..Default::default()
                }
                .into(),
            ),
            completion_provider: Some(lsp_types::CompletionOptions {
                // Where a key or value starts, not only where a path goes
                // on: editors ask on these and on word characters only.
                trigger_characters: Some(
                    [".", "&", "<", "@", ":", " ", "{", "[", "(", ","]
                        .map(str::to_owned)
                        .to_vec(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };
        connection.initialize_finish(
            id,
            serde_json::json!({
                "capabilities": capabilities,
                "serverInfo": { "name": "crol", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        let mut server = Server {
            connection,
            encoding,
            root,
            settings,
            open: HashMap::new(),
            snapshot: None,
            dirty_since: None,
            published: HashMap::new(),
            published_all: false,
            checks,
            worker: None,
            evaluating: false,
            evaluate_again: false,
            waiting: Vec::new(),
            hold_requests: true,
            refresh_hints,
            requests_sent: 0,
            fetcher: remote::Fetcher::new(),
        };
        if watch_files {
            server.watch_files();
        }
        Ok(server)
    }

    /// Asks the client to report changes to files on disk (`SPEC.md` 12.5):
    /// a file that is not open may be a layer, or extended.
    fn watch_files(&mut self) {
        let watchers = ["crod", "crol"]
            .map(|extension| lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::String(format!("**/*.{extension}")),
                kind: None,
            })
            .to_vec();
        let options = lsp_types::DidChangeWatchedFilesRegistrationOptions { watchers };
        let params = lsp_types::RegistrationParams {
            registrations: vec![lsp_types::Registration {
                id: "crol-files".to_owned(),
                method: notification::DidChangeWatchedFiles::METHOD.to_owned(),
                register_options: serde_json::to_value(options).ok(),
            }],
        };
        self.requests_sent += 1;
        self.send(Message::Request(lsp_server::Request::new(
            RequestId::from(format!("crol-{}", self.requests_sent)),
            request::RegisterCapability::METHOD.to_owned(),
            params,
        )));
    }

    /// Adds the application's own checks (`SPEC.md` 12.5).
    pub fn with_checks(mut self, checks: Checks) -> Self {
        self.checks = Some(checks);
        self
    }

    /// Serves until the client asks the server to shut down.
    pub fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let worker = worker::Worker::start(self.root.clone(), self.checks.take());
        let results = worker.results.clone();
        self.worker = Some(worker);
        // After a pause, as the document an editor opens at once is known
        // then, and need not be evaluated for again.
        self.dirty_since = Some(Instant::now());
        loop {
            // A schema fetched, a message, or the pause in typing over.
            let wait = self.dirty_since.map(|since| {
                Duration::from_millis(self.settings.debounce_ms).saturating_sub(since.elapsed())
            });
            let timeout = match wait {
                Some(wait) => crossbeam_channel::after(wait),
                None => crossbeam_channel::never(),
            };
            let message = crossbeam_channel::select! {
                recv(self.connection.receiver) -> message => match message {
                    Ok(message) => message,
                    Err(_) => return Ok(()),
                },
                recv(self.fetcher.receiver) -> fetched => {
                    if let Ok(fetched) = fetched {
                        self.fetcher.done(fetched);
                        self.evaluate();
                    }
                    continue;
                }
                recv(results) -> evaluated => {
                    self.evaluated(evaluated?);
                    continue;
                }
                recv(timeout) -> _ => {
                    self.evaluate();
                    continue;
                }
            };
            match message {
                Message::Request(request) => {
                    if self.connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    self.request(request);
                }
                Message::Notification(notification) => self.notification(notification),
                Message::Response(_) => {}
            }
        }
    }

    fn send(&self, message: Message) {
        // A client that went away ends the loop when the receiver closes.
        let _ = self.connection.sender.send(message);
    }

    fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        self.send(Message::Notification(Notification::new(
            N::METHOD.to_owned(),
            params,
        )));
    }

    fn respond(&self, id: RequestId, result: impl serde::Serialize) {
        self.send(Message::Response(Response::new_ok(id, result)));
    }

    // ---- Notifications ----

    fn notification(&mut self, notification: Notification) {
        match notification.method.as_str() {
            notification::DidOpenTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(
                    notification.params,
                ) && let Some(path) = convert::uri_to_path(&params.text_document.uri)
                {
                    self.open.insert(path.clone(), params.text_document.text);
                    self.edited(&params.text_document.uri);
                    // Until it is evaluated once, nothing is known of a
                    // document in no layer, not even its schema. One read
                    // only for what extends it was not evaluated either.
                    if !self.snapshot.as_ref().is_some_and(|s| {
                        s.file_of(&path)
                            .is_some_and(|f| s.evaluation_of(f).is_some())
                    }) {
                        self.hold_requests = true;
                        self.evaluate();
                    }
                }
            }
            notification::DidChangeTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                    notification.params,
                ) && let Some(path) = convert::uri_to_path(&params.text_document.uri)
                    && let Some(change) = params.content_changes.into_iter().last()
                {
                    self.open.insert(path, change.text);
                    self.edited(&params.text_document.uri);
                }
            }
            notification::DidCloseTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(
                    notification.params,
                ) && let Some(path) = convert::uri_to_path(&params.text_document.uri)
                {
                    self.open.remove(&path);
                    self.dirty_since = Some(Instant::now());
                }
            }
            notification::DidSaveTextDocument::METHOD
            | notification::DidChangeWatchedFiles::METHOD => {
                self.dirty_since = Some(Instant::now());
            }
            notification::DidChangeConfiguration::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeConfigurationParams>(
                    notification.params,
                ) {
                    self.settings.update(&params.settings);
                    self.dirty_since = Some(Instant::now());
                }
            }
            _ => {}
        }
    }

    /// After an edit: the file's syntax errors at once, and an evaluation
    /// after a pause.
    fn edited(&mut self, uri: &Uri) {
        if let Some(path) = convert::uri_to_path(uri)
            && let Some(text) = self.open.get(&path)
        {
            let parse = crol_syntax::parse(text);
            let index = crol_syntax::LineIndex::new(text);
            let diagnostics: Vec<lsp_types::Diagnostic> = parse
                .errors()
                .iter()
                .map(|e| lsp_types::Diagnostic {
                    range: convert::range(text, &index, e.range(), self.encoding),
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    source: Some("crol".to_owned()),
                    message: e.message().to_owned(),
                    ..Default::default()
                })
                .collect();
            if !diagnostics.is_empty() {
                self.published
                    .insert(uri.as_str().to_owned(), diagnostics.clone());
                self.notify::<notification::PublishDiagnostics>(
                    lsp_types::PublishDiagnosticsParams {
                        uri: uri.clone(),
                        diagnostics,
                        version: None,
                    },
                );
            }
        }
        self.dirty_since = Some(Instant::now());
    }

    /// Has the worker evaluate everything, now or as soon as it is done.
    fn evaluate(&mut self) {
        self.dirty_since = None;
        if self.evaluating {
            self.evaluate_again = true;
            return;
        }
        let Some(worker) = &self.worker else {
            return;
        };
        self.evaluating = true;
        worker.evaluate(worker::Job {
            settings: self.settings.clone(),
            open: self.open.clone(),
            remote: self.fetcher.status(),
            encoding: self.encoding,
        });
    }

    /// Takes over an evaluation and publishes its diagnostics, unless
    /// something newer is on its way.
    fn evaluated(&mut self, evaluated: worker::Evaluated) {
        self.evaluating = false;
        let worker::Evaluated {
            mut snapshot,
            diagnostics,
        } = evaluated;
        // Schemas named by URL or crate come from the cache; those not
        // fetched yet this session are fetched, and evaluation says so.
        let fetching = self.settings.remote_schemas && self.fetcher.fetch(&snapshot.remote);
        if let Some(previous) = self.snapshot.take() {
            snapshot.keep_last_good(previous);
        }
        self.snapshot = Some(snapshot);
        let again = fetching || std::mem::take(&mut self.evaluate_again);
        if again {
            self.evaluate();
            return;
        }
        self.publish(diagnostics);
        self.hold_requests = false;
        for request in std::mem::take(&mut self.waiting) {
            self.request(request);
        }
        // Hints are left out while a document has edits not evaluated yet.
        if self.settings.inlay_hints && self.refresh_hints {
            self.requests_sent += 1;
            self.send(Message::Request(lsp_server::Request::new(
                RequestId::from(format!("crol-{}", self.requests_sent)),
                request::InlayHintRefreshRequest::METHOD.to_owned(),
                Json::Null,
            )));
        }
    }

    /// Publishes what changed, and what is open whether it did or not: the
    /// editor is waiting for that. The rest would be a lot of messages for
    /// nothing in a large project.
    fn publish(&mut self, by_file: Vec<(Uri, Vec<lsp_types::Diagnostic>)>) {
        let mut now = HashMap::new();
        let mut listed = HashSet::new();
        for (uri, diagnostics) in by_file {
            let key = uri.as_str().to_owned();
            listed.insert(key.clone());
            let open = convert::uri_to_path(&uri).is_some_and(|path| self.open.contains_key(&path));
            let last = self.published.get(&key).map_or(&[][..], Vec::as_slice);
            if open || !self.published_all || last != diagnostics.as_slice() {
                self.notify::<notification::PublishDiagnostics>(
                    lsp_types::PublishDiagnosticsParams {
                        uri,
                        diagnostics: diagnostics.clone(),
                        version: None,
                    },
                );
            }
            if !diagnostics.is_empty() {
                now.insert(key, diagnostics);
            }
        }
        // Clear the files that are not listed any more.
        self.published.retain(|key, _| !listed.contains(key));
        for key in self.published.keys() {
            if let Ok(uri) = key.parse::<Uri>() {
                self.notify::<notification::PublishDiagnostics>(
                    lsp_types::PublishDiagnosticsParams {
                        uri,
                        diagnostics: Vec::new(),
                        version: None,
                    },
                );
            }
        }
        self.published = now;
        self.published_all = true;
    }

    // ---- Requests ----

    fn request(&mut self, request: Request) {
        // Requests are answered from the latest evaluation, unless one is
        // needed to know anything at all.
        if self.hold_requests && !answered_from_text(&request.method) {
            self.waiting.push(request);
            return;
        }
        let id = request.id.clone();
        let encoding = self.encoding;
        match request.method.as_str() {
            request::DocumentSymbolRequest::METHOD => {
                let result =
                    serde_json::from_value::<lsp_types::DocumentSymbolParams>(request.params)
                        .ok()
                        .and_then(|p| self.text(&p.text_document.uri))
                        .map(|text| {
                            lsp_types::DocumentSymbolResponse::Nested(features::symbols(
                                &text, encoding,
                            ))
                        });
                self.respond(id, result);
            }
            request::Formatting::METHOD => {
                let result =
                    serde_json::from_value::<lsp_types::DocumentFormattingParams>(request.params)
                        .ok()
                        .and_then(|p| self.text(&p.text_document.uri))
                        .and_then(|text| features::format(&text, encoding));
                self.respond(id, result);
            }
            request::HoverRequest::METHOD => {
                let result = serde_json::from_value::<lsp_types::HoverParams>(request.params)
                    .ok()
                    .and_then(|p| {
                        let uri = p.text_document_position_params.text_document.uri;
                        let text = self.text(&uri)?;
                        let snapshot = self.snapshot.as_ref()?;
                        features::hover(
                            snapshot,
                            &uri,
                            &text,
                            p.text_document_position_params.position,
                            encoding,
                            &self.settings,
                        )
                    });
                self.respond(id, result);
            }
            request::Completion::METHOD => {
                let result = serde_json::from_value::<lsp_types::CompletionParams>(request.params)
                    .ok()
                    .and_then(|p| {
                        let uri = p.text_document_position.text_document.uri;
                        let text = self.text(&uri)?;
                        let snapshot = self.snapshot.as_ref()?;
                        Some(completion::complete(
                            snapshot,
                            &uri,
                            &text,
                            p.text_document_position.position,
                            encoding,
                            &self.settings,
                        ))
                    });
                let response = result.map(|(items, incomplete)| {
                    if incomplete {
                        lsp_types::CompletionResponse::List(lsp_types::CompletionList {
                            is_incomplete: true,
                            items,
                        })
                    } else {
                        lsp_types::CompletionResponse::Array(items)
                    }
                });
                self.respond(id, response);
            }
            request::GotoDefinition::METHOD => {
                let result =
                    serde_json::from_value::<lsp_types::GotoDefinitionParams>(request.params)
                        .ok()
                        .and_then(|p| {
                            let uri = p.text_document_position_params.text_document.uri;
                            let text = self.text(&uri)?;
                            let snapshot = self.snapshot.as_ref()?;
                            let mut found = features::definition(
                                snapshot,
                                &uri,
                                &text,
                                p.text_document_position_params.position,
                                encoding,
                            );
                            // An id written in a template may name several
                            // things.
                            match found.len() {
                                0 => None,
                                1 => found.pop().map(lsp_types::GotoDefinitionResponse::Scalar),
                                _ => Some(lsp_types::GotoDefinitionResponse::Array(found)),
                            }
                        });
                self.respond(id, result);
            }
            request::References::METHOD => {
                let result = serde_json::from_value::<lsp_types::ReferenceParams>(request.params)
                    .ok()
                    .and_then(|p| {
                        let uri = p.text_document_position.text_document.uri;
                        let text = self.text(&uri)?;
                        navigate::references(
                            self.snapshot.as_ref(),
                            &uri,
                            &text,
                            p.text_document_position.position,
                            p.context.include_declaration,
                            encoding,
                        )
                    });
                self.respond(id, result);
            }
            request::PrepareRenameRequest::METHOD => {
                let result =
                    serde_json::from_value::<lsp_types::TextDocumentPositionParams>(request.params)
                        .ok()
                        .and_then(|p| {
                            let text = self.text(&p.text_document.uri)?;
                            navigate::prepare_rename(&text, p.position, encoding)
                        });
                self.respond(id, result.map(lsp_types::PrepareRenameResponse::Range));
            }
            request::Rename::METHOD => {
                let result = serde_json::from_value::<lsp_types::RenameParams>(request.params)
                    .ok()
                    .and_then(|p| {
                        let uri = p.text_document_position.text_document.uri;
                        let text = self.text(&uri)?;
                        Some(navigate::rename(
                            self.snapshot.as_ref(),
                            &uri,
                            &text,
                            p.text_document_position.position,
                            &p.new_name,
                            encoding,
                        ))
                    });
                match result {
                    Some(Err(message)) => self.send(Message::Response(Response::new_err(
                        id,
                        lsp_server::ErrorCode::RequestFailed as i32,
                        message,
                    ))),
                    result => self.respond(id, result.and_then(Result::ok).flatten()),
                }
            }
            request::SemanticTokensFullRequest::METHOD => {
                let result =
                    serde_json::from_value::<lsp_types::SemanticTokensParams>(request.params)
                        .ok()
                        .and_then(|p| {
                            let uri = p.text_document.uri;
                            let text = self.text(&uri)?;
                            // Root operations only mean something in a layer.
                            let in_layers = self.snapshot.as_ref().is_some_and(|s| {
                                convert::uri_to_path(&uri)
                                    .and_then(|path| s.file_of(&path))
                                    .is_some_and(|file| s.in_layers(file))
                            });
                            let operations = if in_layers {
                                let mut sources = crol_eval::Sources::new();
                                let file = sources.add(uri.as_str(), text.as_str());
                                crol_eval::operations(&sources, file)
                                    .into_iter()
                                    .map(|(span, kind)| (span.range, kind))
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            Some(lsp_types::SemanticTokensResult::Tokens(tokens::tokens(
                                &text,
                                &operations,
                                encoding,
                            )))
                        });
                self.respond(id, result);
            }
            request::CodeActionRequest::METHOD => {
                let result = serde_json::from_value::<lsp_types::CodeActionParams>(request.params)
                    .ok()
                    .and_then(|p| {
                        let text = self.text(&p.text_document.uri)?;
                        Some(actions::code_actions(
                            self.snapshot.as_ref(),
                            &p.text_document.uri,
                            &text,
                            &p,
                            &self.settings,
                            encoding,
                        ))
                    });
                self.respond(id, result);
            }
            request::InlayHintRequest::METHOD => {
                let result = serde_json::from_value::<lsp_types::InlayHintParams>(request.params)
                    .ok()
                    .and_then(|p| {
                        let uri = p.text_document.uri;
                        let text = self.text(&uri)?;
                        let path = convert::uri_to_path(&uri)?;
                        Some(hints::inlay_hints(
                            self.snapshot.as_ref()?,
                            &path,
                            &text,
                            p.range,
                            &self.settings,
                            encoding,
                        ))
                    });
                self.respond(id, result);
            }
            request::WorkspaceSymbolRequest::METHOD => {
                let result =
                    serde_json::from_value::<lsp_types::WorkspaceSymbolParams>(request.params)
                        .ok()
                        .zip(self.snapshot.as_ref())
                        .map(|(p, snapshot)| {
                            lsp_types::WorkspaceSymbolResponse::Flat(navigate::workspace_symbols(
                                snapshot, &p.query, encoding,
                            ))
                        });
                self.respond(id, result);
            }
            _ => self.send(Message::Response(Response::new_err(
                id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("unknown request {}", request.method),
            ))),
        }
    }

    /// The text of a document: its open buffer, or the file.
    fn text(&self, uri: &Uri) -> Option<String> {
        let path = convert::uri_to_path(uri)?;
        self.open
            .get(&path)
            .cloned()
            .or_else(|| std::fs::read_to_string(&path).ok())
    }
}
