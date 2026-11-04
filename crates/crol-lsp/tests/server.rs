//! The language server over an in-memory connection (`SPEC.md` 14.2).

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};

struct Client {
    connection: Connection,
    server: Option<thread::JoinHandle<()>>,
    next_id: i32,
}

/// The URI the server gives a path: `file:///C:/x` for `\\?\C:\x` on
/// Windows.
fn uri(path: &Path) -> String {
    let path = path.canonicalize().unwrap();
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .replace('\\', "/");
    let slash = if text.starts_with('/') { "" } else { "/" };
    format!("file://{slash}{text}")
}

impl Client {
    fn start(root: &Path) -> Client {
        Client::start_with(root, json!({ "debounce_ms": 10 }))
    }

    fn start_with(root: &Path, options: Value) -> Client {
        let (server_side, client_side) = Connection::memory();
        let server = thread::spawn(move || {
            let server = crol_lsp::Server::start(server_side, None).unwrap();
            server.run().unwrap();
        });
        let mut client = Client {
            connection: client_side,
            server: Some(server),
            next_id: 0,
        };
        client.request(
            "initialize",
            json!({
                "capabilities": { "general": { "positionEncodings": ["utf-16"] } },
                "workspaceFolders": [{ "uri": uri(root), "name": "test" }],
                "initializationOptions": options,
            }),
        );
        client.notify("initialized", json!({}));
        client
    }

    fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(Message::Notification(Notification::new(
                method.to_owned(),
                params,
            )))
            .unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.connection
            .sender
            .send(Message::Request(Request::new(
                id.clone(),
                method.to_owned(),
                params,
            )))
            .unwrap();
        loop {
            match self
                .connection
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Message::Response(response) if response.id == id => {
                    return response.response_result.unwrap_or(Value::Null);
                }
                _ => {}
            }
        }
    }

    /// The next diagnostics published for `uri` that satisfy `wanted`.
    fn diagnostics_for(&self, uri: &str, wanted: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        loop {
            let message = self
                .connection
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            if let Message::Notification(n) = message
                && n.method == "textDocument/publishDiagnostics"
                && n.params["uri"] == uri
            {
                let diagnostics = n.params["diagnostics"].as_array().unwrap().clone();
                if wanted(&diagnostics) {
                    return diagnostics;
                }
            }
        }
    }

    fn stop(mut self) {
        self.request("shutdown", json!(null));
        self.notify("exit", json!(null));
        self.server.take().unwrap().join().unwrap();
    }
}

fn workspace(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn diagnostics_symbols_and_formatting_for_a_document_on_its_own() {
    let dir = workspace("lsp-single", &[("config.crol", "")]);
    let mut client = Client::start(&dir);
    let file = uri(&dir.join("config.crol"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": "web: { port: 1 / 0 }" } }),
    );
    let diagnostics = client.diagnostics_for(&file, |d| {
        d.iter()
            .any(|d| d["message"].as_str().unwrap().contains("division"))
    });
    assert_eq!(
        diagnostics[0]["range"]["start"],
        json!({ "line": 0, "character": 13 })
    );

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": file, "version": 2 },
            "contentChanges": [{ "text": "+web:Service{port:80,limits:{cpu:1}},\nweb.port:443" }],
        }),
    );
    client.diagnostics_for(&file, |d| d.is_empty());

    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": file } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["+web", "web.port"]);
    assert_eq!(symbols[0]["detail"], "Service");
    assert_eq!(symbols[0]["children"][1]["name"], "limits");
    assert_eq!(symbols[0]["children"][1]["children"][0]["name"], "cpu");

    let edits = client.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": file }, "options": { "tabSize": 4, "insertSpaces": true } }),
    );
    assert_eq!(
        edits[0]["newText"],
        "+web: Service { port: 80, limits: { cpu: 1 } }\nweb.port: 443\n"
    );
    client.stop();
}

#[test]
fn hover_completion_and_definition_in_a_project() {
    let dir = workspace(
        "lsp-project",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]), .. }"#,
            ),
            (
                "base/a.crod",
                "t := { port: 80 },\n+web: Service { ..&t, url: \"x:\" + &self.port, .. },\n+db: { host: \"db\" },",
            ),
            (
                "local/a.crod",
                "web.port: 8080,\nmirror: &root.db.host,\nlink: @web,",
            ),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crod"));
    let text = std::fs::read_to_string(dir.join("local/a.crod")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": text } }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());

    // On a target with several operations: its history.
    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": local }, "position": { "line": 0, "character": 5 } }),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.starts_with("- `+web` (definition"), "{markdown}");
    assert!(markdown.contains("`web.port` (patch"), "{markdown}");

    // On a reference: the value it names.
    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": local }, "position": { "line": 1, "character": 16 } }),
    );
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .starts_with("**db.host**: `\"db\"`")
    );

    // Completion after `&root.`.
    let items = client.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": local }, "position": { "line": 1, "character": 14 } }),
    );
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert!(
        labels.contains(&"web") && labels.contains(&"db"),
        "{labels:?}"
    );

    // Go to the definition of an id.
    let location = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": local }, "position": { "line": 2, "character": 7 } }),
    );
    assert_eq!(location["uri"], uri(&dir.join("base/a.crod")));
    assert_eq!(
        location["range"]["start"],
        json!({ "line": 1, "character": 1 })
    );
    let _ = lsp_types::Uri::from_str(location["uri"].as_str().unwrap()).unwrap();
    client.stop();
}

#[test]
fn references_rename_and_workspace_symbols() {
    let dir = workspace(
        "lsp-navigate",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]), .. }"#,
            ),
            (
                "base/a.crod",
                "t := 80,\n+web: Service { p = port: &t, url: \"x:\" + &p, admin: { u: &p }, .. },",
            ),
            (
                "local/a.crod",
                "web.port: 8080,\nother: &t,\nlink: @web,\nm: &root.web.url,",
            ),
        ],
    );
    let mut client = Client::start(&dir);
    let base = uri(&dir.join("base/a.crod"));
    let local = uri(&dir.join("local/a.crod"));
    let text = std::fs::read_to_string(dir.join("local/a.crod")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": text } }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());
    let at = |line: u32, character: u32| (line, character);
    let start = |l: &Value| {
        (
            l["uri"].as_str().unwrap().to_owned(),
            at(
                l["range"]["start"]["line"].as_u64().unwrap() as u32,
                l["range"]["start"]["character"].as_u64().unwrap() as u32,
            ),
        )
    };

    // A root key: its operations in every layer, `&root.web` and `@web`.
    let found = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": local }, "position": { "line": 2, "character": 7 },
            "context": { "includeDeclaration": true },
        }),
    );
    let mut found: Vec<_> = found.as_array().unwrap().iter().map(start).collect();
    found.sort();
    let mut expected = vec![
        (base.clone(), at(1, 1)),
        (local.clone(), at(0, 0)),
        (local.clone(), at(2, 7)),
        (local.clone(), at(3, 9)),
    ];
    expected.sort();
    assert_eq!(found, expected);

    // A bind in a block: its uses there, without the declaration.
    let found = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": base }, "position": { "line": 1, "character": 16 },
            "context": { "includeDeclaration": false },
        }),
    );
    let found: Vec<_> = found.as_array().unwrap().iter().map(start).collect();
    assert_eq!(
        found,
        [(base.clone(), at(1, 43)), (base.clone(), at(1, 59))]
    );

    // On a bind's use: its declaration, and the value it names.
    let location = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": base }, "position": { "line": 1, "character": 59 } }),
    );
    assert_eq!(start(&location), (base.clone(), at(1, 16)));
    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": base }, "position": { "line": 1, "character": 59 } }),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.starts_with("**web.port**: `8080`"), "{markdown}");

    // Renaming a template renames it in every file.
    let range = client.request(
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": local }, "position": { "line": 1, "character": 8 } }),
    );
    assert_eq!(range["start"], json!({ "line": 1, "character": 8 }));
    let edit = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": local }, "position": { "line": 1, "character": 8 },
            "newName": "port",
        }),
    );
    let changes = edit["changes"].as_object().unwrap();
    assert_eq!(changes[&base].as_array().unwrap().len(), 2, "{edit}");
    assert_eq!(changes[&local].as_array().unwrap().len(), 1, "{edit}");
    assert_eq!(changes[&local][0]["newText"], "port");

    // Not a bind, and not a name a bind can have.
    let range = client.request(
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": local }, "position": { "line": 0, "character": 1 } }),
    );
    assert_eq!(range, Value::Null);
    let edit = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": base }, "position": { "line": 1, "character": 16 },
            "newName": "self",
        }),
    );
    assert_eq!(edit, Value::Null);

    // Workspace symbols: the definition of each root key.
    let symbols = client.request("workspace/symbol", json!({ "query": "WE" }));
    let symbols = symbols.as_array().unwrap();
    assert_eq!(symbols.len(), 1, "{symbols:?}");
    assert_eq!(symbols[0]["name"], "web");
    assert_eq!(start(&symbols[0]["location"]), (base.clone(), at(1, 1)));

    // Semantic tokens: (line, character, length, type, modifiers), with the
    // legend's types (property, type, enumMember, variable, keyword) and
    // modifier bits (declaration, definition, modification, early, late).
    let decode = |result: Value| {
        let data: Vec<u64> = result["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_u64().unwrap())
            .collect();
        let (mut line, mut character) = (0, 0);
        data.chunks(5)
            .map(|t| {
                if t[0] > 0 {
                    character = 0;
                }
                line += t[0];
                character += t[1];
                (line, character, t[2], t[3], t[4])
            })
            .collect::<Vec<_>>()
    };
    let tokens = decode(client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": base } }),
    ));
    assert_eq!(
        &tokens[..6],
        [
            (0, 0, 1, 3, 1),   // `t :=`
            (1, 1, 3, 0, 2),   // `+web`, a definition
            (1, 6, 7, 1, 0),   // `Service`
            (1, 16, 1, 3, 1),  // `p =`
            (1, 20, 4, 0, 0),  // `port`
            (1, 26, 1, 3, 16), // `&`
        ]
    );
    let tokens = decode(client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": local } }),
    ));
    assert_eq!(tokens[0], (0, 0, 3, 0, 4)); // `web.port`, a patch
    assert!(tokens.contains(&(3, 3, 1, 4, 16)), "{tokens:?}"); // `&` of `&root`
    client.stop();
}

#[test]
fn inlay_hints_show_what_entries_compute() {
    let dir = workspace(
        "lsp-hints",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]), .. }"#,
            ),
            (
                "base/a.crod",
                "+web: { replicas: 2, port: 8000 + 80, max: &self.replicas * 4, name: \"web\" },",
            ),
            ("local/a.crod", "web.replicas: & * 3,"),
        ],
    );
    let hints_in = |client: &mut Client, uri: &str| {
        let hints = client.request(
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": uri },
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 9, "character": 0 } },
            }),
        );
        hints
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["position"]["character"].as_u64().unwrap(),
                    h["label"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let base = uri(&dir.join("base/a.crod"));
    let local = uri(&dir.join("local/a.crod"));

    // Off by default.
    let mut client = Client::start(&dir);
    assert_eq!(hints_in(&mut client, &base), []);
    client.stop();

    let mut client = Client::start_with(&dir, json!({ "debounce_ms": 10, "inlay_hints": true }));
    // The value after each operation; late references read the final
    // value, so `max` sees the patched `replicas`.
    assert_eq!(
        hints_in(&mut client, &base),
        [(36, "= 8080".to_owned()), (61, "= 24".to_owned())]
    );
    assert_eq!(hints_in(&mut client, &local), [(19, "= 6".to_owned())]);
    client.stop();
}

/// Applies LSP text edits to an ASCII text.
fn apply(text: &str, edits: &[Value]) -> String {
    let offset = |position: &Value| {
        let line = position["line"].as_u64().unwrap() as usize;
        let character = position["character"].as_u64().unwrap() as usize;
        text.split_inclusive('\n')
            .take(line)
            .map(str::len)
            .sum::<usize>()
            + character
    };
    let mut edits: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                offset(&e["range"]["start"]),
                offset(&e["range"]["end"]),
                e["newText"].as_str().unwrap(),
            )
        })
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_owned();
    for (start, end, new) in edits {
        out.replace_range(start..end, new);
    }
    out
}

#[test]
fn code_actions_fix_what_the_diagnostics_say() {
    let dir = workspace(
        "lsp-actions",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]), schema: Some("schema"), .. }"#,
            ),
            (
                "schema.crol",
                r#"Schema {
                    format: 1,
                    root: "app::Services",
                    types: {
                        "app::Services": Transparent {
                            name: "Services",
                            source: ("src/lib.rs", 2),
                            inner: Map(String, Named("app::Service")),
                            ..
                        },
                        "app::Service": Struct {
                            name: "Service",
                            source: ("src/lib.rs", 1),
                            fields: {
                                replicas: { ty: U32, .. },
                                port: { ty: U16, .. },
                                limits: { ty: Named("app::Limits"), .. },
                            },
                            ..
                        },
                        "app::Limits": Struct {
                            name: "Limits",
                            source: ("src/lib.rs", 9),
                            fields: { cpu: { ty: F64, .. }, memory_mb: { ty: U32, .. } },
                            ..
                        },
                    },
                }"#,
            ),
            (
                "base/a.crod",
                "+web: Service { replicas: 2, port: 80, limits: Limits { cpu: 1.0, memory_mb: 512 } },",
            ),
            ("local/a.crod", ""),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crod"));
    let mut version = 1;
    let mut open = |client: &mut Client, text: &str, errors: bool| {
        if version == 1 {
            client.notify(
                "textDocument/didOpen",
                json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": text } }),
            );
        } else {
            client.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": local, "version": version },
                    "contentChanges": [{ "text": text }],
                }),
            );
        }
        version += 1;
        client.diagnostics_for(&local, |d| d.is_empty() != errors)
    };
    // Each action's title and the text after it.
    let actions = |client: &mut Client, text: &str, diagnostic: &Value| {
        let found = client.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": local },
                "range": diagnostic["range"],
                "context": { "diagnostics": [diagnostic], "triggerKind": 2 },
            }),
        );
        found
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                let edits = a["edit"]["changes"][&local].as_array().unwrap();
                (a["title"].as_str().unwrap().to_owned(), apply(text, edits))
            })
            .collect::<Vec<_>>()
    };
    let message = |d: &[Value], part: &str| {
        d.iter()
            .find(|d| d["message"].as_str().unwrap().contains(part))
            .unwrap_or_else(|| panic!("no diagnostic with `{part}` in {d:?}"))
            .clone()
    };

    // Missing fields, from the schema, and a misspelt field.
    let text = "+api: Service { replicas: 1 },\n+db: Service { replicas: 1, prot: 2, limits: Limits { cpu: 1.0, memory_mb: 1 } },";
    let diagnostics = open(&mut client, text, true);
    assert_eq!(
        actions(&mut client, text, &message(&diagnostics, "is missing")),
        [
            (
                "Add the missing fields".to_owned(),
                text.replace(
                    "replicas: 1 }",
                    "replicas: 1, port: 0, limits: Limits { .. } }"
                )
            ),
            (
                "End the block with `..`".to_owned(),
                text.replace("replicas: 1 }", "replicas: 1, .. }")
            ),
        ]
    );
    assert_eq!(
        actions(&mut client, text, &message(&diagnostics, "no field `prot`")),
        [(
            "Change to `port`".to_owned(),
            text.replace("prot:", "port:")
        )]
    );

    // A bind that does not exist.
    let text = "x: { y: &replicas },";
    let diagnostics = open(&mut client, text, true);
    assert_eq!(
        actions(&mut client, text, &message(&diagnostics, "no bind")),
        [(
            "Change to `&self.replicas`".to_owned(),
            "x: { y: &self.replicas },".to_owned()
        )]
    );

    // Declaring a key that exists.
    let text = "+web: Service { replicas: 3, .. },";
    let diagnostics = open(&mut client, text, true);
    assert_eq!(
        actions(&mut client, text, &message(&diagnostics, "already exists")),
        [(
            "Patch `web` instead of declaring it".to_owned(),
            "web: Service { replicas: 3, .. },".to_owned()
        )]
    );

    // The values below, as comments, when asked for.
    let text = "web.limits: Limits { ..&, cpu: 2.0 },";
    open(&mut client, text, false);
    let found = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": local },
            "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 2 } },
            "context": { "diagnostics": [], "triggerKind": 1 },
        }),
    );
    let action = &found.as_array().unwrap()[0];
    assert_eq!(action["title"], "Write the values below as comments");
    assert_eq!(
        apply(text, action["edit"]["changes"][&local].as_array().unwrap()),
        "web.limits: Limits {\n    // memory_mb: 512\n    ..&, cpu: 2.0 },"
    );
    client.stop();
}

