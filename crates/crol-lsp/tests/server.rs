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
                "base/a.crol",
                "t := { port: 80 },\n+web: Service { ..&t, url: \"x:\" + &self.port, .. },\n+db: { host: \"db\" },",
            ),
            (
                "local/a.crol",
                "web.port: 8080,\nmirror: &root.db.host,\nlink: @web,",
            ),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crol"));
    let text = std::fs::read_to_string(dir.join("local/a.crol")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": text } }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());

    // On a target: the final value and its history.
    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": local }, "position": { "line": 0, "character": 5 } }),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.starts_with("**web.port**: `8080`"), "{markdown}");
    assert!(markdown.contains("`+web` (definition"), "{markdown}");
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
    assert_eq!(location["uri"], uri(&dir.join("base/a.crol")));
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
                "base/a.crol",
                "t := 80,\n+web: Service { p = port: &t, url: \"x:\" + &p, admin: { u: &p }, .. },",
            ),
            (
                "local/a.crol",
                "web.port: 8080,\nother: &t,\nlink: @web,\nm: &root.web.url,",
            ),
        ],
    );
    let mut client = Client::start(&dir);
    let base = uri(&dir.join("base/a.crol"));
    let local = uri(&dir.join("local/a.crol"));
    let text = std::fs::read_to_string(dir.join("local/a.crol")).unwrap();
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
                "base/a.crol",
                "+web: { replicas: 2, port: 8000 + 80, max: &self.replicas * 4, name: \"web\" },",
            ),
            ("local/a.crol", "web.replicas: & * 3,"),
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
    let base = uri(&dir.join("base/a.crol"));
    let local = uri(&dir.join("local/a.crol"));

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
                    root: "app::Service",
                    types: {
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
                "base/a.crol",
                "+web: Service { replicas: 2, port: 80, limits: Limits { cpu: 1.0, memory_mb: 512 } },",
            ),
            ("local/a.crol", ""),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crol"));
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
                "base/a.crol",
                "+web: Service { replicas: 2, port: 80, limits: Limits { cpu: 1.0, memory_mb: 512 }, mode: Fast, url: None, depends_on: [] },\n+data: Volume { size_gb: 10 },",
            ),
            ("local/a.crol", "web.port: 81,"),
        ],
    );
    let mut client = Client::start(&dir);
    let base = uri(&dir.join("base/a.crol"));
    let local = uri(&dir.join("local/a.crol"));
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
    assert!(
        markdown.contains("`u32` — Number of copies to run."),
        "{markdown}"
    );
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
fn the_project_file_is_checked_against_its_schema() {
    let project = r#"Project { layers: List([{ name: "base", dir: "base", .. }]), .. }"#;
    let dir = workspace(
        "lsp-project-file",
        &[("project.crol", project), ("base/a.crol", "+web: 1,")],
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
                "base/a.crol",
                "+web: { replicas: 2, limits: { cpu: 1 } },\nweb.limits: & ~ { memory: 2 },",
            ),
            (
                "local/a.crol",
                "web.replicas: & + 1,\nweb: & ~ { limits: { cpu: & * 2 } },",
            ),
        ],
    );
    let mut client = Client::start(&dir);
    let local = uri(&dir.join("local/a.crol"));
    let text = std::fs::read_to_string(dir.join("local/a.crol")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": local, "languageId": "crol", "version": 1, "text": text } }),
    );
    client.diagnostics_for(&local, |d| d.is_empty());
    let base = uri(&dir.join("base/a.crol"));
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
            ("base/a.crol", "#![schema(super::other)]\n+a: 1,"),
        ],
    );
    let client = Client::start(&dir);
    let a = uri(&dir.join("base/a.crol"));
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
            ("base/a.crol", "+a: 1,"),
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
    assert!(markdown.contains("`String` — Where to listen."), "{hover}");

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
