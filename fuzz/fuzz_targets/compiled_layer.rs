//! A `.crob` file comes from elsewhere: whatever it holds, compiling and
//! evaluating it never panics or loops (`SPEC.md` 7.8). The input is what the
//! file says was lowered, past the hash, for a layer of one document.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let mut sources = crol_eval::Sources::new();
    let file = sources.add("a.crod", "+x: { a: 1, b: [2, 3] }\n+y: &root.x.a + 1\n");
    let layers = [crol_eval::Layer {
        name: "base".to_owned(),
        documents: vec![("a.crod".to_owned(), file)],
    }];
    let (compiled, _) = crol_eval::compile_binary(&sources, &layers, &[Some(bytes)]);
    if let Some(compiled) = compiled {
        let _ = crol_eval::evaluate_compiled(&compiled, &Default::default());
    }
});
