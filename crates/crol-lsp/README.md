# crol-lsp

The language server of [CROL](https://github.com/DreamLogics/crol), the Composable
Rust Object Language, as a library. To use the server, you do not need this crate:
run `crol lsp`, from [`crol-cli`](https://crates.io/crates/crol-cli), and see
[editor setup](https://github.com/DreamLogics/crol/blob/main/docs/editors.md).

Use the library to build a server of your own, with checks that only your
application can do. They receive the evaluated data, and their diagnostics are
published with the others:

```rust,no_run
fn main() {
    let (connection, threads) = lsp_server::Connection::stdio();
    let checks: crol_lsp::Checks = Box::new(|snapshot| {
        // Diagnostics of the application's own, from the evaluated data.
        let _ = snapshot;
        Vec::new()
    });
    crol_lsp::Server::start(connection, Some(checks)).unwrap().run().unwrap();
    threads.join().unwrap();
}
```

The `remote-schemas` feature, on by default, fetches schemas named by URL or
crate, in the background.

## Documentation

- [Editor setup](https://github.com/DreamLogics/crol/blob/main/docs/editors.md)
- [Schemas](https://github.com/DreamLogics/crol/blob/main/docs/schemas.md)

## Licence

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
