//! Constants the program gives (`SPEC.md` 6.1.4): `$name`, from the
//! composer to `update_constants`.

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolConfig, CrolData, CrolLayer, CrolVariants};
use crol_derive::Schema;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Schema)]
struct Limits {
    cpu: u32,
    memory_mb: u32,
}

#[derive(Debug, PartialEq, Deserialize, Schema)]
struct Service {
    replicas: u32,
    limits: Limits,
}

fn layer(name: &str, text: &str) -> CrolLayer {
    CrolLayer::from_sources(name, [("a.crod", text)])
}

fn limits() -> Limits {
    Limits {
        cpu: 2,
        memory_mb: 512,
    }
}

#[test]
fn a_constant_is_a_definition_or_a_patch() {
    let data = CrolComposer::new()
        .load_layer(layer("base", "count: $foo_count, other: 1"))
        .load_layer(layer("local", "count: & + $foo_count"))
        .with_constant("foo_count", 42)
        .unwrap()
        .compose_data()
        .unwrap();
    assert_eq!(data.get::<i64>("count").unwrap(), 84);
}

#[test]
fn a_struct_keeps_its_type_name() {
    let services: BTreeMap<String, Service> = CrolComposer::new()
        .with_constant("limits", limits())
        .unwrap()
        .load_layer(layer(
            "base",
            "+web: Service { replicas: 2, limits: Limits { ..$limits, cpu: 4 } },\n\
             +db: Service { replicas: 1, limits: $limits ~ { memory_mb: 1024 } }",
        ))
        .compose()
        .unwrap();
    assert_eq!(services["web"].limits.cpu, 4);
    assert_eq!(services["web"].limits.memory_mb, 512);
    assert_eq!(services["db"].limits.memory_mb, 1024);

    let error = CrolComposer::new()
        .with_constant("limits", limits())
        .unwrap()
        .load_layer(layer("base", "+web: Other { cpu: 1 }\nweb: & ~ $limits"))
        .compose_data()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot merge `Other` with `Limits`"),
        "{error}"
    );
}

#[test]
fn an_id_resolves_where_it_lands() {
    let data = CrolComposer::new()
        .with_constant("target", crol::Id::key("db"))
        .unwrap()
        .load_layer(layer("base", "+db: { port: 1 }, +web: { uses: $target }"))
        .compose_data()
        .unwrap();
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
    let data = CrolComposer::new()
        .with_constant("target", crol::Id::key("nothing"))
        .unwrap()
        .load_layer(layer("base", "+web: { uses: $target }"))
        .compose_data()
        .unwrap();
    assert_eq!(data.warnings().len(), 1, "{:?}", data.warnings());
}

#[test]
fn a_constant_not_given_is_missing() {
    let data = CrolComposer::new()
        .load_layer(layer("base", "replicas: $replicas ?? 1"))
        .compose_data()
        .unwrap();
    assert_eq!(data.get::<u32>("replicas").unwrap(), 1);

    let error = CrolComposer::new()
        .with_constant("foo_count", 1)
        .unwrap()
        .load_layer(layer("base", "count: $foo_cuont"))
        .compose_data()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no constant `foo_cuont` was given; did you mean `$foo_count`?"),
        "{error}"
    );
}

#[test]
fn with_constant_checks_what_it_is_given() {
    let message = |result: Result<CrolComposer, crol::Error>| result.unwrap_err().to_string();
    assert!(message(CrolComposer::new().with_constant("1x", 1)).contains("not an identifier"));
    assert!(
        message(
            CrolComposer::new()
                .with_constant("x", 1)
                .unwrap()
                .with_constant("x", 2)
        )
        .contains("given twice")
    );
    assert!(message(CrolComposer::new().with_constant("x", f64::NAN)).contains("finite"));
}

fn schema() -> crol::schema::SchemaFile {
    crol::schema::build::<BTreeMap<String, Service>>()
        .with_constant::<u32>("replicas")
        .with_constant::<Limits>("limits")
}

