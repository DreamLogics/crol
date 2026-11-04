# crol-lsp

The language server for [CROL](https://github.com/DreamLogics/crol), as a
library (`SPEC.md` 12.5). `crol lsp` runs it on standard input and output;
an application can build its own server with its own checks:

```rust,no_run
let (connection, threads) = lsp_server::Connection::stdio();
let checks: crol_lsp::Checks = Box::new(|snapshot| {
    // Diagnostics of the application's own, from the evaluated data.
    let _ = snapshot;
    Vec::new()
});
crol_lsp::Server::start(connection, Some(checks)).unwrap().run().unwrap();
threads.join().unwrap();
```

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
