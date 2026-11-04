//! A load the size and shape of a real game's data, for measuring where time
//! and memory go: thousands of root keys of deep blocks over many documents,
//! civilisation-like variants in a group, a mod layer of patches, and
//! definitions that spread others. It writes its own data, so that it needs no
//! game files. Run with `cargo bench -p crol-eval --bench game`.
//!
//! Peak memory is the process's, from `/proc/self/status`, so it is printed
//! on Linux only, and it only grows: each line is the peak so far.

#![expect(clippy::print_stdout, reason = "a benchmark prints its timings")]

use std::fmt::Write as _;
use std::time::Instant;

use crol_eval::{
    DataOptions, Layer, Sources, Variants, binary, compile, compile_binary, encode_layer,
    evaluate_compiled, evaluate_contexts, fingerprint,
};

const KEYS: usize = 14_000;
const KEYS_PER_DOCUMENT: usize = 5;
const CIVS: [&str; 9] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
];

/// One definition: one in three is a unit with deep blocks, and the rest are
/// small, with long text, which makes the mix of a converted game's data.
fn definition(out: &mut String, i: usize) {
    if !i.is_multiple_of(3) {
        let help = "Upgrades your units, which are stronger and cost more. ".repeat(20);
        let _ = writeln!(
            out,
            "+k{i}: Graphic {{ file: \"graphic{i}\", help: \"{help}\", frames: {}, \
             angles: 8, layer: 20, speed: 0.5, sound: @sound{}, original: {{ id: {i} }} }},",
            i % 60,
            i % 90,
        );
        return;
    }
    let parent = (i % 51 == 48).then(|| format!("..&root.k{}, ", i - 3));
    let parent = parent.unwrap_or_default();
    let _ = writeln!(
        out,
        "+k{i}: Unit {{ {parent}name: \"Unit {i}\", kind: Trainable, class: @class{}, \
         tags: [\"heavy\", \"machine\", \"repairable\"], hit_points: {}, line_of_sight: 11.0, \
         speed: 0.5, collision_size: (0.5, 0.5, 2.0), \
         graphics: {{ dying: @effect{}, icon: {}, standing: @graphic{i} }}, \
         placement: {{ obstruction: Round, obstruction_class: 2 }}, \
         presentation: {{ ui_group: Unit, minimap_mode: 1, sort_number: 5, \
         selection_outline_size: (0.5, 0.5, 2.0) }}, \
         behaviour: {{ interaction_mode: 4, combat_level: 4, blast_defense: TargetOnly, \
         dead_unit: @k{} }}, \
         resources: {{ decay: 10.0, storages: [{{ resource: @population, amount: 1 }}, \
         {{ resource: @carbon, amount: 2 }}] }}, \
         costs: [{{ resource: @ore, amount: {} }}, {{ resource: @nova, amount: 30 }}], \
         tasks: [{{ kind: Attack, wait: 2 }}, {{ kind: Move, wait: 1 }}, \
         {{ kind: Garrison, wait: 3 }}], \
         original: {{ id: {i}, internal_name: \"UNIT-{i}\", \
         strings: {{ name: {}, help: {} }} }} }},",
        i % 40,
        100 + i % 900,
        i % 30,
        i % 200,
        i / 2,
        50 + i % 200,
        7000 + i,
        28000 + i,
    );
}

fn base_layer(sources: &mut Sources) -> Layer {
    let mut documents = Vec::new();
    for start in (0..KEYS).step_by(KEYS_PER_DOCUMENT) {
        let mut text = String::new();
        for i in start..(start + KEYS_PER_DOCUMENT).min(KEYS) {
            definition(&mut text, i);
        }
        let name = format!("units/{start:06}.crod");
        let file = sources.add(name.clone(), text);
        documents.push((name, file));
    }
    // Each civilisation patches its share of the keys, in a document of its
    // own beside each group of them.
    for (c, civ) in CIVS.iter().enumerate() {
        for start in (0..KEYS).step_by(KEYS_PER_DOCUMENT * 40) {
            let mut text = format!("<{civ}> {{\n");
            for i in (start..start + KEYS_PER_DOCUMENT * 40).step_by(30) {
                let i = (i + 3 * c) % KEYS / 3 * 3;
                let _ = writeln!(
                    text,
                    "    k{i}: & ~ Unit {{ graphics: {{ standing: @civ{c}_{i}, icon: {c} }}, \
                     speed: 0.6 }},"
                );
            }
            text.push_str("}\n");
            let name = format!("units/{civ}/{start:06}.crod");
            let file = sources.add(name.clone(), text);
            documents.push((name, file));
        }
    }
    Layer {
        name: "base".into(),
        documents,
    }
}

