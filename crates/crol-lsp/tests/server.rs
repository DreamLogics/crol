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
                "initializationOptions": { "debounce_ms": 10 },
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
        "+web: Service { port: 80, limits: { cpu: 1 } },\nweb.port: 443,\n"
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