#[test]
fn completion_and_hover_from_the_schema() {
    let dir = workspace(
        "lsp-schema",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]), schema: Some("schema"), .. }"#,
            ),
            (
                "schema.crol",
                r#"Schema {
                    format: 1,
                    root: "app::Definition",
                    types: {
                        "app::Definition": Enum {
                            name: "Definition",
                            source: ("src/lib.rs", 1),
                            variants: {
                                Service: { shape: Newtype(Named("app::Service")), .. },
                                Volume: { shape: Newtype(Named("app::Volume")), .. },
                            },
                            ..
                        },
                        "app::Service": Struct {
                            name: "Service",
                            source: ("src/lib.rs", 5),
                            fields: {
                                replicas: { ty: U32, doc: "Number of copies to run.", .. },
                                port: { ty: U16, .. },
                                limits: { ty: Named("app::Limits"), .. },
                                mode: { ty: Named("app::Mode"), .. },
                                url: { ty: Option(String), .. },
                                depends_on: { ty: List(Id(["app::Service"])), .. },
                            },
                            ..
                        },
                        "app::Limits": Struct {
                            name: "Limits",
                            source: ("src/lib.rs", 9),
                            fields: { cpu: { ty: F64, .. }, memory_mb: { ty: U32, .. } },
                            ..
                        },
                        "app::Mode": Enum {
                            name: "Mode",
                            source: ("src/lib.rs", 12),
                            variants: {
                                Fast: { shape: Unit, .. },
                                Slow: { shape: Newtype(U32), doc: "Slowly.", .. },
                            },
                            ..
                        },
                        "app::Volume": Struct {
                            name: "Volume",
                            source: ("src/lib.rs", 15),
                            fields: { size_gb: { ty: U32, .. } },
                            ..
                        },
                    },
                }"#,
            ),
            (
                "base/a.crod",
                "+web: Service { replicas: 2, port: 80, limits: Limits { cpu: 1.0, memory_mb: 512 }, mode: Fast, url: None, depends_on: [] },\n+data: Volume { size_gb: 10 },",
            ),
            ("local/a.crod", "web.port: 81,"),
        ],
    );
    let mut client = Client::start(&dir);
    let base = uri(&dir.join("base/a.crod"));
    let local = uri(&dir.join("local/a.crod"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": "web.port: 81," } }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());
    let mut version = 1;
    // The completions at the end of `text`: (label, insert text, detail).
    let mut complete = |client: &mut Client, text: &str| {
        version += 1;
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": local, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
        let items = client.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": local },
                "position": { "line": 0, "character": text.len() },
            }),
        );
        items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["label"].as_str().unwrap().to_owned(),
                    i["insertText"].as_str().unwrap_or("").to_owned(),
                    i["detail"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let has = |items: &[(String, String, String)], label: &str, insert: &str| {
        items.iter().any(|(l, i, _)| l == label && i == insert)
    };

    // Fields not written yet, filled with the value they have now.
    let items = complete(&mut client, "web: Service { ..&, rep");
    assert!(has(&items, "replicas", "replicas: 2"), "{items:?}");
    assert!(has(&items, "port", "port: 81"), "{items:?}");
    assert!(
        items.iter().any(|(l, _, d)| l == "replicas" && d == "u32"),
        "{items:?}"
    );

    // Values of the field's type.
    let items = complete(&mut client, "web.mode: S");
    assert!(
        has(&items, "Fast", "") && has(&items, "Slow", "Slow(0)"),
        "{items:?}"
    );
    let items = complete(&mut client, "web.url: N");
    assert!(
        has(&items, "None", "") && has(&items, "Some", "Some(\"\")"),
        "{items:?}"
    );

    // Ids, of the types the field may refer to.
    let items = complete(&mut client, "web.depends_on: [@");
    let labels: Vec<&str> = items.iter().map(|(l, _, _)| l.as_str()).collect();
    assert_eq!(labels, ["web"]);

    // The keys of `&self`, while the text does not parse.
    let items = complete(&mut client, "web.limits: Limits { ..&, cpu: &self.");
    let labels: Vec<&str> = items.iter().map(|(l, _, _)| l.as_str()).collect();
    assert_eq!(labels, ["cpu", "memory_mb"]);

    // Hover: the type and doc.
    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": base }, "position": { "line": 0, "character": 18 } }),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert_eq!(markdown, "Number of copies to run.\n\n`u32`");
    client.stop();
}

#[test]
fn documents_on_their_own_are_checked_against_their_schema() {
    let dir = workspace(
        "lsp-document-schema",
        &[
            (
                "schema.crol",
                r#"Schema {
                    format: 1,
                    root: "app::Service",
                    types: {
                        "app::Service": Struct {
                            name: "Service",
                            source: ("src/lib.rs", 1),
                            fields: { replicas: { ty: U32, .. } },
                            ..
                        },
                    },
                }"#,
            ),
            ("config.crol", ""),
        ],
    );
    let mut client = Client::start(&dir);
    let config = uri(&dir.join("config.crol"));
    let open = |client: &mut Client, version: i32, text: &str| {
        if version == 1 {
            client.notify(
                "textDocument/didOpen",
                json!({ "textDocument": { "uri": config, "languageId": "crol", "version": 1, "text": text } }),
            );
        } else {
            client.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": config, "version": version },
                    "contentChanges": [{ "text": text }],
                }),
            );
        }
        client.diagnostics_for(&config, |d| !d.is_empty())
    };
    let diagnostics = open(
        &mut client,
        1,
        "#![schema(::schema)]\n\nService { replicas: \"two\" }",
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("found \"two\""),
        "{diagnostics:?}"
    );
    let diagnostics = open(&mut client, 2, "#![schema(::nothing)]\n\n{ replicas: 2 }");
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("schema: "),
        "{diagnostics:?}"
    );
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);
    client.stop();
}

#[test]
fn diagnostics_follow_a_types_explicit_defaults() {
    let dir = workspace(
        "lsp-explicit-defaults",
        &[
            (
                "schema.crol",
                r#"Schema {
                    format: 1,
                    root: "app::Relaxed",
                    types: {
                        "app::Relaxed": Struct {
                            name: "Relaxed",
                            source: ("src/lib.rs", 1),
                            fields: { name: { ty: String, .. }, port: { ty: U16, default: Unrecorded, .. } },
                            explicit_defaults: Some(false),
                            ..
                        },
                    },
                }"#,
            ),
            ("config.crol", ""),
        ],
    );
    let client = Client::start(&dir);
    let config = uri(&dir.join("config.crol"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": config, "languageId": "crol", "version": 1, "text": "#![schema(::schema)]\n\nRelaxed { port: 1 }" } }),
    );
    // A field without a default must still be written.
    let diagnostics = client.diagnostics_for(&config, |d| !d.is_empty());
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["message"], "missing field `name`");
    // The one that has a default may be left out, without `..`.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": config, "version": 2 },
            "contentChanges": [{ "text": "#![schema(::schema)]\n\nRelaxed { name: \"a\" }" }],
        }),
    );
    client.diagnostics_for(&config, <[Value]>::is_empty);
    client.stop();
}

#[test]
fn the_project_file_is_checked_against_its_schema() {
    let project = r#"Project { layers: List([{ name: "base", dir: "base", .. }]), .. }"#;
    let dir = workspace(
        "lsp-project-file",
        &[("project.crol", project), ("base/a.crod", "+web: 1,")],
    );
    let client = Client::start(&dir);
    let file = uri(&dir.join("project.crol"));
    let text = r#"Project { layers: List([{ name: "base", dir: "base", .. }]), options: { max_contexts: -1, .. }, .. }"#;
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": text } }),
    );
    let diagnostics = client.diagnostics_for(&file, |d| !d.is_empty());
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("found -1"),
        "{diagnostics:?}"
    );
    client.stop();
}

#[test]
fn remote_schemas_are_fetched_without_blocking() {
    let dir = workspace("lsp-remote-schema", &[("config.crol", "")]);
    let config = uri(&dir.join("config.crol"));
    // Nothing listens there, so fetching fails at once.
    let text = "#![schema(\"https://127.0.0.1:9/schema.crol\")]\n\n{ replicas: 2 }";
    let open = |client: &Client| {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": config, "languageId": "crol", "version": 1, "text": text } }),
        );
    };
    let message = |diagnostics: &[Value]| {
        diagnostics
            .first()
            .and_then(|d| d["message"].as_str())
            .unwrap_or_default()
            .to_owned()
    };

    let client = Client::start_with(&dir, json!({ "remote_schemas": false }));
    open(&client);
    let diagnostics = client.diagnostics_for(&config, |d| !d.is_empty());
    assert!(
        message(&diagnostics).ends_with("is not cached, and fetching schemas is off"),
        "{diagnostics:?}"
    );
    client.stop();

    let client = Client::start(&dir);
    open(&client);
    let diagnostics = client.diagnostics_for(&config, |d| !d.is_empty());
    assert_eq!(
        message(&diagnostics),
        "schema: fetching `https://127.0.0.1:9/schema.crol`"
    );
    let diagnostics =
        client.diagnostics_for(&config, |d| message(d).starts_with("schema: cannot fetch"));
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);
    client.stop();
}

#[test]
fn go_to_value_below() {
    let dir = workspace(
        "lsp-value-below",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]), .. }"#,
            ),
            (
                "base/a.crod",
                "+web: { replicas: 2, limits: { cpu: 1 } },\nweb.limits: & ~ { memory: 2 },",
            ),
            (
                "local/a.crod",
                "web.replicas: & + 1,\nweb: & ~ { limits: { cpu: & * 2 } },",
            ),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crod"));
    let text = std::fs::read_to_string(dir.join("local/a.crod")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": text } }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());
    let base = uri(&dir.join("base/a.crod"));
    let mut definition = |line: u32, character: u32| {
        let location = client.request(
            "textDocument/definition",
            json!({ "textDocument": { "uri": local }, "position": { "line": line, "character": character } }),
        );
        (location["uri"].clone(), location["range"]["start"].clone())
    };
    // `& + 1` adds to what `+web` defined.
    assert_eq!(
        definition(0, 14),
        (json!(base), json!({ "line": 0, "character": 1 }))
    );
    // `&` in `cpu`: web.limits.cpu was last changed by `+web`, not by the
    // patch that added `memory`.
    assert_eq!(
        definition(1, 26),
        (json!(base), json!({ "line": 0, "character": 1 }))
    );
    client.stop();
}

#[test]
fn layer_documents_that_name_another_schema_get_a_warning() {
    let schema = r#"Schema { format: 1, root: "app::Service", types: {} }"#;
    let dir = workspace(
        "lsp-other-schema",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }]), schema: Some("schema"), .. }"#,
            ),
            ("schema.crol", schema),
            ("other.crol", schema),
            ("base/a.crod", "#![schema(super::other)]\n+a: 1,"),
        ],
    );
    let client = Client::start(&dir);
    let a = uri(&dir.join("base/a.crod"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": a, "languageId": "crol", "version": 1, "text": "#![schema(super::other)]\n+a: 1," } }),
    );
    let diagnostics = client.diagnostics_for(&a, |d| !d.is_empty());
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], 2);
    assert_eq!(
        diagnostics[0]["message"],
        "this document names another schema than the project's, which is used instead"
    );
    client.stop();
}

#[test]
fn documents_on_their_own_complete_from_their_schema() {
    let dir = workspace(
        "lsp-own-schema",
        &[
            (
                "schema.crol",
                r#"Schema {
                    format: 1,
                    root: "app::Config",
                    types: {
                        "app::Config": Struct {
                            name: "Config",
                            source: ("src/lib.rs", 1),
                            fields: {
                                listen: { ty: String, doc: "Where to listen.", .. },
                                limits: { ty: Named("app::Limits"), .. },
                            },
                            ..
                        },
                        "app::Limits": Struct {
                            name: "Limits",
                            source: ("src/lib.rs", 5),
                            fields: { cpu: { ty: F64, .. } },
                            ..
                        },
                    },
                }"#,
            ),
            ("config.crol", ""),
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }]), .. }"#,
            ),
            ("base/a.crod", "+a: 1,"),
        ],
    );
    let mut client = Client::start(&dir);
    let complete = |client: &mut Client, uri: &str, first: &str, text: &str| {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "crol", "version": 1, "text": first } }),
        );
        client.diagnostics_for(uri, |d| d.is_empty());
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": text }],
            }),
        );
        let line = text.lines().count() - 1;
        let character = text.lines().last().unwrap().len();
        let items = client.request(
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        );
        items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    // The fields of the root type, and inside them.
    let config = uri(&dir.join("config.crol"));
    let start = "#![schema(schema)]\n\n{ listen: \"x\", limits: { cpu: 1.0 } }";
    let labels = complete(&mut client, &config, start, "#![schema(schema)]\n\n{ lim");
    assert_eq!(labels, ["listen", "limits"]);
    let labels = complete(
        &mut client,
        &config,
        start,
        "#![schema(schema)]\n\n{ limits: { c",
    );
    assert_eq!(labels, ["cpu"]);
    // Written as entries, without braces.
    let entries = "#![schema(schema)]\n\nlisten: \"x\",\nlimits: { cpu: 1.0 },";
    let labels = complete(
        &mut client,
        &config,
        entries,
        "#![schema(schema)]\n\nlisten: \"x\",\nl",
    );
    assert_eq!(labels, ["limits"]);
    // The same with a line break as the only separator.
    let labels = complete(
        &mut client,
        &config,
        entries,
        "#![schema(schema)]\n\nlisten: \"x\"\nl",
    );
    assert_eq!(labels, ["limits"]);
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": config, "version": 3 }, "contentChanges": [{ "text": entries }] }),
    );
    client.diagnostics_for(&config, |d| d.is_empty());
    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": config }, "position": { "line": 2, "character": 2 } }),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap_or_default();
    assert_eq!(markdown, "Where to listen.\n\n`String`", "{hover}");

    // The project file: the fields of `Project` crol provides.
    let project = uri(&dir.join("project.crol"));
    let text = r#"Project { layers: List([{ name: "base", dir: "base", .. }]), "#;
    let labels = complete(
        &mut client,
        &project,
        r#"Project { layers: List([{ name: "base", dir: "base", .. }]), .. }"#,
        text,
    );
    assert!(labels.iter().any(|l| l == "schema"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "layers"), "{labels:?}");
    client.stop();
}

