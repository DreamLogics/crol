//! `crol eval` (`SPEC.md` 11): print the evaluated data as a data-mode
//! document, or one value.

use std::io::Write;

use crol::CrolConfig;
use crol_eval::{Key, Value, ValueKind, print};
use crol_syntax::ast;
use crol_syntax::format::FormatConfig;

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first, then optionally a key and path to print
    /// only that value: `web.limits.cpu`.
    #[arg(value_name = "LAYER... [KEY[.PATH]]")]
    arguments: Vec<String>,

    /// The maximum line width of the output.
    #[arg(long, default_value_t = FormatConfig::default().max_width)]
    max_width: usize,
}

/// Returns whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let mut layers = args.arguments.clone();
    // The last argument is a key path if it names no layer.
    let path = match layers.last() {
        Some(last)
            if layers.len() > 1 && !crate::layers::names_a_layer(last, &CrolConfig::default()) =>
        {
            layers.pop()
        }
        _ => None,
    };
    let Some((sources, evaluation)) = crate::layers::evaluate(&layers, reporter) else {
        return false;
    };
    reporter.diagnostics(&sources, &evaluation.diagnostics);
    let Some(root) = evaluation.value else {
        return false;
    };
    let config = FormatConfig::default().with_max_width(args.max_width);
    let output = match &path {
        None => print::to_document(&root, &config),
        Some(path) => match select(&root, path) {
            Ok(value) => print::to_document(value, &config),
            Err(message) => {
                reporter.error(&message);
                return false;
            }
        },
    };
    if let Err(error) = std::io::stdout().lock().write_all(output.as_bytes()) {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}

/// The value at `path` (`web.limits.cpu`, `web.tags[0]`) in the root.
fn select<'v>(root: &'v Value, path: &str) -> Result<&'v Value, String> {
    let parse = crol_syntax::parse(&format!("{path}: ()"));
    let target = match parse.document().entries().next() {
        Some(ast::AnyEntry::Entry(entry)) if parse.errors().is_empty() => entry.target(),
        _ => None,
    };
    let Some(target) = target else {
        return Err(format!("`{path}` is not a key or a path"));
    };
    let missing = || format!("there is no value at `{path}`");
    let key = target.key().and_then(|k| k.token()).ok_or_else(missing)?;
    let key = match key.kind() {
        crol_syntax::SyntaxKind::IDENT => key.text().to_owned(),
        _ => key.text().trim_matches('"').to_owned(),
    };
    let mut value = entry(root, &Key::Str(key)).ok_or_else(missing)?;
    for selector in target.selectors() {
        value = match &selector {
            ast::Selector::Field(field) => {
                let name = field.name().ok_or_else(missing)?;
                entry(value, &Key::Str(name.text().to_owned()))
            }
            ast::Selector::Index(index) => {
                let i: usize = index
                    .index()
                    .ok_or_else(missing)?
                    .text()
                    .parse()
                    .map_err(|_| missing())?;
                element(value, i)
            }
            ast::Selector::Accessor(accessor) => {
                let token = accessor.key().ok_or_else(missing)?;
                match token.text().parse::<i128>() {
                    Ok(i) => {
                        let i = if accessor.is_negative() { -i } else { i };
                        entry(value, &Key::Int(i))
                            .or_else(|| element(value, usize::try_from(i).ok()?))
                    }
                    Err(_) => entry(value, &Key::Str(token.text().trim_matches('"').to_owned())),
                }
            }
            _ => None,
        }
        .ok_or_else(missing)?;
    }
    Ok(value)
}

fn entry<'v>(value: &'v Value, key: &Key) -> Option<&'v Value> {
    match &value.kind {
        ValueKind::Keyed(keyed) => keyed.entries.get(key),
        _ => None,
    }
}

fn element(value: &Value, index: usize) -> Option<&Value> {
    match &value.kind {
        ValueKind::List(e) | ValueKind::Tuple(e) | ValueKind::NamedTuple(_, e) => e.get(index),
        _ => None,
    }
}
