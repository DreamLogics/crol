//! The performance target of `SPEC.md` 12.3: 10 000 root keys and 16
//! contexts, evaluated in under 200 ms. Run with
//! `cargo bench -p crol-eval --bench big`.

#![expect(clippy::print_stdout, reason = "a benchmark prints its timings")]

use std::time::Instant;

use crol_eval::{DataOptions, Layer, Sources, Variants, evaluate_data};

fn main() {
    let keys = 10_000;
    let mut base = String::from(
        "service := { replicas: 1, port: 8080, tags: [\"managed\"], env: { LOG_LEVEL: \"info\" }, url: \"http://x:\" + &self.port },\n",
    );
    for i in 0..keys {
        base.push_str(&format!(
            "+s{i}: Service {{ ..&service, image: \"img{i}\", depends_on: [@s{}], .. }},\n",
            (i + 1) % keys
        ));
    }
    let mut local = String::new();
    for i in (0..keys).step_by(10) {
        local.push_str(&format!(
            "s{i}<prod>.replicas: & * 4,\ns{i}<cache>.env.CACHE: \"redis\",\n"
        ));
    }
    let mut sources = Sources::new();
    let layers = vec![
        Layer {
            name: "base".into(),
            documents: vec![("a.crol".into(), sources.add("base.crol", base))],
        },
        Layer {
            name: "local".into(),
            documents: vec![("a.crol".into(), sources.add("local.crol", local))],
        },
    ];
    let mut variants = Variants::new();
    variants.add_grouped(["dev", "prod", "prod_eu"]).unwrap();
    variants.add_discrete("cache").unwrap();
    variants.add_discrete("debug").unwrap();
    variants.add_parent("prod_eu", "prod").unwrap();
    let options = DataOptions::default().with_variants(variants);
    for _ in 0..3 {
        let start = Instant::now();
        let evaluation = evaluate_data(&sources, &layers, &options);
        let elapsed = start.elapsed();
        assert!(
            evaluation.value.is_some(),
            "{:?}",
            evaluation.diagnostics.first()
        );
        let contexts = evaluation.contexts.len();
        println!("{keys} keys, {} contexts: {elapsed:?}", contexts + 1);
    }
}