// ---- Nested structures ----

/// Structs at least three levels deep, through lists, options, maps, enums
/// with struct variants, newtypes and tuples.
const NESTED_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Config",
    types: {
        "app::Definition": Enum {
            name: "Definition",
            source: ("src/lib.rs", 1),
            variants: { Config: { shape: Newtype(Named("app::Config")), .. } },
            ..
        },
        "app::Config": Struct {
            name: "Config",
            source: ("src/lib.rs", 2),
            fields: {
                name: { ty: String, .. },
                server: { ty: Named("app::Server"), .. },
                fallback: { ty: Option(Named("app::Server")), .. },
                services: { ty: List(Named("app::Service")), .. },
                by_name: { ty: Map(String, Named("app::Service")), .. },
                ports: { ty: Map(U16, Named("app::Service")), .. },
                backend: { ty: Named("app::Backend"), .. },
                backends: { ty: List(Named("app::Backend")), .. },
                modes: { ty: List(Named("app::Mode")), .. },
                fallback_mode: { ty: Option(Named("app::Mode")), .. },
                wrapped: { ty: Named("app::Wrapper"), .. },
                pair: { ty: Tuple([String, Named("app::Retry")]), .. },
                extra: { ty: Named("app::Retry"), flatten: true, .. },
            },
            ..
        },
        "app::Service": Struct {
            name: "Service",
            source: ("src/lib.rs", 3),
            fields: {
                name: { ty: String, .. },
                server: { ty: Named("app::Server"), .. },
                depends_on: { ty: List(Id(["app::Config"])), .. },
            },
            ..
        },
        "app::Server": Struct {
            name: "Server",
            source: ("src/lib.rs", 4),
            fields: {
                tls: { ty: Named("app::Tls"), doc: "How to encrypt.", .. },
                listeners: { ty: List(Named("app::Listener")), .. },
            },
            ..
        },
        "app::Tls": Struct {
            name: "Tls",
            source: ("src/lib.rs", 5),
            fields: {
                cert: { ty: Named("app::Cert"), .. },
                enabled: { ty: Bool, .. },
            },
            ..
        },
        "app::Cert": Struct {
            name: "Cert",
            source: ("src/lib.rs", 6),
            fields: {
                path: { ty: String, doc: "Where the certificate is.", .. },
                mode: { ty: Named("app::Mode"), .. },
            },
            ..
        },
        "app::Listener": Struct {
            name: "Listener",
            source: ("src/lib.rs", 7),
            fields: {
                routes: { ty: List(Named("app::Route")), .. },
                port: { ty: U16, .. },
            },
            ..
        },
        "app::Route": Struct {
            name: "Route",
            source: ("src/lib.rs", 8),
            fields: { prefix: { ty: String, .. }, target: { ty: String, .. } },
            ..
        },
        "app::Mode": Enum {
            name: "Mode",
            source: ("src/lib.rs", 9),
            variants: { Strict: { shape: Unit, .. }, Lax: { shape: Unit, .. } },
            ..
        },
        "app::Backend": Enum {
            name: "Backend",
            source: ("src/lib.rs", 10),
            variants: {
                Local: { shape: Unit, .. },
                Remote: {
                    shape: Struct({
                        host: { ty: String, doc: "The host.", .. },
                        retry: { ty: Named("app::Retry"), .. },
                    }),
                    ..
                },
                Wrapped: { shape: Newtype(Named("app::Retry")), .. },
                Pair: { shape: Tuple([String, Named("app::Retry")]), .. },
            },
            ..
        },
        "app::Retry": Struct {
            name: "Retry",
            source: ("src/lib.rs", 11),
            fields: {
                count: { ty: U32, .. },
                backoff: { ty: Named("app::Backoff"), .. },
            },
            ..
        },
        "app::Backoff": Struct {
            name: "Backoff",
            source: ("src/lib.rs", 12),
            fields: { seconds: { ty: U32, .. }, jitter: { ty: Bool, .. } },
            ..
        },
        "app::Wrapper": Newtype {
            name: "Wrapper",
            source: ("src/lib.rs", 13),
            inner: Named("app::Retry"),
            ..
        },
    },
}"#;

/// A document held by a client, as an editor holds it: every change is
/// sent as `didChange`, and the request follows at once, as Zed's does.
struct Buffer {
    uri: String,
    version: i32,
}

impl Buffer {
    /// Opens `path`, and waits for its first evaluation.
    fn open(client: &Client, dir: &Path, path: &str) -> Buffer {
        let uri = uri(&dir.join(path));
        let text = std::fs::read_to_string(dir.join(path)).unwrap();
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "crol", "version": 1, "text": text } }),
        );
        let diagnostics = client.diagnostics_for(&uri, |_| true);
        // A schema that does not read would make every case fail.
        assert!(
            !format!("{diagnostics:?}").contains("while deserializing"),
            "{diagnostics:?}"
        );
        Buffer { uri, version: 1 }
    }

    fn change(&mut self, client: &Client, text: &str) {
        self.version += 1;
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": self.uri, "version": self.version },
                "contentChanges": [{ "text": text }],
            }),
        );
    }

    /// The completions at the `|` in `marked`, without waiting for an
    /// evaluation of the text.
    fn complete(&mut self, client: &mut Client, marked: &str) -> Vec<Value> {
        let (text, position) = at_marker(marked);
        self.change(client, &text);
        let items = client.request(
            "textDocument/completion",
            json!({ "textDocument": { "uri": self.uri }, "position": position }),
        );
        items.as_array().cloned().unwrap_or_default()
    }

    /// The hover text at the `|` in `marked`, after evaluating the text.
    fn hover(&mut self, client: &mut Client, marked: &str) -> String {
        let (text, position) = at_marker(marked);
        self.change(client, &text);
        client.diagnostics_for(&self.uri, |_| true);
        let hover = client.request(
            "textDocument/hover",
            json!({ "textDocument": { "uri": self.uri }, "position": position }),
        );
        hover["contents"]["value"]
            .as_str()
            .unwrap_or("(no hover)")
            .to_owned()
    }
}

/// The text without the `|` in it, and the position of the `|` as LSP
/// counts it.
fn at_marker(marked: &str) -> (String, Value) {
    let at = marked.find('|').unwrap();
    let before = &marked[..at];
    let line = before.matches('\n').count();
    let column = before.rsplit('\n').next().unwrap().encode_utf16().count();
    (
        marked.replacen('|', "", 1),
        json!({ "line": line, "character": column }),
    )
}

fn labels(items: &[Value]) -> Vec<&str> {
    items.iter().map(|i| i["label"].as_str().unwrap()).collect()
}

/// A completion case: the text, the labels wanted, the labels not wanted.
type Case<'a> = (&'a str, &'a [&'a str], &'a [&'a str]);