#[test]
fn the_schema_records_its_constants() {
    let schema = schema();
    let text = schema.to_text().unwrap();
    assert!(text.contains("constants:"), "{text}");
    assert_eq!(crol::schema::SchemaFile::from_text(&text).unwrap(), schema);
    // A schema without constants is written as it always was.
    let plain = crol::schema::build::<BTreeMap<String, Service>>().to_text().unwrap();
    assert!(!plain.contains("constants: {"), "{plain}");
}

#[test]
fn a_declared_constant_must_be_given_and_fit() {
    let text = "+web: Service { replicas: $replicas ?? 1, limits: $limits }";
    let error = CrolComposer::new()
        .schema(schema())
        .with_constant("limits", limits())
        .unwrap()
        .load_layer(layer("base", text))
        .compose_data()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "the schema declares constant `replicas`, which was not given"
    );

    let error = CrolComposer::new()
        .schema(schema())
        .with_constant("replicas", "three")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("constant `replicas` does not fit the schema: expected u32"),
        "{error}"
    );

    // A schema given after the constant checks it when composing.
    let error = CrolComposer::new()
        .with_constant("replicas", -1)
        .unwrap()
        .with_constant("limits", limits())
        .unwrap()
        .schema(schema())
        .load_layer(layer("base", text))
        .compose_data()
        .unwrap_err();
    assert!(error.to_string().contains("does not fit"), "{error}");

    let services: BTreeMap<String, Service> = CrolComposer::new()
        .schema(schema())
        .with_constant("replicas", 3)
        .unwrap()
        .with_constant("limits", limits())
        .unwrap()
        .load_layer(layer("base", text))
        .compose()
        .unwrap();
    assert_eq!(services["web"].replicas, 3);
}

const UPDATED: &str = "\
+a: { n: $count + 1 }
+b: { n: &root.a.n * 2 }
+c: { n: 7 }
+d: { n: $other ?? 0 }
<rebel> { c.n: $count }
";

fn composed(constants: &[(&str, i64)]) -> CrolData {
    let config = CrolConfig {
        variants: CrolVariants::new().add_grouped(["rebel", "empire"]),
        ..Default::default()
    };
    let mut composer = CrolComposer::with_cfg(config).load_layer(layer("base", UPDATED));
    for (name, value) in constants {
        composer = composer.with_constant(name, value).unwrap();
    }
    composer.compose_data().unwrap()
}

#[test]
fn updating_constants_gives_what_composing_again_gives() {
    let mut data = composed(&[("count", 1)]);
    let reloaded = data.update_constants().set("count", 5).unwrap().apply().unwrap();
    let fresh = composed(&[("count", 5)]);
    assert_eq!(data.fingerprint(), fresh.fingerprint());
    assert_eq!(data.key_fingerprints(), fresh.key_fingerprints());
    let changed: Vec<&str> = reloaded.changed.iter().map(|c| c.key()).collect();
    assert_eq!(changed, ["a", "b", "c"]);
    // `a` and `c` read it, and `b` reads `a`; `d` is left alone.
    assert_eq!(reloaded.evaluated, Some(3));
    let rebel = data.context(["rebel"]).unwrap();
    let c: BTreeMap<String, i64> = data.get_in("c", &rebel).unwrap();
    assert_eq!(c["n"], 5);

    // The same value again changes nothing.
    let reloaded = data.update_constants().set("count", 5).unwrap().apply().unwrap();
    assert_eq!(reloaded.changed, []);
    assert_eq!(reloaded.evaluated, Some(0));

    // A new constant, and one taken away.
    data.update_constants()
        .set("other", 9)
        .unwrap()
        .remove("count")
        .unwrap()
        .set("count", 2)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(data.fingerprint(), composed(&[("count", 2), ("other", 9)]).fingerprint());
    data.update_constants().remove("other").unwrap().apply().unwrap();
    assert_eq!(data.fingerprint(), composed(&[("count", 2)]).fingerprint());
}

