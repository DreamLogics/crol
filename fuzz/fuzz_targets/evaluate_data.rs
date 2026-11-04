//! Evaluating layers of data-mode documents never panics or loops
//! (`SPEC.md` 14.2). The input is split at `\0` into documents, each in a
//! layer of its own, so that chains, templates and variants cross layers.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let mut sources = crol_eval::Sources::new();
    let layers: Vec<crol_eval::Layer> = text
        .split('\0')
        .take(4)
        .enumerate()
        .map(|(i, document)| crol_eval::Layer {
            name: format!("layer{i}"),
            documents: vec![(
                "a.crol".to_owned(),
                sources.add(format!("layer{i}/a.crol"), document),
            )],
        })
        .collect();
    let options = crol_eval::DataOptions::default();
    let evaluation = crol_eval::evaluate_data(&sources, &layers, &options);
    if let Some(value) = &evaluation.value {
        let _ = crol_eval::print::to_all_contexts(
            value,
            &evaluation.contexts,
            &evaluation.variants,
            &Default::default(),
        );
        let _ =
            crol_eval::fingerprint::fingerprint(value, &evaluation.contexts, &evaluation.variants);
    }
});
