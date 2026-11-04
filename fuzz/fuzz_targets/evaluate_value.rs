//! Evaluating a value-mode document never panics or loops (`SPEC.md`
//! 14.2): the limits of section 7.7 stop it.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let mut sources = crol_eval::Sources::new();
    let file = sources.add("fuzz.crov", text);
    let evaluation = crol_eval::evaluate_value(&sources, file);
    if let Some(value) = &evaluation.value {
        let _ = crol_eval::print::to_document(value, &Default::default());
    }
});