#[test]
fn a_failed_update_keeps_the_data() {
    let mut data = composed(&[("count", 1)]);
    let before = data.fingerprint();
    let error = data.update_constants().remove("count").unwrap().apply().unwrap_err();
    assert!(error.to_string().contains("no constant `count` was given"), "{error}");
    assert_eq!(data.fingerprint(), before);
    assert_eq!(data.get::<BTreeMap<String, i64>>("a").unwrap()["n"], 2);
}

#[test]
fn reloading_documents_keeps_the_constants() {
    let mut data = composed(&[("count", 1)]);
    data.reload(&[crol::DocumentChange {
        layer: "base",
        path: "a.crod",
        text: Some("+a: { n: $count + 10 }"),
    }])
    .unwrap();
    assert_eq!(data.get::<BTreeMap<String, i64>>("a").unwrap()["n"], 11);
}

#[test]
fn a_declared_constant_cannot_be_removed() {
    let mut data = CrolComposer::new()
        .schema(schema())
        .with_constant("replicas", 3)
        .unwrap()
        .with_constant("limits", limits())
        .unwrap()
        .load_layer(layer("base", "+web: Service { replicas: $replicas, limits: $limits }"))
        .compose_data()
        .unwrap();
    let error = data.update_constants().remove("replicas").err().unwrap();
    assert!(error.to_string().contains("declared in the schema"), "{error}");
    let error = data.update_constants().set("replicas", "x").err().unwrap();
    assert!(error.to_string().contains("does not fit"), "{error}");
    let reloaded = data.update_constants().set("replicas", 4).unwrap().apply().unwrap();
    assert_eq!(reloaded.evaluated, Some(1));
}

#[test]
fn value_mode_reads_constants_through_a_reader() {
    #[derive(Debug, Deserialize)]
    struct Config {
        dir: String,
        workers: u32,
        limits: Limits,
    }
    let config: Config = crol::CrolReader::new()
        .with_constant("home", "/home/me")
        .unwrap()
        .with_constant("limits", limits())
        .unwrap()
        .from_str(r#"dir: $home + "/data", workers: $workers ?? 4, limits: $limits ~ { cpu: 8 }"#)
        .unwrap();
    assert_eq!(config.dir, "/home/me/data");
    assert_eq!(config.workers, 4);
    assert_eq!(config.limits.cpu, 8);

    // The shorthands give no constants.
    let error = crol::from_str::<BTreeMap<String, u32>>("a: $home").unwrap_err();
    assert!(error.to_string().contains("no constant `home` was given"), "{error}");
    let workers: BTreeMap<String, u32> = crol::from_str("a: $workers ?? 2").unwrap();
    assert_eq!(workers["a"], 2);

    // A declared constant must be given, whatever `??` says.
    let error = crol::CrolReader::new()
        .schema(schema())
        .with_constant("limits", limits())
        .unwrap()
        .from_str::<BTreeMap<String, u32>>("a: $replicas ?? 1")
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "the schema declares constant `replicas`, which was not given"
    );
}

#[test]
fn a_compiled_layer_keeps_constants_by_name() {
    let text = layer("base", "count: $count * 2, other: $other ?? 0");
    let bytes = text.to_binary().unwrap();
    for count in [1, 7] {
        let compiled = CrolLayer::from_binary("base", &bytes).unwrap();
        let from_bytes = CrolComposer::new()
            .load_layer(compiled)
            .with_constant("count", count)
            .unwrap()
            .compose_data()
            .unwrap();
        let from_text = CrolComposer::new()
            .load_layer(text.clone())
            .with_constant("count", count)
            .unwrap()
            .compose_data()
            .unwrap();
        assert_eq!(from_bytes.get::<i64>("count").unwrap(), count * 2);
        assert_eq!(from_bytes.fingerprint(), from_text.fingerprint());
    }
}