fn mod_layer(sources: &mut Sources) -> Layer {
    let mut text = String::new();
    for i in (0..KEYS).step_by(15).map(|i| i / 3 * 3) {
        let _ = writeln!(
            text,
            "k{i}: & ~ Unit {{ line_of_sight: 6.0, behaviour: {{ combat_level: 5 }}, \
             tags: & + [\"mech\"], hit_points: & + 40 }},"
        );
    }
    let file = sources.add("mod.crod", text);
    Layer {
        name: "mod".into(),
        documents: vec![("mod.crod".into(), file)],
    }
}

/// The peak memory so far, in MB; 0 where there is no `/proc`.
fn peak() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find(|line| line.starts_with("VmHWM"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

fn main() {
    let mut sources = Sources::new();
    let layers = vec![base_layer(&mut sources), mod_layer(&mut sources)];
    let bytes: usize = sources.files().map(|(_, file)| file.text().len()).sum();
    println!(
        "{KEYS} keys, {} MB of text, {} documents",
        bytes >> 20,
        sources.files().count()
    );
    println!("peak {} MB before compiling", peak());

    let start = Instant::now();
    let (compiled, diagnostics) = compile(&sources, &layers);
    println!("compile: {:?}, peak {} MB", start.elapsed(), peak());
    let compiled = compiled.unwrap_or_else(|| panic!("{:?}", diagnostics.first()));

    let mut variants = Variants::new();
    variants.add_grouped(CIVS).unwrap();
    let options = DataOptions::default()
        .with_variants(variants)
        .with_warn_cross_file_patches(false);
    let start = Instant::now();
    let evaluation = evaluate_compiled(&compiled, &options);
    println!(
        "evaluate, {} contexts: {:?}, peak {} MB",
        evaluation.contexts.len() + 1,
        start.elapsed(),
        peak()
    );
    assert!(
        !evaluation.has_errors(),
        "{:?}",
        evaluation.diagnostics.first()
    );
    let base = evaluation.value.as_ref().unwrap();

    let start = Instant::now();
    let names = vec![vec![CIVS[0].to_owned()]];
    let later = options.clone().with_contexts(Some(names));
    let again = evaluate_contexts(&compiled, &later, base);
    println!("one context later: {:?}", start.elapsed());
    assert_eq!(again.contexts.len(), 1);

    let hash = fingerprint::fingerprint(base, &evaluation.contexts, &evaluation.variants);
    let hex: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    println!("fingerprint {hex}");

    // The base layer as a `.crob` file, and the mod over it as text.
    let start = Instant::now();
    let bytes = encode_layer(&sources, &layers[0]).expect("the base layer compiles");
    println!(
        "encode the base layer: {:?}, {} MB",
        start.elapsed(),
        bytes.len() >> 20
    );
    let start = Instant::now();
    let read = binary::read_layer(&bytes).expect("it reads back");
    let read_time = start.elapsed();
    let start = Instant::now();
    let (compiled, diagnostics) = compile_binary(&sources, &layers, &[Some(&read.lowered), None]);
    println!(
        "compile from it: {read_time:?} to read, {:?} to link",
        start.elapsed()
    );
    let compiled = compiled.unwrap_or_else(|| panic!("{:?}", diagnostics.first()));
    let again = evaluate_compiled(&compiled, &options);
    let same = fingerprint::fingerprint(
        again.value.as_ref().expect("it evaluates"),
        &again.contexts,
        &again.variants,
    );
    assert_eq!(same, hash, "a compiled layer evaluates the same");
}
