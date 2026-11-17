//! Constants the program gives, `$name` (`SPEC.md` 6.1.4), in the editor:
//! what the schema declares of them, and the project's stand-in values
//! (`SPEC.md` 12.4).

use crol::CheckOptions;
use crol::schema::SchemaFile;
use crol_eval::{Constants, Diagnostic, FileId, Span, Value};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{SyntaxKind, SyntaxNode, TextRange};

/// The start of the message the evaluator gives a `$name` that is not
/// given, where tools ask for a warning.
const ABSENT: &str = "no constant `";

/// With a schema that declares constants, a warning on each `$name` in
/// `root` that it does not declare (`SPEC.md` 6.1.4): the program may not
/// give it.
pub(crate) fn undeclared(file: FileId, root: &SyntaxNode, schema: &SchemaFile) -> Vec<Diagnostic> {
    if schema.constants.is_empty() {
        return Vec::new();
    }
    root.descendants()
        .filter_map(ast::Constant::cast)
        .filter_map(|constant| {
            let name = constant.name()?;
            (!schema.constants.contains_key(name.text())).then(|| {
                Diagnostic::warning(
                    Span::new(file, constant.syntax().text_range()),
                    format!(
                        "constant `{}` is not in the schema; it is missing unless the program gives it",
                        name.text()
                    ),
                )
            })
        })
        .collect()
}

/// Drops the evaluator's warnings that a constant was not given where
/// `undeclared` already says more of the same `$name`.
pub(crate) fn drop_absent(diagnostics: &mut Vec<Diagnostic>, undeclared: &[Diagnostic]) {
    if undeclared.is_empty() {
        return;
    }
    diagnostics.retain(|d| {
        d.is_error()
            || !d.message.starts_with(ABSENT)
            || !undeclared.iter().any(|u| u.span == d.span)
    });
}

/// What is wrong with the project's stand-in values against what `schema`
/// declares: a value that does not fit its type is an error at its name in
/// the project file. A declared constant without a stand-in value is a
/// warning where it is used, which the evaluator gives, not an error here.
pub(crate) fn stand_in_problems(
    project_file: FileId,
    project_text: &str,
    constants: &Constants,
    schema: &SchemaFile,
) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    for (name, declared) in &schema.constants {
        let Some(value) = constants.get(name) else {
            continue;
        };
        let problems = crol::check_constant(value, schema, &declared.ty, &CheckOptions::default());
        if let Some(problem) = problems.iter().find(|d| d.is_error()) {
            let range = stand_in_range(project_text, name).unwrap_or_default();
            found.push(Diagnostic::error(
                Span::new(project_file, range),
                format!(
                    "constant `{name}`: the stand-in value does not fit the schema: {}",
                    problem.message
                ),
            ));
        }
    }
    found
}

/// Where a project file gives `name` its stand-in value: the key in its
/// `constants`, else `constants` itself.
fn stand_in_range(text: &str, name: &str) -> Option<TextRange> {
    let root = crol_syntax::parse(text).syntax();
    let key_of = |entry: &ast::Entry| {
        let token = entry.target()?.key()?.token()?;
        Some((
            token.text().trim_matches('"').to_owned(),
            token.text_range(),
        ))
    };
    let entry = root
        .descendants()
        .filter_map(ast::Entry::cast)
        .find(|e| key_of(e).is_some_and(|(key, _)| key == "constants"))?;
    let own = entry.value().and_then(|value| {
        value
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ENTRY)
            .filter_map(ast::Entry::cast)
            .find_map(|e| key_of(&e).filter(|(key, _)| key == name))
    });
    own.or_else(|| key_of(&entry)).map(|(_, range)| range)
}

/// What hover says of a constant (`SPEC.md` 12.5): its name and type, then
/// [`details`].
pub(crate) fn about(name: &str, schema: Option<&SchemaFile>, constants: &Constants) -> String {
    let code = crate::features::code;
    let mut head = format!("constant {}", code(&format!("${name}")));
    if let Some((schema, def)) = declared(name, schema) {
        let ty = crate::schema::rust_type(schema, &def.ty);
        head.push_str(&format!(": {}", code(&ty)));
    }
    let mut parts = vec![head];
    parts.extend(details(name, schema, constants));
    parts.join("\n\n")
}

/// What is known of a constant past its type, in Markdown paragraphs: its
/// doc from the schema, its stand-in value from the project, and whether
/// the schema leaves it out.
pub(crate) fn details(
    name: &str,
    schema: Option<&SchemaFile>,
    constants: &Constants,
) -> Vec<String> {
    let def = declared(name, schema);
    let mut parts = Vec::new();
    if let Some((_, def)) = def
        && !def.doc.trim().is_empty()
    {
        parts.push(def.doc.trim().to_owned());
    }
    parts.push(match constants.get(name) {
        Some(value) => stand_in(value),
        None => "No stand-in value: the project's `constants` give none.".to_owned(),
    });
    if def.is_none() && schema.is_some_and(|s| !s.constants.is_empty()) {
        parts.push("Not in the schema: it is missing unless the program gives it.".to_owned());
    }
    parts
}

/// The declaration of a constant in a schema.
fn declared<'a>(
    name: &str,
    schema: Option<&'a SchemaFile>,
) -> Option<(&'a SchemaFile, &'a crol::schema::ConstantDef)> {
    let schema = schema?;
    Some((schema, schema.constants.get(name)?))
}

/// A stand-in value, in Markdown: inline if it is short, else a block.
fn stand_in(value: &Value) -> String {
    let text = crol_eval::print::to_text(value);
    if text.contains('\n') || text.len() > MAX_INLINE {
        format!("Stand-in value:\n\n```crol\n{text}\n```")
    } else {
        format!("Stand-in value: {}", crate::features::code(&text))
    }
}

/// The longest stand-in value written inline.
const MAX_INLINE: usize = 60;
