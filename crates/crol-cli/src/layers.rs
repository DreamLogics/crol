//! Layer arguments and evaluation, shared by `check` and `eval`.

use std::path::Path;

use crol::{CrolComposer, CrolConfig, LayerSource};
use crol_eval::{DataOptions, Evaluation, Layer, Sources, Variants};

use crate::diagnostics::Reporter;

/// The variant declarations, common to every command that evaluates
/// (`SPEC.md` 11).
#[derive(Debug, Clone, Default, clap::Args)]
pub(crate) struct VariantArgs {
    /// Declare discrete variants: `--discrete cache,debug`.
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    discrete: Vec<String>,

    /// Declare a group of alternatives: `--group dev,prod,prod_eu`.
    /// Repeatable.
    #[arg(long, value_name = "NAMES")]
    group: Vec<String>,

    /// Give a variant a parent: `--parent prod_eu=prod`. Repeatable.
    #[arg(long, value_name = "CHILD=PARENT")]
    parent: Vec<String>,
}

impl VariantArgs {
    /// The declared variants, or an error message.
    pub(crate) fn variants(&self) -> Result<Variants, String> {
        let mut variants = Variants::new();
        for group in &self.group {
            variants.add_grouped(group.split(','))?;
        }
        for name in &self.discrete {
            variants.add_discrete(name)?;
        }
        for pair in &self.parent {
            let (child, parent) = pair
                .split_once('=')
                .ok_or_else(|| format!("`--parent {pair}` is not `child=parent`"))?;
            variants.add_parent(child, parent)?;
        }
        Ok(variants)
    }
}

/// Whether `text` has the form of a module path (`SPEC.md` 4.2).
fn is_module_path(text: &str) -> bool {
    let rest = text.strip_prefix("::").unwrap_or(text);
    !rest.is_empty() && rest.split("::").all(crol_eval::is_identifier)
}

/// Whether a layer argument names something loadable.
pub(crate) fn names_a_layer(argument: &str, config: &CrolConfig) -> bool {
    let source = argument
        .split_once('=')
        .map_or(argument, |(_, source)| source);
    Path::new(source).exists() || is_module_path(source) && source.load(config).is_ok()
}

/// Loads the layers named by `arguments` (each `name=source` or a source)
/// and evaluates them in `contexts` (`None`: every valid context). Returns
/// `None` after reporting a loading error.
pub(crate) fn evaluate(
    arguments: &[String],
    variants: &VariantArgs,
    contexts: Option<Vec<Vec<String>>>,
    reporter: &mut Reporter,
) -> Option<(Sources, Evaluation)> {
    let declared = match variants.variants() {
        Ok(declared) => declared,
        Err(message) => {
            reporter.error(&message);
            return None;
        }
    };
    if arguments.is_empty() {
        reporter
            .error("no layers given; name them as arguments (project files are not supported yet)");
        return None;
    }
    // Tools always follow `extends` (`SPEC.md` 4.1).
    let config = CrolConfig {
        load_extends: true,
        ..CrolConfig::<String>::default()
    };
    let mut composer = CrolComposer::with_cfg(config.clone());
    for argument in arguments {
        let (name, source) = match argument.split_once('=') {
            Some((name, source)) if crol_eval::is_identifier(name) => (Some(name), source),
            _ => (None, argument.as_str()),
        };
        // The module path, if both name something (`SPEC.md` 11).
        let module = is_module_path(source).then(|| source.load(&config));
        let layer = match module {
            Some(Ok(layer)) => Ok(layer),
            Some(Err(error)) if !Path::new(source).exists() => Err(error),
            _ => Path::new(source).load(&config),
        };
        match layer {
            Ok(layer) => {
                let layer = match name {
                    Some(name) => layer.with_name(name),
                    None => layer,
                };
                composer = composer.load_layer(layer);
            }
            Err(error) => {
                reporter.error(&error.to_string());
                return None;
            }
        }
    }
    let mut sources = Sources::new();
    let layers: Vec<Layer> = composer
        .layers()
        .iter()
        .map(|layer| Layer {
            name: layer.name().to_owned(),
            documents: layer
                .documents()
                .map(|(relative, display, text)| (relative.to_owned(), sources.add(display, text)))
                .collect(),
        })
        .collect();
    let options = DataOptions::default()
        .with_variants(declared)
        .with_contexts(contexts);
    let evaluation = crol_eval::evaluate_data(&sources, &layers, &options);
    Some((sources, evaluation))
}