/// Runs completion on each case, and reports every one that fails at once.
fn check_completions(client: &mut Client, buffer: &mut Buffer, prefix: &str, cases: &[Case<'_>]) {
    let mut failures = Vec::new();
    for (marked, wanted, unwanted) in cases {
        let items = buffer.complete(client, &format!("{prefix}{marked}"));
        let found = labels(&items);
        if !wanted.iter().all(|l| found.contains(l)) || unwanted.iter().any(|l| found.contains(l)) {
            failures.push(format!("{marked:?}: wanted {wanted:?}, got {found:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

fn nested_document(name: &str) -> (Client, Buffer) {
    let dir = workspace(
        name,
        &[
            ("schema.crol", NESTED_SCHEMA),
            ("config.crol", "#![schema(schema)]\n\n{ name: \"x\" }"),
        ],
    );
    let client = Client::start(&dir);
    let buffer = Buffer::open(&client, &dir, "config.crol");
    (client, buffer)
}

const HEADER: &str = "#![schema(schema)]\n\n";

#[test]
fn completion_of_fields_at_any_depth() {
    let (mut client, mut buffer) = nested_document("lsp-nested-fields");
    let tls: &[&str] = &["cert", "enabled"];
    let cert: &[&str] = &["path", "mode"];
    let service: &[&str] = &["name", "server", "depends_on"];
    let retry: &[&str] = &["count", "backoff"];
    let backoff: &[&str] = &["seconds", "jitter"];
    let cases: &[Case<'_>] = &[
        // Blocks in blocks.
        ("{ server: { tls: { cert: { | } } } }", cert, tls),
        (
            "{ server: Server { tls: Tls { cert: Cert { | } } } }",
            cert,
            tls,
        ),
        ("{ server: { tls: { cert: { pa| } } } }", cert, tls),
        (
            "{ server: { tls: Tls { cert: { mode: Lax, | } } } }",
            &["path"],
            &["mode"],
        ),
        // List elements.
        ("{ services: [{ | }] }", service, tls),
        ("{ services: [{ name: \"a\" }, { | }] }", service, tls),
        ("{ services: [{ server: { tls: { | } } }] }", tls, cert),
        (
            "{ services: [Service { server: Server { | } }] }",
            &["tls"],
            cert,
        ),
        (
            "{ server: { listeners: [{ port: 1 }, { routes: [{ | }] }] } }",
            &["prefix", "target"],
            &["port"],
        ),
        // Options.
        ("{ fallback: { tls: { | } } }", tls, cert),
        ("{ fallback: Some({ tls: { | } }) }", tls, cert),
        ("{ fallback: Some(Server { | }) }", &["tls"], cert),
        // Maps.
        ("{ by_name: { web: { server: { | } } } }", &["tls"], service),
        ("{ by_name: { \"a b\": { | } } }", service, tls),
        (
            "{ by_name: { [\"a b\"]: { server: { | } } } }",
            &["tls"],
            service,
        ),
        (
            "{ ports: { 80: { server: { tls: { | } } } } }",
            tls,
            service,
        ),
        // Enums with struct variants, and other payloads.
        ("{ backend: Remote { | } }", &["host", "retry"], retry),
        ("{ backend: Remote { retry: { | } } }", retry, &["host"]),
        (
            "{ backend: Remote { retry: { backoff: { | } } } }",
            backoff,
            retry,
        ),
        (
            "{ backends: [Local, Remote { retry: Retry { | } }] }",
            retry,
            &["host"],
        ),
        ("{ backend: Wrapped({ | }) }", retry, &["host"]),
        (
            "{ backend: Pair(\"a\", { backoff: { | } }) }",
            backoff,
            retry,
        ),
        ("{ wrapped: Wrapper({ backoff: { | } }) }", backoff, retry),
        ("{ pair: (\"a\", { backoff: { | } }) }", backoff, retry),
        // `~` and parentheses keep the place.
        ("{ server: { .. } ~ { tls: { | } } }", tls, cert),
        ("{ server: ({ tls: { | } }) }", tls, cert),
        // Variant blocks. What the block around sets, one may set again.
        ("{ server: { <prod> { tls: { | } } } }", tls, cert),
        ("{ server: { tls: { }, <prod> { | } } }", &["tls"], cert),
        (
            "{ services: [{ <prod> { server: { | } } }] }",
            &["tls"],
            service,
        ),
        (
            "{ server: { <prod> { <eu> { tls: { cert: { | } } } } } }",
            cert,
            tls,
        ),
        // Flattened fields.
        ("{ | }", &["name", "count", "backoff"], &["extra"]),
        ("{ backoff: { | } }", backoff, retry),
        // A type name at the root, and entries without braces.
        ("Config { server: { tls: { | } } }", tls, cert),
        ("server: { tls: { cert: { | } } }", cert, tls),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);
    client.stop();
}

#[test]
fn completion_after_dotted_paths() {
    let (mut client, mut buffer) = nested_document("lsp-nested-paths");
    let tls: &[&str] = &["cert", "enabled"];
    let cert: &[&str] = &["path", "mode"];
    let cases: &[Case<'_>] = &[
        ("{ server.| }", &["tls", "listeners"], &["server"]),
        ("{ server.tls.| }", tls, cert),
        ("{ server.tls.cert.| }", cert, tls),
        ("{ server.tls.cert.pa| }", cert, tls),
        ("{ server.tls.cert.|: \"x\" }", cert, tls),
        ("{ services[0].server.tls.| }", tls, cert),
        ("{ by_name[\"web\"].server.| }", &["tls"], cert),
        ("{ pair.1.backoff.| }", &["seconds", "jitter"], cert),
        ("{ server: { tls.cert.| } }", cert, tls),
        ("{ server: { <prod> { tls.cert.| } } }", cert, tls),
        ("server.tls.|", tls, cert),
        ("server.tls.|\nname: \"x\",\n", tls, cert),
        ("name: \"x\",\nserver.tls.cert.|", cert, tls),
        // The values there.
        ("{ server.tls.cert.mode: | }", &["Strict", "Lax"], cert),
        ("{ server.tls.enabled: | }", &["true", "false"], cert),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);
    client.stop();
}

#[test]
fn completion_of_values_at_any_depth() {
    let (mut client, mut buffer) = nested_document("lsp-nested-values");
    let mode: &[&str] = &["Strict", "Lax"];
    let none: &[&str] = &[];
    let cases: &[Case<'_>] = &[
        (
            "{ server: { tls: { cert: { mode: | } } } }",
            mode,
            &["path"],
        ),
        (
            "{ server: { tls: { cert: { mode: La| } } } }",
            mode,
            &["path"],
        ),
        (
            "{ server: { tls: { enabled: | } } }",
            &["true", "false"],
            none,
        ),
        ("{ server: { tls: | } }", &["Tls"], none),
        (
            "{ services: [{ server: { tls: { enabled: | } } }] }",
            &["true"],
            none,
        ),
        (
            "{ backend: Remote { retry: { backoff: { jitter: | } } } }",
            &["true"],
            none,
        ),
        // Elements and payloads, where a value goes and not a field.
        ("{ modes: [|] }", mode, &["name"]),
        ("{ modes: [Strict, |] }", mode, &["name"]),
        ("{ modes: [Strict, L|] }", mode, &["name"]),
        ("{ services: [|] }", &["Service"], &["name"]),
        ("{ services: [{ }, |] }", &["Service"], &["name"]),
        (
            "{ server: { listeners: [{ routes: [|] }] } }",
            &["Route"],
            &["tls"],
        ),
        ("{ fallback_mode: Some(|) }", mode, &["name"]),
        ("{ backends: [|] }", &["Local", "Remote"], &["name"]),
        // Nothing after a finished value.
        (
            "{ server: { tls: { enabled: true | } } }",
            none,
            &["true", "cert"],
        ),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);
    client.stop();
}

#[test]
fn completion_in_documents_that_do_not_parse() {
    let (mut client, mut buffer) = nested_document("lsp-nested-unfinished");
    let tls: &[&str] = &["cert", "enabled"];
    let cert: &[&str] = &["path", "mode"];
    let mode: &[&str] = &["Strict", "Lax"];
    let cases: &[Case<'_>] = &[
        // Nothing closed yet.
        ("{ server: { tls: { cert: { |", cert, tls),
        ("{ server: { tls: { cert: { pa|", cert, tls),
        ("{ services: [{ server: { tls: { |", tls, cert),
        (
            "{ services: [{ server: { tls: { cert: { mode: |",
            mode,
            cert,
        ),
        ("{ backend: Remote { retry: { |", &["count"], tls),
        ("{ fallback: Some({ |", &["tls"], cert),
        // A word on a line of its own, and entries after it.
        (
            "{\n  server: {\n    tls: {\n      cert: {\n        pa|\n      }\n    }\n  }\n}",
            cert,
            tls,
        ),
        (
            "{\n  server: {\n    tl|\n    listeners: [],\n  }\n}",
            &["tls"],
            cert,
        ),
        (
            "{\n  server: {\n    tls: {\n      c|\n      enabled: true,\n    },\n  },\n}",
            &["cert"],
            &["enabled"],
        ),
        // A value not written yet, and the next entry after it.
        (
            "{\n  server: {\n    tls: {\n      enabled: |\n      cert: { },\n    },\n  },\n}",
            &["true"],
            tls,
        ),
        (
            "{\n  server: {\n    tls: |\n    listeners: [],\n  },\n}",
            &["Tls"],
            cert,
        ),
        // Unfinished lines above the cursor.
        (
            "{\n  name:\n  server: {\n    tls: { | },\n  },\n}",
            tls,
            cert,
        ),
        ("{\n  nam\n  server: {\n    tls: { | },\n  },\n}", tls, cert),
        (
            "{\n  server: {\n    listeners\n    tls: {\n      cert: { | },\n    },\n  },\n}",
            cert,
            tls,
        ),
        ("{\n  name: 1 +\n  server: { tls: { | } },\n}", tls, cert),
        ("{\n  name: ,\n  server: { tls: { | } },\n}", tls, cert),
        (
            "{\n  services: [{ name: }],\n  server: { tls: { | } },\n}",
            tls,
            cert,
        ),
        // A dot with nothing after it.
        (
            "{\n  server: { tls. },\n  fallback: { tls: { | } },\n}",
            tls,
            cert,
        ),
        ("{\n  server.\n  fallback.tls.|\n}", tls, cert),
        // Entries on lines of their own, without commas.
        (
            "{\n  server: {\n    tls: {\n      enabled: true\n      |\n    }\n  }\n}",
            &["cert"],
            &["enabled"],
        ),
        (
            "{\n  server: {\n    tls: {\n      cert: { }\n      en|\n    }\n  }\n}",
            &["enabled"],
            &["cert"],
        ),
        (
            "{\n  name: \"x\"\n  server: {\n    tls: { | }\n  }\n}",
            tls,
            cert,
        ),
        // Brackets that do not match.
        (
            "{ services: [{ server: { tls: { } ], fallback: { tls: { | } } }",
            tls,
            cert,
        ),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);

    // After the evaluation of the broken text too.
    let marked = format!("{HEADER}{{ services: [{{ server: {{ tls: {{ |");
    let (text, position) = at_marker(&marked);
    buffer.change(&client, &text);
    client.diagnostics_for(&buffer.uri, |d| !d.is_empty());
    thread::sleep(Duration::from_millis(50));
    let items = client.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": buffer.uri }, "position": position }),
    );
    assert_eq!(
        labels(items.as_array().unwrap()),
        ["cert", "enabled"],
        "{items}"
    );
    client.stop();
}

#[test]
fn completion_from_the_project_at_any_depth() {
    let base = "\
+prod: Config {
    name: \"p\",
    server: {
        tls: { cert: { path: \"/etc/p\", mode: Strict }, enabled: true },
        listeners: [{ routes: [{ prefix: \"/\", target: \"a\" }], port: 80 }],
    },
    services: [{ name: \"first\", server: { tls: { enabled: true } } }],
    by_name: { web: { name: \"w\" } },
    backend: Remote { host: \"h\", retry: { count: 3, backoff: { seconds: 2, jitter: false } } },
},
";
    let dir = workspace(
        "lsp-nested-project",
        &[
            (
                "project.crol",
                r#"Project {
                    layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]),
                    variants: [Discrete("eu")],
                    schema: Some("schema"),
                    root: Some("app::Definition"),
                    ..
                }"#,
            ),
            ("schema.crol", NESTED_SCHEMA),
            ("base/a.crod", base),
            ("local/a.crod", "prod.name: \"q\",\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let mut local = Buffer::open(&client, &dir, "local/a.crod");
    let tls: &[&str] = &["cert", "enabled"];
    let cert: &[&str] = &["path", "mode"];
    let backoff: &[&str] = &["seconds", "jitter"];
    let cases: &[Case<'_>] = &[
        // Paths from a root key.
        ("prod.|", &["server", "services"], tls),
        ("prod.server.|", &["tls"], cert),
        ("prod.server.tls.|", tls, cert),
        ("prod.server.tls.cert.|", cert, tls),
        ("prod.server.tls.cert.pa|", cert, tls),
        ("prod.services[0].server.tls.|", tls, cert),
        ("prod.by_name[\"web\"].server.|", &["tls"], cert),
        ("prod.server.tls.cert.mode: |", &["Strict", "Lax"], cert),
        // Through an enum, by the variant the value has.
        ("prod.backend.|", &["host", "retry"], tls),
        ("prod.backend.retry.backoff.|", backoff, tls),
        ("prod.backend: { retry: { backoff: { | } } }", backoff, tls),
        // Patches as blocks.
        ("prod: { server: { tls: { | } } }", tls, cert),
        (
            "prod: Config { server: { tls: { cert: { | } } } }",
            cert,
            tls,
        ),
        ("prod: { services: [{ server: { | } }] }", &["tls"], cert),
        // Variant blocks at the root.
        ("<eu> { prod.server.tls.| }", tls, cert),
        ("<eu> { prod: { server: { tls: { | } } } }", tls, cert),
        ("prod<eu>: { server: { tls: { | } } }", tls, cert),
        ("prod.server<eu>.tls.|", tls, cert),
        // New definitions.
        (
            "+other: Config { services: [Service { server: { tls: { | } } }] }",
            tls,
            cert,
        ),
        // Earlier entries, finished or not.
        ("prod.name: \"a\",\nprod.server.tls.|", tls, cert),
        ("prod.name:\nprod.server.tls.|", tls, cert),
        // The keys of references.
        ("x: &root.prod.services[0].|", &["name", "server"], tls),
        (
            "x: &root.prod.server.listeners[0].routes[0].|",
            &["prefix", "target"],
            tls,
        ),
        ("x: &root.prod.server.tls.cert.|", cert, tls),
        // Ids, in a list in a list.
        ("prod.services[0].depends_on: [@|", &["prod"], tls),
        ("prod: { services: [{ depends_on: [@|] }] }", &["prod"], tls),
    ];
    check_completions(&mut client, &mut local, "", cases);

    // What a field has now is what accepting it writes.
    let insert = |items: &[Value], label: &str| {
        items
            .iter()
            .find(|i| i["label"] == label)
            .map(|i| i["insertText"].as_str().unwrap_or("").to_owned())
    };
    let items = local.complete(&mut client, "prod.server.tls.cert.|");
    assert_eq!(
        insert(&items, "path").as_deref(),
        Some("path: \"/etc/p\""),
        "{items:?}"
    );
    let items = local.complete(&mut client, "prod: { server: { tls: { cert: { | } } } }");
    assert_eq!(
        insert(&items, "mode").as_deref(),
        Some("mode: Strict"),
        "{items:?}"
    );
    let items = local.complete(&mut client, "prod: { services: [{ | }] }");
    assert_eq!(
        insert(&items, "name").as_deref(),
        Some("name: \"first\""),
        "{items:?}"
    );
    let items = local.complete(&mut client, "prod.backend.retry.backoff.|");
    assert_eq!(
        insert(&items, "seconds").as_deref(),
        Some("seconds: 2"),
        "{items:?}"
    );

    // `&self` and `&parent` in blocks in lists.
    local.change(&client, "prod.name: \"q\",\n");
    client.diagnostics_for(&local.uri, |d| d.is_empty());
    let mut base_buffer = Buffer::open(&client, &dir, "base/a.crod");
    let marked = base.replace("name: \"first\",", "name: \"first\", port: &self.|");
    let items = base_buffer.complete(&mut client, &marked);
    assert!(labels(&items).contains(&"server"), "{items:?}");
    let marked = base.replace("name: \"first\",", "name: \"first\", port: &parent.|");
    let items = base_buffer.complete(&mut client, &marked);
    assert!(labels(&items).contains(&"services"), "{items:?}");
    client.stop();
}

const SEALED_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Unit",
    types: {
        "app::Unit": Struct {
            name: "Unit",
            source: ("a.rs", 1),
            fields: {
                hp: { ty: I32, constraint: Some("positive"), .. },
                original: { ty: U32, doc: "The id.", sealed: true, .. },
            },
            ..
        },
    },
    constraints: { positive: { constraint: Min(0.0), sealed: true } },
}"#;

#[test]
fn sealed_fields_and_constraints_in_a_project() {
    let dir = workspace(
        "lsp-sealed",
        &[
            (
                "project.crol",
                r#"Project {
                    layers: List([{ name: "base", dir: "base", .. }, { name: "local", dir: "local", .. }]),
                    schema: Some("schema"),
                    root: Some("app::Map"),
                    ..
                }"#,
            ),
            (
                "schema.crol",
                &SEALED_SCHEMA.replace(
                    r#"root: "app::Unit",
    types: {"#,
                    r#"root: "app::Map",
    types: {
        "app::Map": Transparent {
            name: "",
            source: ("a.rs", 1),
            inner: Map(String, Named("app::Unit")),
        },"#,
                ),
            ),
            ("base/a.crod", "+x: Unit { hp: 1, original: 1 },\n"),
            ("local/a.crod", "x.hp: -1\nx.original: 2\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let local = Buffer::open(&client, &dir, "local/a.crod");
    client.diagnostics_for(&uri(&dir.join("local/a.crod")), |found| {
        says(found, "-1 is below 0")
    });

    // Completion above the defining layer leaves the sealed field out.
    let mut own = Buffer::open(&client, &dir, "base/a.crod");
    let mut local = local;
    check_completions(
        &mut client,
        &mut local,
        "",
        &[("x.|", &["hp"], &["original"])],
    );
    check_completions(
        &mut client,
        &mut own,
        "",
        &[("x.|", &["hp", "original"], &[])],
    );
}

#[test]
fn hover_at_any_depth() {
    let (mut client, mut buffer) = nested_document("lsp-nested-hover");
    let text = "\
{
    server: { tls: { cert: { path: \"x\", mode: Lax } } },
    services: [
        { name: \"a\" },
        { name: \"b\", server: { tls: { cert: { path: \"y\", mode: Strict } } } },
    ],
    fallback: Some({ tls: { cert: { path: \"z\" } } }),
    by_name: { web: { server: { tls: { enabled: true } } } },
    ports: { 80: { name: \"http\" } },
    backend: Remote { host: \"h\", retry: { count: 1 } },
    backends: [Local, Remote { host: \"i\" }],
    pair: (\"a\", { count: 2 }),
    wrapped: Wrapper({ count: 3 }),
}";
    let path = "Where the certificate is.\n\n`String`";
    let host = "The host.\n\n`String`";
    let cases = [
        ("path: \"x\"", path),
        ("name: \"b\"", "`String`"),
        ("path: \"y\"", path),
        ("mode: Strict", "`Mode`"),
        ("path: \"z\"", path),
        ("enabled: true", "`bool`"),
        ("name: \"http\"", "`String`"),
        ("host: \"h\"", host),
        ("count: 1", "`u32`"),
        ("host: \"i\"", host),
        ("count: 2", "`u32`"),
        ("count: 3", "`u32`"),
    ];
    let mut failures = Vec::new();
    for (word, wanted) in cases {
        let i = text.find(word).unwrap() + 1;
        let marked = format!("{HEADER}{}|{}", &text[..i], &text[i..]);
        let markdown = buffer.hover(&mut client, &marked);
        if markdown != wanted {
            failures.push(format!("wanted {wanted:?}, got {markdown:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    client.stop();
}

fn inlay_labels(client: &mut Client, buffer: &Buffer) -> Vec<String> {
    let hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": buffer.uri },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 40, "character": 0 } },
        }),
    );
    hints
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["label"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn inlay_hints_at_any_depth() {
    let text = "\
#![schema(schema)]

{
    server: { tls: { cert: { path: \"a\" + \"b\" } } },
    services: [
        { name: \"s\" + \"1\" },
        { name: \"s\" + \"2\" },
    ],
    fallback: Some({ listeners: [{ port: 40 + 2 }] }),
    by_name: { web: { name: \"w\" + \"1\" } },
    ports: { 80: { name: \"p\" + \"1\" } },
    backend: Remote { host: \"h\" + \"1\", retry: { count: 1 + 1 } },
    pair: (\"a\", { count: 2 + 2 }),
    wrapped: Wrapper({ count: 3 + 3 }),
}
";
    let dir = workspace(
        "lsp-nested-hints",
        &[("schema.crol", NESTED_SCHEMA), ("config.crol", text)],
    );
    let mut client = Client::start_with(&dir, json!({ "debounce_ms": 10, "inlay_hints": true }));
    let buffer = Buffer::open(&client, &dir, "config.crol");
    assert_eq!(
        inlay_labels(&mut client, &buffer),
        [
            "= \"ab\"", "= \"s1\"", "= \"s2\"", "= 42", "= \"w1\"", "= \"p1\"", "= \"h1\"", "= 2",
            "= 4", "= 6"
        ]
    );
    client.stop();
}

#[test]
fn inlay_hints_in_lists_in_a_layer() {
    let dir = workspace(
        "lsp-nested-layer-hints",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }]), .. }"#,
            ),
            (
                "base/a.crod",
                "+a: { list: [{ deep: [{ x: 1 + 1 }] }], opt: Some({ y: 2 + 2 }) },\n",
            ),
        ],
    );
    let mut client = Client::start_with(&dir, json!({ "debounce_ms": 10, "inlay_hints": true }));
    let buffer = Buffer::open(&client, &dir, "base/a.crod");
    assert_eq!(inlay_labels(&mut client, &buffer), ["= 2", "= 4"]);
    client.stop();
}

#[test]
fn code_actions_know_the_type_of_blocks_in_lists() {
    let text = "#![schema(schema)]\n\n{ backends: [Remote { host: \"h\" }] }";
    let dir = workspace(
        "lsp-nested-actions",
        &[("schema.crol", NESTED_SCHEMA), ("config.crol", text)],
    );
    let mut client = Client::start(&dir);
    let file = uri(&dir.join("config.crol"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": text } }),
    );
    let diagnostics = client.diagnostics_for(&file, |d| !d.is_empty());
    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": file },
            "range": diagnostics[0]["range"],
            "context": { "diagnostics": diagnostics },
        }),
    );
    // The placeholder comes from the field's type.
    assert!(
        actions.to_string().contains("retry: Retry { .. }"),
        "{actions}"
    );
    client.stop();
}

// ---- What an editor does and the other tests do not ----

#[test]
fn completion_is_triggered_where_fields_and_values_start() {
    let dir = workspace("lsp-triggers", &[("config.crol", "")]);
    let (server_side, client_side) = Connection::memory();
    let server = thread::spawn(move || {
        let server = crol_lsp::Server::start(server_side, None).unwrap();
        server.run().unwrap();
    });
    let mut client = Client {
        connection: client_side,
        server: Some(server),
        next_id: 0,
    };
    let result = client.request(
        "initialize",
        json!({ "capabilities": {}, "workspaceFolders": [{ "uri": uri(&dir), "name": "test" }] }),
    );
    client.notify("initialized", json!({}));
    let triggers = result["capabilities"]["completionProvider"]["triggerCharacters"]
        .as_array()
        .unwrap();
    // Zed asks on word characters and on these, and on nothing else.
    for c in [".", "&", "<", "@", ":", " ", "{", "[", "(", ","] {
        assert!(triggers.contains(&json!(c)), "{c:?} in {triggers:?}");
    }
    client.stop();
}

#[test]
fn accepting_a_completion_replaces_the_word_at_the_cursor() {
    let (mut client, mut buffer) = nested_document("lsp-nested-edits");
    let cases = [
        (
            "{ server: { tls: { en| } } }",
            "enabled",
            "{ server: { tls: { enabled: false } } }",
        ),
        (
            "{ server.tls.cert.pa| }",
            "path",
            "{ server.tls.cert.path: \"\" }",
        ),
        (
            "{ server.tls.cert.| }",
            "path",
            "{ server.tls.cert.path: \"\" }",
        ),
        (
            "{ server.tls.en|abled }",
            "enabled",
            "{ server.tls.enabled: false }",
        ),
        // A value is there already.
        (
            "{ server.tls.en|: true }",
            "enabled",
            "{ server.tls.enabled: true }",
        ),
        (
            "{ server: { tls: { en|: true } } }",
            "enabled",
            "{ server: { tls: { enabled: true } } }",
        ),
        (
            "{ services: [{ server: { tls: { cert: { mode: St| } } } }] }",
            "Strict",
            "{ services: [{ server: { tls: { cert: { mode: Strict } } } }] }",
        ),
        (
            "{ backends: [Re|] }",
            "Remote",
            "{ backends: [Remote { .. }] }",
        ),
    ];
    let mut failures = Vec::new();
    for (marked, label, wanted) in cases {
        let marked = format!("{HEADER}{marked}");
        let (text, _) = at_marker(&marked);
        let items = buffer.complete(&mut client, &marked);
        let Some(item) = items.iter().find(|i| i["label"] == label) else {
            failures.push(format!("{marked:?}: no {label} in {:?}", labels(&items)));
            continue;
        };
        let accepted = if item["textEdit"].is_null() {
            "(no textEdit)".to_owned()
        } else {
            apply(&text, std::slice::from_ref(&item["textEdit"]))
        };
        if accepted != format!("{HEADER}{wanted}") || item["filterText"] != label {
            failures.push(format!("{marked:?}: {accepted:?}, {item}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    client.stop();
}

#[test]
fn completion_counts_columns_as_the_client_does() {
    let (mut client, mut buffer) = nested_document("lsp-nested-columns");
    let cases: &[Case<'_>] = &[
        (
            "{ name: \"héllo 😀\", server: { tls: { | } } }",
            &["cert"],
            &["name"],
        ),
        (
            "{ \"é\": 1, server: { tls: { en| } } }",
            &["enabled"],
            &["name"],
        ),
        (
            "{\r\n  server: {\r\n    tls: {\r\n      |\r\n    }\r\n  }\r\n}",
            &["cert"],
            &["name"],
        ),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);
    client.stop();
}

#[test]
fn nothing_is_offered_in_strings_and_comments() {
    let (mut client, mut buffer) = nested_document("lsp-nested-strings");
    let none: &[&str] = &[];
    let name: &[&str] = &["name", "server"];
    let cases: &[Case<'_>] = &[
        ("{ name: \"a: |\" }", none, name),
        ("{ name: \"{ |\" }", none, name),
        ("{ name: \"a, |", none, name),
        ("{ // a: |\n}", none, name),
        ("{ /* { | */ }", none, name),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);
    client.stop();
}

/// A word after a character of more than one byte: typing in a string or
/// comment, where Zed asks for completions as anywhere else.
#[test]
fn text_outside_ascii_before_the_cursor_does_not_end_the_server() {
    let (mut client, mut buffer) = nested_document("lsp-nested-unicode");
    for marked in [
        "{ name: \"é|\" }",
        "{ name: \"caf€e|\" }",
        "{ // é|\n}",
        "{ é|",
    ] {
        buffer.complete(&mut client, &format!("{HEADER}{marked}"));
    }
    let items = buffer.complete(&mut client, &format!("{HEADER}{{ | }}"));
    assert!(labels(&items).contains(&"name"), "{items:?}");
    client.stop();
}

/// Every prefix of a document is a state it passes through while typing.
#[test]
fn completion_answers_at_the_end_of_every_prefix() {
    let (mut client, mut buffer) = nested_document("lsp-nested-prefixes");
    let text = "#![schema(schema)]\n{ // é\n  services: [{ name: \"ü\", server.tls: { } }],\n  <a + !b> { by_name[\"é\"].server<c>: Some((1, r#\"x\"#)) },\n  ..&t, x := [1 2], -y,\n} ] ) }";
    for (end, _) in text.char_indices() {
        buffer.complete(&mut client, &format!("{}|", &text[..end]));
    }
    let items = buffer.complete(&mut client, &format!("{HEADER}{{ | }}"));
    assert!(labels(&items).contains(&"name"), "{items:?}");
    client.stop();
}

#[test]
fn a_document_just_opened_is_answered_at_once() {
    let dir = workspace(
        "lsp-just-opened",
        &[("schema.crol", NESTED_SCHEMA), ("config.crol", "")],
    );
    // An evaluation after a pause would come too late.
    let mut client = Client::start_with(&dir, json!({ "debounce_ms": 60000 }));
    let (text, position) = at_marker("#![schema(schema)]\n\n{ server: { tls: { | } } }");
    let file = uri(&dir.join("config.crol"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": text } }),
    );
    let items = client.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": file }, "position": position }),
    );
    assert_eq!(
        labels(items.as_array().unwrap()),
        ["cert", "enabled"],
        "{items}"
    );
    client.stop();
}

#[test]
fn a_standalone_crod_file_is_data_mode() {
    let text = "#![type(Config)]\n\n+port: 80\n";
    let dir = workspace("lsp-standalone-data-errors", &[("data.crod", text)]);
    let client = Client::start(&dir);
    let file = uri(&dir.join("data.crod"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": text } }),
    );
    client.diagnostics_for(&file, |d| {
        d.iter()
            .any(|d| d["message"].as_str().unwrap().contains("type"))
    });
    client.stop();
}

#[test]
fn a_standalone_crod_file_completes_its_own_variants() {
    let text = "+port: 80\n+name: \"a\"\n<prod> { port: 443 }\n<prod> { name: \"b\" }\n";
    let dir = workspace("lsp-standalone-variants", &[("data.crod", text)]);
    let mut buffer_client = Client::start(&dir);
    let file = uri(&dir.join("data.crod"));
    buffer_client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": text } }),
    );
    let at = json!({
        "textDocument": { "uri": file },
        "position": { "line": 3, "character": 1 },
    });
    // Requests are answered from the latest finished evaluation.
    let mut found = false;
    for _ in 0..200 {
        let items = buffer_client.request("textDocument/completion", at.clone());
        if items.to_string().contains("prod") {
            found = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(found);
    buffer_client.stop();
}

#[test]
fn a_standalone_crol_file_is_value_mode() {
    let text = "<prod> { port: 443 }\n";
    let dir = workspace("lsp-standalone-value", &[("value.crol", text)]);
    let client = Client::start(&dir);
    let file = uri(&dir.join("value.crol"));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": file, "languageId": "crol", "version": 1, "text": text } }),
    );
    // Variants are data mode's.
    client.diagnostics_for(&file, |d| !d.is_empty());
    client.stop();
}

#[test]
fn a_crol_file_in_a_layer_directory_is_reported_on_it() {
    let dir = workspace(
        "lsp-crol-in-layer",
        &[
            (
                "project.crol",
                "Project { layers: List([{ name: \"base\", dir: \"base\", .. }]), .. }",
            ),
            ("base/a.crod", "+a: 1\n"),
            ("base/stray.crol", "1\n"),
        ],
    );
    let client = Client::start(&dir);
    client.diagnostics_for(&uri(&dir.join("base/stray.crol")), |d| {
        d.iter()
            .any(|d| d["message"].as_str().unwrap().contains("cannot be a layer"))
    });
    client.stop();
}

// ---- What a document builds on: `extends`, `files`, failed layers ----

/// Opens `text` as `uri` without waiting for anything.
fn open_text(client: &Client, uri: &str, text: &str) {
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "crol", "version": 1, "text": text } }),
    );
}

/// Whether a diagnostic's message holds `text`.
fn says(diagnostics: &[Value], text: &str) -> bool {
    diagnostics
        .iter()
        .any(|d| d["message"].as_str().unwrap().contains(text))
}

#[test]
fn a_document_on_its_own_is_evaluated_on_what_it_extends() {
    let dir = workspace(
        "lsp-extends",
        &[
            ("base.crod", "+web: { port: 80, name: \"w\" }\n"),
            ("local.crod", "#![extends(base)]\n\nweb.port: & + 1\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local.crod"));
    open_text(&client, &local, "#![extends(base)]\n\nweb.port: & + 1\n");
    client.diagnostics_for(&local, |d| d.is_empty());
    let mut buffer = Buffer {
        uri: local,
        version: 1,
    };

    // Hover has the operations on the key, in what is below too.
    let hover = buffer.hover(&mut client, "#![extends(base)]\n\nweb.po|rt: & + 1\n");
    assert!(hover.contains("`81`"), "{hover}");
    assert!(hover.contains("base.crod"), "{hover}");

    // Completion sees the keys of the extended document.
    let items = buffer.complete(&mut client, "#![extends(base)]\n\nx: &root.|\n");
    assert!(labels(&items).contains(&"web"), "{items:?}");

    // Go to definition leaves the document for the one that defines it.
    let (text, position) = at_marker("#![extends(base)]\n\nw|eb.port: & + 1\n");
    buffer.change(&client, &text);
    client.diagnostics_for(&buffer.uri, |d| d.is_empty());
    let found = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": buffer.uri }, "position": position }),
    );
    assert_eq!(found["uri"], uri(&dir.join("base.crod")), "{found}");
    client.stop();
}

#[test]
fn an_extended_file_that_changes_evaluates_the_document_again() {
    let dir = workspace(
        "lsp-extends-changes",
        &[
            ("base.crod", "+web: { port: 80 }\n"),
            ("local.crod", "#![extends(base)]\n\ndb.host: \"h\"\n"),
        ],
    );
    let client = Client::start(&dir);
    let local = uri(&dir.join("local.crod"));
    let base = uri(&dir.join("base.crod"));
    open_text(&client, &local, "#![extends(base)]\n\ndb.host: \"h\"\n");
    client.diagnostics_for(&local, |d| says(d, "cannot be patched"));

    // An open buffer counts, saved or not.
    open_text(&client, &base, "+web: { port: 80 }\n+db: { host: \"a\" }\n");
    client.diagnostics_for(&local, |d| d.is_empty());

    // Closed, the file on disk counts again.
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": base } }),
    );
    client.diagnostics_for(&local, |d| says(d, "cannot be patched"));

    // A change on disk, as the client's file watcher reports it.
    std::fs::write(dir.join("base.crod"), "+db: { host: \"b\" }\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": base, "type": 2 }] }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());
    client.stop();
}

#[test]
fn the_server_asks_to_be_told_about_files_on_disk() {
    let dir = workspace("lsp-watch", &[("a.crod", "+a: 1\n")]);
    let (server_side, client_side) = Connection::memory();
    let server = thread::spawn(move || {
        crol_lsp::Server::start(server_side, None)
            .unwrap()
            .run()
            .unwrap();
    });
    let send = |message: Message| client_side.sender.send(message).unwrap();
    send(Message::Request(Request::new(
        RequestId::from(1),
        "initialize".to_owned(),
        json!({
            "capabilities": {
                "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } },
            },
            "workspaceFolders": [{ "uri": uri(&dir), "name": "test" }],
        }),
    )));
    send(Message::Notification(Notification::new(
        "initialized".to_owned(),
        json!({}),
    )));
    let registration = loop {
        let message = client_side
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        if let Message::Request(request) = message
            && request.method == "client/registerCapability"
        {
            break request;
        }
    };
    let text = registration.params.to_string();
    assert!(text.contains("workspace/didChangeWatchedFiles"), "{text}");
    assert!(text.contains("crod"), "{text}");
    send(Message::Request(Request::new(
        RequestId::from(2),
        "shutdown".to_owned(),
        json!(null),
    )));
    send(Message::Notification(Notification::new(
        "exit".to_owned(),
        json!(null),
    )));
    server.join().unwrap();
}

#[test]
fn a_failed_extends_is_reported_on_the_attribute() {
    let dir = workspace(
        "lsp-extends-failed",
        &[
            ("base.crod", "+web: { port: 80 }\n"),
            ("ring_a.crod", "#![extends(ring_b)]\n\n+a: 1\n"),
            ("ring_b.crod", "#![extends(ring_a)]\n\n+b: 1\n"),
            (
                "missing.crod",
                "#![extends(base)]\n#![extends(nothing)]\n\nweb.port: 1\n",
            ),
        ],
    );
    let client = Client::start(&dir);
    let file = uri(&dir.join("missing.crod"));
    open_text(
        &client,
        &file,
        &std::fs::read_to_string(dir.join("missing.crod")).unwrap(),
    );
    let diagnostics = client.diagnostics_for(&file, |d| !d.is_empty());
    // On the second attribute; what the first brought in still counts.
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 1);
    assert!(says(&diagnostics, "nothing"), "{diagnostics:?}");

    let ring = uri(&dir.join("ring_a.crod"));
    open_text(
        &client,
        &ring,
        &std::fs::read_to_string(dir.join("ring_a.crod")).unwrap(),
    );
    let diagnostics = client.diagnostics_for(&ring, |d| !d.is_empty());
    assert!(says(&diagnostics, "cycle"), "{diagnostics:?}");
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);

    // A document that extends itself.
    let own = uri(&dir.join("base.crod"));
    open_text(&client, &own, "#![extends(base)]\n\n+web: 1\n");
    let diagnostics = client.diagnostics_for(&own, |d| says(d, "cycle"));
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);
    client.stop();
}

#[test]
fn a_problem_in_an_extended_document_is_shown_on_the_attribute() {
    let dir = workspace(
        "lsp-extends-broken",
        &[
            ("base.crod", "+web: 1 / 0\n"),
            ("local.crod", "#![extends(base)]\n\n+db: 1\n"),
        ],
    );
    let client = Client::start(&dir);
    let local = uri(&dir.join("local.crod"));
    open_text(&client, &local, "#![extends(base)]\n\n+db: 1\n");
    let diagnostics = client.diagnostics_for(&local, |d| !d.is_empty());
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);
    assert!(says(&diagnostics, "division"), "{diagnostics:?}");
    client.stop();
}

#[test]
fn a_layer_of_a_project_is_evaluated_on_what_it_extends() {
    let dir = workspace(
        "lsp-extends-project",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "local", dir: "local", .. }]), .. }"#,
            ),
            ("shared/base.crod", "+web: { port: 80 }\n"),
            (
                "local/a.crod",
                "#![extends(::shared::base)]\n\nweb.port: & + 1\n",
            ),
        ],
    );
    let client = Client::start(&dir);
    let file = uri(&dir.join("local/a.crod"));
    // What is extended is not a lower layer, which tools warn about.
    let diagnostics = client.diagnostics_for(&file, |d| !d.is_empty());
    assert!(says(&diagnostics, "not a lower layer"), "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], 2, "{diagnostics:?}");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    client.stop();
}

/// A schema with a default, and a type for `files`.
const DEFAULTS_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Services",
    types: {
        "app::Services": Transparent {
            name: "Services",
            source: ("src/lib.rs", 3),
            inner: Map(String, Named("app::Service")),
            ..
        },
        "app::Service": Struct {
            name: "Service",
            source: ("src/lib.rs", 1),
            fields: {
                replicas: { ty: U32, default: Value(3), .. },
                name: { ty: String, default: Value("x"), .. },
            },
            ..
        },
        "app::Config": Struct {
            name: "Config",
            source: ("src/lib.rs", 2),
            fields: { port: { ty: U16, doc: "The port.", .. } },
            ..
        },
    },
}"#;

#[test]
fn files_give_value_documents_a_type() {
    let project = r#"Project {
        layers: List([{ name: "base", dir: "data", .. }]),
        schema: Some("schema"),
        files: [{ glob: "config::*", type: "app::Config" }],
        ..
    }"#;
    let dir = workspace(
        "lsp-files",
        &[
            ("project.crol", project),
            ("schema.crol", DEFAULTS_SCHEMA),
            ("data/a.crod", "+web: Service { .. }\n"),
            ("config/app.crol", "{ port: 80 }\n"),
            ("other/app.crol", "{ port: \"not checked\" }\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "config/app.crol");
    let items = buffer.complete(&mut client, "{ po| }\n");
    assert!(labels(&items).contains(&"port"), "{items:?}");
    buffer.change(&client, "{ port: \"x\" }\n");
    let diagnostics = client.diagnostics_for(&buffer.uri, |d| !d.is_empty());
    assert!(says(&diagnostics, "found \"x\""), "{diagnostics:?}");

    // A document that matches no glob is not given the project's schema.
    let other = uri(&dir.join("other/app.crol"));
    open_text(&client, &other, "{ port: \"not checked\" }\n");
    client.diagnostics_for(&other, |d| d.is_empty());
    client.stop();
}

#[test]
fn a_type_the_schema_does_not_have_is_reported() {
    let project = r#"Project {
        layers: List([{ name: "base", dir: "data", .. }]),
        schema: Some("schema"),
        files: [{ glob: "config::*", type: "app::Nothing" }],
        ..
    }"#;
    let dir = workspace(
        "lsp-files-unknown",
        &[
            ("project.crol", project),
            ("schema.crol", DEFAULTS_SCHEMA),
            ("data/a.crod", "+web: Service { .. }\n"),
            ("config/app.crol", "{ port: 80 }\n"),
        ],
    );
    let client = Client::start(&dir);
    let file = uri(&dir.join("config/app.crol"));
    open_text(&client, &file, "{ port: 80 }\n");
    let diagnostics = client.diagnostics_for(&file, |d| !d.is_empty());
    assert!(says(&diagnostics, "app::Nothing"), "{diagnostics:?}");
    client.stop();
}

/// A project of three layers, the middle one's directory as given.
fn three_layers(name: &str, middle: &[(&str, &str)], middle_dir: &str) -> PathBuf {
    let project = format!(
        r#"Project {{ layers: List([
            {{ name: "base", dir: "base", .. }},
            {{ name: "middle", dir: "{middle_dir}", .. }},
            {{ name: "top", dir: "top", .. }},
        ]), .. }}"#
    );
    let mut files = vec![
        ("project.crol", project.as_str()),
        ("base/a.crod", "+web: { port: 80 }\n"),
        ("top/a.crod", "web.port: & + 1\n+db: 2\n"),
    ];
    files.extend_from_slice(middle);
    workspace(name, &files)
}

#[test]
fn a_stray_file_leaves_the_rest_of_its_layer_and_the_layers_after_it() {
    let dir = three_layers(
        "lsp-failed-file",
        &[
            ("middle/a.crod", "+cache: 1\n"),
            ("middle/stray.crol", "1\n"),
        ],
        "middle",
    );
    let mut client = Client::start(&dir);
    client.diagnostics_for(&uri(&dir.join("middle/stray.crol")), |d| {
        says(d, "cannot be a layer")
    });
    let mut buffer = Buffer::open(&client, &dir, "top/a.crod");
    let hover = buffer.hover(&mut client, "web.po|rt: & + 1\n+db: 2\n");
    assert!(hover.contains("`81`"), "{hover}");
    let items = buffer.complete(&mut client, "web.port: & + 1\n+db: 2\nx: &root.|\n");
    let found = labels(&items);
    assert!(
        found.contains(&"cache") && found.contains(&"db"),
        "{found:?}"
    );
    client.stop();
}

#[test]
fn a_layer_that_cannot_be_found_leaves_the_layers_after_it() {
    let dir = three_layers("lsp-failed-layer", &[], "nothing");
    let mut client = Client::start(&dir);
    client.diagnostics_for(&uri(&dir.join("project.crol")), |d| {
        says(d, "layer `middle`")
    });
    let mut buffer = Buffer::open(&client, &dir, "top/a.crod");
    let hover = buffer.hover(&mut client, "web.po|rt: & + 1\n+db: 2\n");
    assert!(hover.contains("`81`"), "{hover}");
    client.stop();
}

#[test]
fn what_a_failed_layer_would_have_defined_is_no_error_above_it() {
    let dir = three_layers("lsp-failed-gap", &[], "nothing");
    std::fs::write(dir.join("top/a.crod"), "cache.size: 1\nweb.port: & + 1\n").unwrap();
    let mut client = Client::start(&dir);
    let file = uri(&dir.join("top/a.crod"));
    let diagnostics = client.diagnostics_for(&file, |d| !d.is_empty());
    assert!(says(&diagnostics, "cannot be patched"), "{diagnostics:?}");
    assert!(says(&diagnostics, "`middle`"), "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], 2, "{diagnostics:?}");
    // The layers below the gap still evaluate.
    open_text(&client, &file, "cache.size: 1\nweb.port: & + 1\n");
    let mut buffer = Buffer {
        uri: file,
        version: 1,
    };
    let items = buffer.complete(&mut client, "cache.size: 1\nx: &root.|\n");
    assert!(labels(&items).contains(&"web"), "{items:?}");
    client.stop();
}

#[test]
fn default_reads_the_project_schema() {
    let project = r#"Project {
        layers: List([{ name: "base", dir: "data", .. }]),
        schema: Some("schema"),
        ..
    }"#;
    let text = "+web: Service { replicas: &default * 2, .. }\n";
    let dir = workspace(
        "lsp-default-project",
        &[
            ("project.crol", project),
            ("schema.crol", DEFAULTS_SCHEMA),
            ("data/a.crod", text),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "data/a.crod");
    let hover = buffer.hover(
        &mut client,
        "+web: Service { replicas: &default * 2, .. }\n+top: &root.web.repl|icas\n",
    );
    assert!(hover.contains("`6`"), "{hover}");
    client.stop();
}

#[test]
fn default_reads_the_schema_of_a_document_on_its_own() {
    let text = "#![schema(schema)]\n\n+web: Service { replicas: &default * 2, .. }\n";
    let dir = workspace(
        "lsp-default-alone",
        &[
            ("schema.crol", DEFAULTS_SCHEMA),
            ("a.crod", text),
            ("b.crod", ""),
        ],
    );
    let client = Client::start(&dir);
    let file = uri(&dir.join("a.crod"));
    open_text(&client, &file, text);
    client.diagnostics_for(&file, |d| d.is_empty());

    // Without a schema it stays an error.
    let plain = uri(&dir.join("b.crod"));
    open_text(&client, &plain, "+web: &default\n");
    client.diagnostics_for(&plain, |d| says(d, "no schema is given"));
    client.stop();
}

#[test]
fn a_document_on_its_own_keeps_its_last_good_evaluation() {
    let text = "#![schema(schema)]\n\n+web: Service { replicas: 7, name: \"w\" }\n";
    let dir = workspace(
        "lsp-last-good",
        &[("schema.crol", DEFAULTS_SCHEMA), ("a.crod", text)],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crod");
    let broken = "#![schema(schema)]\n\n+web: Service { replicas: 7, name: 1 / 0 }\n\
                  +top: &root.web.repl|icas\n";
    let hover = buffer.hover(&mut client, broken);
    assert!(hover.contains("`7`"), "{hover}");
    assert!(hover.contains("last"), "{hover}");

    // Filling in a field writes its value, not a placeholder.
    let items = buffer.complete(
        &mut client,
        "#![schema(schema)]\n\n+web: Service { name: 1 / 0, rep| }\n",
    );
    let replicas = items.iter().find(|i| i["label"] == "replicas").unwrap();
    assert_eq!(replicas["textEdit"]["newText"], "replicas: 7", "{replicas}");
    client.stop();
}

#[test]
fn a_value_document_keeps_its_last_good_evaluation() {
    let dir = workspace("lsp-last-good-value", &[("a.crol", "{ port: 80 }\n")]);
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crol");
    let hover = buffer.hover(&mut client, "{ port: 1 / 0, copy: &root.po|rt }\n");
    assert!(hover.contains("`80`"), "{hover}");
    client.stop();
}

#[test]
fn a_stray_file_in_an_extended_directory_is_reported_on_it() {
    let dir = workspace(
        "lsp-extends-stray",
        &[
            ("shared/a.crod", "+web: 1\n"),
            ("shared/stray.crol", "1\n"),
            ("local.crod", "#![extends(shared)]\n\nweb: 2\n"),
        ],
    );
    let client = Client::start(&dir);
    let local = uri(&dir.join("local.crod"));
    open_text(&client, &local, "#![extends(shared)]\n\nweb: 2\n");
    client.diagnostics_for(&uri(&dir.join("shared/stray.crol")), |d| {
        says(d, "cannot be a layer")
    });
    client.stop();
}

#[test]
fn super_paths_and_directories_can_be_extended() {
    let dir = workspace(
        "lsp-extends-super",
        &[
            ("shared/a.crod", "+web: { port: 1 }\n"),
            ("shared/b.crod", "+db: { port: 2 }\n"),
            ("deep/er/local.crod", ""),
        ],
    );
    let client = Client::start(&dir);
    let local = uri(&dir.join("deep/er/local.crod"));
    let text = "#![extends(super::super::shared)]\n\nweb.port: 3\ndb.port: 4\n";
    open_text(&client, &local, text);
    client.diagnostics_for(&local, |d| d.is_empty());
    client.stop();
}

/// A schema of the enums serde writes without a name.
const TAGGED_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Root",
    types: {
        "app::Root": Struct {
            name: "Root",
            source: ("src/lib.rs", 1),
            fields: {
                internal: { ty: Named("app::Internal"), .. },
                adjacent: { ty: Named("app::Adjacent"), .. },
                untagged: { ty: Named("app::Untagged"), .. },
                external: { ty: Named("app::External"), .. },
            },
            ..
        },
        "app::Internal": Enum {
            name: "Internal",
            source: ("src/lib.rs", 2),
            repr: Internal { tag: "kind" },
            variants: {
                Disk: { shape: Struct({ path: { ty: String, .. } }), .. },
                Memory: { shape: Newtype(Named("app::Memory")), .. },
            },
            ..
        },
        "app::Memory": Struct {
            name: "Memory",
            source: ("src/lib.rs", 3),
            fields: { size: { ty: U32, .. }, path: { ty: String, .. } },
            ..
        },
        "app::Adjacent": Enum {
            name: "Adjacent",
            source: ("src/lib.rs", 4),
            repr: Adjacent { tag: "t", content: "c" },
            variants: { A: { shape: Unit, .. } },
            ..
        },
        "app::Untagged": Enum {
            name: "Untagged",
            source: ("src/lib.rs", 5),
            repr: Untagged,
            variants: {
                One: { shape: Struct({ first: { ty: U32, .. } }), .. },
                Two: { shape: Struct({ second: { ty: U32, .. } }), .. },
            },
            ..
        },
        "app::External": Enum {
            name: "External",
            source: ("src/lib.rs", 6),
            variants: { One: { shape: Struct({ first: { ty: U32, .. } }), .. } },
            ..
        },
    },
}"#;

#[test]
fn enums_without_a_name_offer_their_fields() {
    let dir = workspace(
        "lsp-tagged",
        &[
            ("schema.crol", TAGGED_SCHEMA),
            ("a.crol", "#![schema(schema)]\n\n{ .. }\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crol");
    let cases: &[Case<'_>] = &[
        ("{ internal: { | } }", &["kind", "path", "size"], &[]),
        ("{ internal: { kind: \"Disk\", | } }", &["path"], &["kind"]),
        ("{ adjacent: { | } }", &["t", "c"], &[]),
        ("{ untagged: { | } }", &["first", "second"], &[]),
        // The name says the variant here, and a block has none.
        ("{ external: { | } }", &[], &["first"]),
    ];
    check_completions(&mut client, &mut buffer, HEADER, cases);
    client.stop();
}

#[test]
fn hover_on_a_dotted_target_is_about_the_segment_under_it() {
    let text = "#![schema(schema)]\n\n{ server: { tls: { cert: { path: \"a\" } } } }\n";
    let dir = workspace(
        "lsp-hover-segment",
        &[("schema.crol", NESTED_SCHEMA), ("config.crol", text)],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "config.crol");
    let text = |marked: &str| format!("#![schema(schema)]\n\n{{ {marked}: \"a\" }}\n");
    for (marked, wanted) in [
        ("ser|ver.tls.cert.path", "`Server`"),
        ("server.t|ls.cert.path", "How to encrypt.\n\n`Tls`"),
        ("server.tls.ce|rt.path", "`Cert`"),
        (
            "server.tls.cert.pa|th",
            "Where the certificate is.\n\n`String`",
        ),
    ] {
        let hover = buffer.hover(&mut client, &text(marked));
        assert_eq!(hover, wanted, "{marked}");
    }
    client.stop();
}

#[test]
fn inlay_hints_on_elements() {
    let dir = workspace(
        "lsp-element-hints",
        &[
            (
                "project.crol",
                r#"Project { layers: List([{ name: "base", dir: "base", .. }]), .. }"#,
            ),
            (
                "base/a.crod",
                "+a: [1 + 1, 2, 3 * 3]\n+b: (2 + 2, \"x\")\n+c: Some(1 + 4)\n+d: [1] + [2 + 4]\n",
            ),
            ("value.crol", "{ a: [1 + 1, { b: [2 + 2] }] }\n"),
        ],
    );
    let mut client = Client::start_with(&dir, json!({ "debounce_ms": 10, "inlay_hints": true }));
    let buffer = Buffer::open(&client, &dir, "base/a.crod");
    assert_eq!(
        inlay_labels(&mut client, &buffer),
        ["= 2", "= 9", "= 4", "= 5", "= [1, 6]"]
    );
    let buffer = Buffer::open(&client, &dir, "value.crol");
    assert_eq!(inlay_labels(&mut client, &buffer), ["= 2", "= 4"]);
    client.stop();
}

/// Types to generate a real schema file from, as an application does.
#[allow(dead_code)]
mod generated {
    /// The main configuration.
    #[derive(crol::Schema)]
    pub(super) struct Config {
        /// The name of the configuration.
        name: String,
        /// Labels to attach.
        tags: Vec<String>,
        /// A shorter name, if there is one.
        nick: Option<String>,
        /// How much it may use.
        limits: Limits,
        /// How it runs.
        mode: Mode,
        /// The next one along.
        next: Option<Box<Config>>,
        undocumented: u32,
    }

    /// Limits on what it uses.
    #[derive(crol::Schema)]
    pub(super) struct Limits {
        /// Memory, in megabytes.
        memory: u32,
    }

    /// How something runs.
    #[derive(crol::Schema)]
    pub(super) enum Mode {
        /// As fast as it can.
        Fast,
        Slow,
        /// By hand.
        Custom {
            /// How hard to try.
            effort: u32,
        },
    }
}

fn generated_schema() -> String {
    crol::schema::build::<generated::Config>()
        .to_text()
        .unwrap()
}

/// The schema of data where each root value is a `Config`: a map, as a struct
/// root type would be the whole document (`SPEC.md` 4.1).
fn generated_data_schema() -> String {
    crol::schema::build::<indexmap::IndexMap<String, generated::Config>>()
        .to_text()
        .unwrap()
}

const GENERATED_PROJECT: &str = r#"Project {
    layers: List([
        { name: "base", dir: "base", read_only: true },
        { name: "local", dir: "local", .. },
    ]),
    schema: Some("crol::schema"),
    files: [{ glob: "cfg::*", type: "app::Config" }],
    ..,
}
"#;

/// `text` with `|` before `word`, at the first place `context` is, for
/// [`Buffer::hover`].
fn mark(text: &str, context: &str, word: &str) -> String {
    let at = text.find(context).expect(context) + context.find(word).expect(word);
    format!("{}|{}", &text[..at], &text[at..])
}

/// A hover case: where it is, as in [`mark`], and how the text starts.
type HoverCase<'a> = (&'a str, &'a str, &'a str);

/// Hovers on each case, and reports every one that fails at once.
fn check_hovers(client: &mut Client, buffer: &mut Buffer, text: &str, cases: &[HoverCase<'_>]) {
    let mut failures = Vec::new();
    for (context, word, wanted) in cases {
        let markdown = buffer.hover(client, &mark(text, context, word));
        if !markdown.starts_with(wanted) {
            failures.push(format!(
                "{word} in {context}: wanted {wanted:?}, got {markdown:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Hovers on every character, as the mouse goes. A fence that is not at the
/// start of a line does not open in Markdown, and shows as raw text.
fn check_every_hover_renders(client: &mut Client, buffer: &Buffer, text: &str) {
    for (line, content) in text.lines().enumerate() {
        for character in 0..content.encode_utf16().count() {
            let hover = client.request(
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": buffer.uri },
                    "position": { "line": line, "character": character },
                }),
            );
            let Some(markdown) = hover["contents"]["value"].as_str() else {
                continue;
            };
            let place = format!("{line}:{character}: {markdown:?}");
            assert_eq!(hover["contents"]["kind"], "markdown", "{place}");
            for (at, _) in markdown.match_indices("```") {
                assert!(at == 0 || markdown[..at].ends_with('\n'), "{place}");
            }
            // Not a value: a doc and a type.
            assert!(!markdown.starts_with("**"), "{place}");
            assert!(markdown.lines().count() <= 5, "{place}");
        }
    }
}

#[test]
fn hover_in_a_project_file_shows_the_doc_of_the_field() {
    let dir = workspace(
        "lsp-hover-project-file",
        &[
            ("project.crol", GENERATED_PROJECT),
            ("crol/schema.crol", &generated_schema()),
            ("base/a.crod", ""),
            ("local/a.crod", ""),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "project.crol");
    let layer = "The layer's name.\n\n`String`";
    check_hovers(
        &mut client,
        &mut buffer,
        GENERATED_PROJECT,
        &[
            // The type name of a value that is no field's.
            ("Project {", "Project", "A project file"),
            // A key, and a value through a variant, a list and a block.
            (
                "layers:",
                "layers",
                "The layers, lowest first.\n\n`ProjectLayers`",
            ),
            (
                "List(",
                "List",
                "The layers, listed.\n\n`ProjectLayers::List`",
            ),
            ("name: \"base\"", "name", layer),
            ("\"base\"", "base", layer),
            ("dir: \"base\"", "dir", "Its module path"),
            ("read_only:", "read_only", "Generated or vendored data"),
            ("true", "true", "Generated or vendored data"),
            // A value in `Some(...)`, and `Some` itself.
            ("schema:", "schema", "The schema:"),
            ("Some(", "Some", "The schema:"),
            ("\"crol::schema\"", "crol", "The schema:"),
            // In a list of the field's.
            ("glob:", "glob", "A module path whose segments"),
            ("\"cfg::*\"", "cfg", "A module path whose segments"),
            ("\"app::Config\"", "app", "The schema path of their type."),
        ],
    );
    buffer.change(&client, GENERATED_PROJECT);
    client.diagnostics_for(&buffer.uri, |_| true);
    check_every_hover_renders(&mut client, &buffer, GENERATED_PROJECT);
    client.stop();
}

#[test]
fn hover_in_a_schema_file_shows_the_doc_of_the_field() {
    let text = generated_schema();
    let dir = workspace("lsp-hover-schema-file", &[("crol/schema.crol", &text)]);
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "crol/schema.crol");
    let ty = "The type of the field.";
    let fields = "The fields, by the name data writes them with.";
    check_hovers(
        &mut client,
        &mut buffer,
        &text,
        &[
            ("Schema {", "Schema", "A schema file:"),
            ("format: 1", "format", "The format version"),
            ("format: 1", "1", "The format version"),
            ("root:", "root", "The path of the root type"),
            ("root: \"", "\"", "The path of the root type"),
            ("types:", "types", "Every named type"),
            // An entry of a map has the doc of the map's field.
            (
                "\"server::generated::Config\": Struct",
                "server",
                "Every named type",
            ),
            ("Struct {", "Struct", "A struct with named fields."),
            ("name: \"Config\"", "name", "The name data writes"),
            ("\"Config\"", "Config", "The name data writes"),
            ("doc: \"The main", "doc", "The doc comment"),
            ("\"The main", "The", "The doc comment"),
            ("source: (", "source", "Where it is defined"),
            // An element of a tuple has the field's doc too.
            ("source: (\"", "\"", "Where it is defined"),
            ("fields: {", "fields", fields),
            ("name: { ty", "name", fields),
            ("ty: String", "ty", ty),
            // The `:` and the bracket a value starts with are in the entry.
            ("ty: String", ":", ty),
            ("name: { ty", "{", fields),
            // A variant has its own doc; what it holds, the field's.
            ("ty: String", "String", "`String`, `str` or `PathBuf`"),
            ("ty: List(String)", "List", "`Vec<T>`, `VecDeque<T>`"),
            ("ty: Named(\"", "Named", "A type in the schema's `types`"),
            ("ty: Named(\"", "\"", ty),
            ("ty: Option(", "Option", "`Option<T>`"),
            // The variants of an enum and the fields in them.
            ("Enum {", "Enum", "An enum."),
            ("variants: {", "variants", "The variants, by"),
            ("shape: Unit", "shape", "What the variant holds"),
            ("shape: Unit", "Unit", "No value:"),
            ("shape: Struct", "Struct", "Named fields"),
        ],
    );
    buffer.change(&client, &text);
    client.diagnostics_for(&buffer.uri, |_| true);
    check_every_hover_renders(&mut client, &buffer, &text);
    client.stop();
}

#[test]
fn hover_in_data_shows_the_doc_of_the_field() {
    let text = "#![schema(crol::schema)]

+web: Config {
    name: \"w\",
    tags: [\"a\", \"b\"],
    nick: Some(\"n\"),
    limits: { memory: 512 },
    mode: Fast,
    next: None,
    undocumented: 1,
}
+db: Config {
    name: \"d\",
    tags: [],
    nick: None,
    limits: { memory: 1 },
    mode: Slow,
    next: Some(Config { name: \"e\", tags: [\"z\"], nick: None, limits: { memory: 7 }, mode: Custom { effort: 3 }, next: None, undocumented: 4 }),
    undocumented: 2,
}
";
    let dir = workspace(
        "lsp-hover-data",
        &[
            ("crol/schema.crol", &generated_data_schema()),
            ("a.crod", text),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crod");
    let name = "The name of the configuration.\n\n`String`";
    let tags = "Labels to attach.\n\n`String`";
    check_hovers(
        &mut client,
        &mut buffer,
        text,
        &[
            ("+web", "web", "The main configuration.\n\n`Config`"),
            ("Config {", "Config", "The main configuration."),
            ("name: \"w\"", "name", name),
            ("\"w\"", "w", name),
            ("\"b\"", "b", tags),
            (
                "Some(\"n\")",
                "n",
                "A shorter name, if there is one.\n\n`String`",
            ),
            ("nick: None", "None", "A shorter name, if there is one."),
            ("memory: 512", "512", "Memory, in megabytes.\n\n`u32`"),
            // A variant's doc, else the field's.
            ("mode: Fast", "Fast", "As fast as it can.\n\n`Mode::Fast`"),
            ("mode: Slow", "Slow", "How it runs.\n\n`Mode::Slow`"),
            ("Custom {", "Custom", "By hand."),
            // At any depth, through `Some` and a named block.
            ("\"e\"", "e", name),
            ("\"z\"", "z", tags),
            ("effort: 3", "3", "How hard to try.\n\n`u32`"),
        ],
    );
    // No doc: the type alone.
    for word in ["undocumented", "1"] {
        let markdown = buffer.hover(&mut client, &mark(text, "undocumented: 1", word));
        assert_eq!(markdown, "`u32`", "{word}");
    }
    client.stop();
}

#[test]
fn hover_shows_the_layers_only_when_there_are_several_operations() {
    let config = "Config { name: \"w\", tags: [], nick: None, limits: { memory: 1 }, \
                  mode: Fast, next: None, undocumented: 1 }";
    let base = format!("+web: {config}\n");
    let project = "Project {
    layers: List([{ name: \"base\", dir: \"base\", .. }, { name: \"top\", dir: \"top\", .. }]),
    schema: Some(\"crol::schema\"),
    ..,
}
";
    let dir = workspace(
        "lsp-hover-history",
        &[
            ("project.crol", project),
            ("crol/schema.crol", &generated_data_schema()),
            ("base/a.crod", &base),
            ("top/a.crod", "web.limits.memory: 2\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "top/a.crod");

    // Several operations: the doc, the type, and after them the layers.
    let markdown = buffer.hover(&mut client, "web.limits.mem|ory: 2\n");
    let (doc, history) = markdown.split_once("\n\n---\n\n").expect(&markdown);
    assert_eq!(doc, "Memory, in megabytes.\n\n`u32`");
    assert!(history.contains("layer `base`"), "{markdown}");
    assert!(history.contains("layer `top`"), "{markdown}");

    // The same on the value.
    assert_eq!(
        buffer.hover(&mut client, "web.limits.memory: |2\n"),
        markdown
    );

    // One operation: the doc alone.
    let mut below = Buffer::open(&client, &dir, "base/a.crod");
    let markdown = below.hover(&mut client, &mark(&base, "tags: []", "tags"));
    assert_eq!(markdown, "Labels to attach.\n\n`Vec<String>`");
    client.stop();
}

#[test]
fn hover_on_a_reference_has_what_it_names_after_the_doc() {
    let config = "Config { name: \"w\", tags: [], nick: None, limits: { memory: 1 }, \
                  mode: Fast, next: None, undocumented: 1 }";
    let text = format!(
        "#![schema(crol::schema)]\n\n+web: {config}\n+db: Config {{ name: &root.web.name, \
         tags: [], nick: None, limits: {{ memory: 1 }}, mode: Fast, next: None, \
         undocumented: 2 }}\n"
    );
    let dir = workspace(
        "lsp-hover-reference",
        &[
            ("crol/schema.crol", &generated_data_schema()),
            ("a.crod", &text),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crod");
    let markdown = buffer.hover(&mut client, &mark(&text, "&root.web.name", "web"));
    assert_eq!(
        markdown,
        "The name of the configuration.\n\n`String`\n\n**web.name**: `\"w\"`"
    );
    client.stop();
}

#[test]
fn hover_keeps_to_values_and_keys() {
    let text = "#![schema(crol::schema)]\n\nt := 1\nn = +web: Config { .. }\n";
    let dir = workspace(
        "lsp-hover-elsewhere",
        &[
            ("crol/schema.crol", &generated_data_schema()),
            ("a.crod", text),
        ],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crod");
    for (context, word) in [
        ("#![schema", "schema"),
        ("crol::schema", "crol"),
        ("t := 1", "t"),
        ("t := 1", "1"),
        ("n = +web", "n"),
        ("{ .. }", ".."),
        ("{ .. }", "}"),
    ] {
        let markdown = buffer.hover(&mut client, &mark(text, context, word));
        assert_eq!(markdown, "(no hover)", "{word} in {context}");
    }
    client.stop();
}

#[test]
fn hover_says_nothing_where_nothing_is_known() {
    let dir = workspace("lsp-hover-nothing", &[("a.crol", "{ port: 80 }\n")]);
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "a.crol");
    for marked in [
        "{ po|rt: 80, tags: [\"a\"], nested: { on: true } }\n",
        "{ port: 8|0, tags: [\"a\"], nested: { on: true } }\n",
        "{ port: 80, tags: [\"|a\"], nested: { on: true } }\n",
        "{ port: 80, tags: [\"a\"], nested: { on: tr|ue } }\n",
        "|{ port: 80, tags: [\"a\"], nested: { on: true } }\n",
    ] {
        assert_eq!(buffer.hover(&mut client, marked), "(no hover)", "{marked}");
    }
    client.stop();
}

#[test]
fn completion_at_every_level_of_a_schema_file() {
    let text = generated_schema();
    let dir = workspace("lsp-complete-schema-file", &[("crol/schema.crol", &text)]);
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "crol/schema.crol");
    let head = "#![schema(\"crate:crol@0.1\")]\n\n";
    let cases: &[Case<'_>] = &[
        ("Schema { | }", &["format", "root", "types"], &[]),
        ("Schema { format: 1, | }", &["root", "types"], &["format"]),
        (
            "Schema { types: { \"a::B\": | } }",
            &[
                "Struct",
                "TupleStruct",
                "Newtype",
                "Transparent",
                "UnitStruct",
                "Enum",
            ],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { | } } }",
            &[
                "name",
                "doc",
                "source",
                "fields",
                "deny_unknown_fields",
                "default",
                "explicit_defaults",
            ],
            &["repr", "variants"],
        ),
        (
            "Schema { types: { \"a::B\": Enum { | } } }",
            &["name", "doc", "source", "repr", "variants"],
            &["fields"],
        ),
        (
            "Schema { types: { \"a::B\": Enum { repr: | } } }",
            &["External", "Internal", "Adjacent", "Untagged"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Enum { repr: Internal { | } } } }",
            &["tag"],
            &["content"],
        ),
        (
            "Schema { types: { \"a::B\": Enum { variants: { V: { | } } } } }",
            &["shape", "doc", "aliases", "explicit_defaults"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Enum { variants: { V: { shape: | } } } } }",
            &["Unit", "Newtype", "Tuple", "Struct"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Enum { variants: { V: { shape: Struct({ x: { | } }) } } } } }",
            &["ty", "doc"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { fields: { x: { | } } } } }",
            &["ty", "doc", "aliases", "default", "flatten", "refers"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { fields: { x: { ty: String, | } } } } }",
            &["doc", "aliases", "default", "flatten", "refers"],
            &["ty"],
        ),
        (
            "Schema { types: { \"a::B\": Struct { fields: { x: { ty: | } } } } }",
            &[
                "Bool", "String", "U32", "Option", "List", "Map", "Named", "Any",
            ],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { fields: { x: { ty: List(|) } } } } }",
            &["Bool", "String", "Named"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { fields: { x: { ty: Map(String, Option(|)) } } } } }",
            &["Named", "Any"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { fields: { x: { default: | } } } } }",
            &["Required", "Unrecorded", "Value"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { deny_unknown_fields: | } } }",
            &["true", "false"],
            &[],
        ),
        (
            "Schema { types: { \"a::B\": Struct { explicit_defaults: | } } }",
            &["None", "Some", "true", "false"],
            &[],
        ),
    ];
    check_completions(&mut client, &mut buffer, head, cases);

    // Fields come with their docs and types, also of a type with no name.
    let items = buffer.complete(&mut client, &format!("{head}Schema {{ | }}"));
    let format = items.iter().find(|i| i["label"] == "format").unwrap();
    let doc = format["documentation"]["value"].as_str().unwrap();
    assert!(doc.starts_with("The format version"), "{format}");
    let items = buffer.complete(
        &mut client,
        &format!("{head}Schema {{ types: {{ \"a::B\": Struct {{ | }} }} }}"),
    );
    let fields = items.iter().find(|i| i["label"] == "fields").unwrap();
    assert_eq!(fields["detail"], "Map<String, Field>", "{fields}");
    client.stop();
}

#[test]
fn completion_names_the_types_of_a_schema_file() {
    let text = generated_schema();
    let dir = workspace("lsp-complete-type-paths", &[("crol/schema.crol", &text)]);
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "crol/schema.crol");
    let wanted = "server::generated::Limits";
    let named = "Named(\"server::generated::Limits\")";
    let root = "root: \"server::generated::Config\"";
    // In `Named("...")` and `root: "..."`, with the quotes written or not.
    for (written, replacement, inserted) in [
        (named, "Named(|)", "\"server::generated::Limits\""),
        (named, "Named(\"|\")", wanted),
        (named, "Named(\"ser|\")", wanted),
        (named, "Named(\"ser|", wanted),
        (root, "root: \"|\"", wanted),
    ] {
        assert!(text.contains(written), "{written}");
        let items = buffer.complete(&mut client, &text.replacen(written, replacement, 1));
        let found = items.iter().find(|i| i["label"] == wanted);
        let found = found.unwrap_or_else(|| panic!("{replacement}: {:?}", labels(&items)));
        assert_eq!(found["textEdit"]["newText"], inserted, "{replacement}");
        let range = &found["textEdit"]["range"];
        assert_eq!(
            range["start"]["line"], range["end"]["line"],
            "{replacement}"
        );
    }
    client.stop();
}

#[test]
fn completion_at_every_level_of_a_project_file() {
    let dir = workspace(
        "lsp-complete-project-file",
        &[("project.crol", GENERATED_PROJECT)],
    );
    let mut client = Client::start(&dir);
    let mut buffer = Buffer::open(&client, &dir, "project.crol");
    let cases: &[Case<'_>] = &[
        (
            "Project { | }",
            &["layers", "variants", "options", "schema", "files"],
            &[],
        ),
        ("Project { layers: | }", &["List", "Command"], &[]),
        (
            "Project { layers: List([{ | }]) }",
            &["name", "dir", "read_only"],
            &[],
        ),
        (
            "Project { layers: List([{ read_only: | }]) }",
            &["true", "false"],
            &[],
        ),
        ("Project { schema: | }", &["None", "Some"], &[]),
        (
            "Project { options: { | } }",
            &["implicit_some", "max_contexts"],
            &[],
        ),
        ("Project { variants: [|] }", &["Discrete", "Grouped"], &[]),
    ];
    check_completions(&mut client, &mut buffer, "", cases);
    client.stop();
}

#[test]
fn a_schema_file_is_checked_against_the_built_in_schema() {
    let text = generated_schema();
    let dir = workspace("lsp-schema-file-checked", &[("crol/schema.crol", &text)]);
    let client = Client::start(&dir);
    let file = uri(&dir.join("crol/schema.crol"));
    open_text(&client, &file, &text);
    client.diagnostics_for(&file, |d| d.is_empty());
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": file, "version": 2 },
            "contentChanges": [{ "text": text.replacen("source:", "sauce:", 1) }],
        }),
    );
    client.diagnostics_for(&file, |d| says(d, "has no field `sauce`"));
    client.stop();
}

// ---- A struct as the root type (`SPEC.md` 4.1, 12.5) ----

const STRUCT_ROOT_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Config",
    types: {
        "app::Config": Struct {
            name: "Config",
            doc: "The whole configuration.",
            source: ("src/lib.rs", 1),
            fields: {
                name: { ty: String, doc: "The name of the application.", .. },
                items: { ty: List(Named("app::Item")), doc: "What it holds.", .. },
                options: { ty: Named("app::Options"), doc: "How it behaves.", .. },
                retries: { ty: U32, default: Value(3), .. },
            },
            ..
        },
        "app::Item": Struct {
            name: "Item",
            doc: "One thing.",
            source: ("src/lib.rs", 2),
            fields: { id: { ty: U32, doc: "Its number.", .. } },
            ..
        },
        "app::Options": Struct {
            name: "Options",
            doc: "The options.",
            source: ("src/lib.rs", 3),
            fields: {
                fast: { ty: Bool, doc: "Whether to hurry.", .. },
                quiet: { ty: Bool, doc: "Whether to say nothing.", .. },
            },
            ..
        },
    },
}"#;

const STRUCT_ROOT_PROJECT: &str = r#"Project {
    layers: List([
        { name: "base", dir: "base", .. },
        { name: "local", dir: "local", .. },
    ]),
    schema: Some("schema"),
    ..
}"#;

fn struct_root_project(name: &str, base: &str, local: &str) -> PathBuf {
    workspace(
        name,
        &[
            ("project.crol", STRUCT_ROOT_PROJECT),
            ("schema.crol", STRUCT_ROOT_SCHEMA),
            ("base/a.crod", base),
            ("local/a.crod", local),
        ],
    )
}

#[test]
fn completion_at_a_struct_root_offers_its_fields() {
    let dir = struct_root_project(
        "lsp-struct-root-completion",
        "name: \"below\"\nitems: []\n",
        "options: { fast: true, quiet: true }\n",
    );
    let mut client = Client::start(&dir);
    let mut local = Buffer::open(&client, &dir, "local/a.crod");
    let fields: &[&str] = &["fast", "quiet"];
    let root: &[&str] = &["name", "items", "options", "retries"];
    let cases: &[Case<'_>] = &[
        ("|", root, fields),
        // Not what this document writes; what the layers below set stays.
        (
            "options: { .. }\n|",
            &["name", "items", "retries"],
            &["options"],
        ),
        ("name: \"x\"\n|", &["items", "options"], &["name"]),
        ("-name\n|", &["items"], &["name"]),
        // A template is no field, and does not count as written.
        ("t := { a: 1 }\n|", root, &["t"]),
        // At depth.
        ("options: { | }", fields, &["name", "items", "options"]),
        ("options: { fast: true, | }", &["quiet"], &["fast", "name"]),
        ("options.|", fields, &["name"]),
        ("items: [{ | }]", &["id"], &["name", "fast"]),
        ("<prod> { | }", root, fields),
        ("<prod> { options: { | } }", fields, &["name"]),
        ("{ | }", root, fields),
        ("options: { fast: |", &["true", "false"], &["name"]),
    ];
    check_completions(&mut client, &mut local, "", cases);

    // Accepting a field fills in the value below.
    let items = local.complete(&mut client, "|");
    let name = items.iter().find(|i| i["label"] == "name").unwrap();
    assert_eq!(name["textEdit"]["newText"], "name: \"below\"");
    assert_eq!(
        name["documentation"]["value"],
        "The name of the application."
    );
    client.stop();
}

#[test]
fn hover_at_a_struct_root_shows_the_field() {
    let text =
        "name: \"x\"\nitems: [{ id: 1 }]\noptions: { fast: true, quiet: true }\nretries: 1\n";
    let dir = struct_root_project("lsp-struct-root-hover", text, "");
    let mut client = Client::start(&dir);
    let mut base = Buffer::open(&client, &dir, "base/a.crod");
    let name = "The name of the application.\n\n`String`";
    let fast = "Whether to hurry.\n\n`bool`";
    let cases = [
        // On the key and the value.
        ("name: \"x\"", 1, name),
        ("name: \"x\"", 7, name),
        ("items:", 1, "What it holds.\n\n`Vec<Item>`"),
        ("options:", 1, "How it behaves.\n\n`Options`"),
        ("fast: true", 1, fast),
        ("fast: true", 7, fast),
        ("id: 1", 1, "Its number.\n\n`u32`"),
        ("id: 1", 4, "Its number.\n\n`u32`"),
        // A field with no doc shows its type.
        ("retries:", 1, "`u32`"),
    ];
    let mut failures = Vec::new();
    for (word, into, wanted) in cases {
        let i = text.find(word).unwrap() + into;
        let marked = format!("{}|{}", &text[..i], &text[i..]);
        let markdown = base.hover(&mut client, &marked);
        if markdown != wanted {
            failures.push(format!("{word}: wanted {wanted:?}, got {markdown:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    client.stop();
}

#[test]
fn diagnostics_at_a_struct_root_are_of_the_composed_root() {
    let dir = struct_root_project(
        "lsp-struct-root-diagnostics",
        "name: \"x\"\n",
        "items: []\nfoo: { bar: 2 }\n",
    );
    let client = Client::start(&dir);
    let base = uri(&dir.join("base/a.crod"));
    let local = uri(&dir.join("local/a.crod"));
    // The top document has what is wrong with the whole, and a lower one
    // nothing: it is no whole on its own.
    let found = client.diagnostics_for(&local, |d| says(d, "`Config` is missing the fields"));
    assert!(says(&found, "`Config` has no field `foo`"), "{found:?}");
    assert_eq!(found.len(), 2, "{found:?}");
    let text = std::fs::read_to_string(dir.join("base/a.crod")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": base, "languageId": "crol", "version": 1, "text": text } }),
    );
    let found = client.diagnostics_for(&base, |_| true);
    assert_eq!(found, Vec::<Value>::new());
    client.stop();
}

#[test]
fn code_actions_for_the_root_are_about_the_root() {
    let text = "name: \"x\"\noptions: { fast: true, quiet: true }\nnam: 1\n";
    let dir = struct_root_project("lsp-struct-root-actions", "", text);
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crod"));
    let found = client.diagnostics_for(&local, |d| says(d, "is missing the field"));
    let titles = |client: &mut Client, part: &str| -> Vec<(String, String)> {
        let diagnostic = found
            .iter()
            .find(|d| d["message"].as_str().unwrap().contains(part))
            .unwrap();
        let actions = client.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": local },
                "range": diagnostic["range"],
                "context": { "diagnostics": [diagnostic], "triggerKind": 2 },
            }),
        );
        actions
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                let edits = a["edit"]["changes"][&local].as_array().unwrap();
                (a["title"].as_str().unwrap().to_owned(), apply(text, edits))
            })
            .collect()
    };
    // Not on `options: { ... }`, the first block in the document.
    assert_eq!(
        titles(&mut client, "is missing the field"),
        [
            (
                "Add the missing fields".to_owned(),
                format!("{text}items: []\nretries: 0\n")
            ),
            (
                "End the document with `..`".to_owned(),
                format!("{text}..\n")
            ),
        ]
    );
    // A misspelt root key.
    assert_eq!(
        titles(&mut client, "has no field `nam`"),
        [(
            "Change to `name`".to_owned(),
            text.replace("nam: 1", "name: 1")
        )]
    );
    client.stop();
}

#[test]
fn a_complete_project_has_no_diagnostics_in_any_layer() {
    let dir = struct_root_project(
        "lsp-struct-root-complete",
        "name: \"x\"\nitems: []\n",
        "options: { fast: true, quiet: false }\n..\n",
    );
    let client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crod"));
    let found = client.diagnostics_for(&local, |_| true);
    assert_eq!(found, Vec::<Value>::new());
    client.stop();
}

#[test]
fn a_read_only_top_layer_passes_the_missing_field_to_a_writable_one() {
    let dir = workspace(
        "lsp-struct-root-read-only",
        &[
            (
                "project.crol",
                r#"Project {
    layers: List([
        { name: "base", dir: "base", .. },
        { name: "vendored", dir: "vendored", read_only: true },
    ]),
    schema: Some("schema"),
    ..
}"#,
            ),
            ("schema.crol", STRUCT_ROOT_SCHEMA),
            ("base/a.crod", "name: \"x\"\n"),
            ("vendored/a.crod", "items: []\n"),
        ],
    );
    let client = Client::start(&dir);
    let base = uri(&dir.join("base/a.crod"));
    client.diagnostics_for(&base, |d| says(d, "`Config` is missing the fields"));
    client.stop();
}

#[test]
fn a_standalone_crod_file_is_its_own_whole() {
    let dir = workspace(
        "lsp-struct-root-standalone",
        &[
            ("schema.crol", STRUCT_ROOT_SCHEMA),
            ("one.crod", "#![schema(::schema)]\n\nname: \"x\"\n"),
        ],
    );
    let mut client = Client::start(&dir);
    let file = uri(&dir.join("one.crod"));
    open_text(&client, &file, "#![schema(::schema)]\n\nname: \"x\"\n");
    client.diagnostics_for(&file, |d| says(d, "`Config` is missing the fields"));
    let mut one = Buffer {
        uri: file,
        version: 1,
    };
    let items = one.complete(&mut client, "#![schema(::schema)]\n\nname: \"x\"\n|");
    let found = labels(&items);
    assert!(found.contains(&"items"), "{found:?}");
    assert!(!found.contains(&"name"), "{found:?}");
    client.stop();
}

#[test]
fn an_enum_root_stays_the_type_of_each_root_value() {
    let schema = STRUCT_ROOT_SCHEMA
        .replace("root: \"app::Config\"", "root: \"app::Definition\"")
        .replace(
            "types: {",
            r#"types: {
        "app::Definition": Enum {
            name: "Definition",
            doc: "What the data defines.",
            source: ("src/lib.rs", 4),
            variants: { Options: { shape: Newtype(Named("app::Options")), .. } },
            ..
        },"#,
        );
    let dir = workspace(
        "lsp-enum-root",
        &[
            ("project.crol", STRUCT_ROOT_PROJECT),
            ("schema.crol", &schema),
            ("base/a.crod", "web: Options { fast: true, quiet: true }\n"),
            ("local/a.crod", ""),
        ],
    );
    let mut client = Client::start(&dir);
    let mut base = Buffer::open(&client, &dir, "base/a.crod");
    let cases: &[Case<'_>] = &[
        ("|", &[], &["name", "items", "options"]),
        ("web: Options { | }", &["fast", "quiet"], &["name"]),
        ("web: |", &["Options"], &["fast"]),
    ];
    check_completions(&mut client, &mut base, "", cases);
    let hover = base.hover(&mut client, "w|eb: Options { fast: true, quiet: true }\n");
    assert_eq!(hover, "The options.\n\n`Options`");
    client.stop();
}
