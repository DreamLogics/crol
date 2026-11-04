//! `crol compile` (`SPEC.md` 7.8, 11): write a layer as a `.crob` file.

use std::path::{Path, PathBuf};

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layer: a directory or a `.crod` file.
    layer: PathBuf,

    /// Where to write it; by default beside the layer, as `<layer>.crob`.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

/// Returns whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let name = args.layer.file_stem().map_or_else(
        || "layer".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let compiled = crol::CrolLayer::from_path(name, &args.layer).and_then(|l| l.to_binary());
    let bytes = match compiled {
        Ok(bytes) => bytes,
        Err(error) => {
            reporter.error(&error.to_string());
            return false;
        }
    };
    let output = args.output.clone().unwrap_or_else(|| beside(&args.layer));
    if let Err(error) = std::fs::write(&output, bytes) {
        reporter.error(&format!("cannot write {}: {error}", output.display()));
        return false;
    }
    true
}

fn beside(layer: &Path) -> PathBuf {
    layer.with_extension("crob")
}
