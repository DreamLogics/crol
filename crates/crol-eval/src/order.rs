//! The order of a layer's documents (`SPEC.md` 7.1): by folder, a folder's
//! own documents before those of its subfolders, and among the documents,
//! or the subfolders, of one folder as their `#![extends(...)]` say, then
//! by name. Which documents this puts in order, and not only by name,
//! decides whether their patches to one key may meet (`SPEC.md` 7.2).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crol_syntax::ast;

use crate::Layer;
use crate::diagnostic::Diagnostic;
use crate::source::{FileId, Sources, Span};

/// What an `extends` names in the layer of the document it is written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerTarget {
    /// A document, by its path in the layer.
    Document(String),
    /// A folder, by its path in the layer, without a final `/`.
    Folder(String),
}

/// What `module_path`, written in the document at `from` (a path in the
/// layer), names among `paths`, the paths of the layer's documents: `None`
/// if it starts from the root directory, goes above the layer, or names
/// none of them (`SPEC.md` 4.1). Such an `extends` orders the layer's
/// documents; any other is a layer below.
pub fn layer_target<'p>(
    paths: impl IntoIterator<Item = &'p str>,
    from: &str,
    module_path: &str,
) -> Option<LayerTarget> {
    let mut folder: Vec<&str> = from.split('/').collect();
    folder.pop();
    let mut names = Vec::new();
    for (i, part) in module_path.split("::").map(str::trim).enumerate() {
        match part {
            // A leading `::` starts from the root directory.
            "" if i == 0 => return None,
            "super" if names.is_empty() => {
                folder.pop()?;
            }
            name => names.push(name),
        }
    }
    if names.is_empty() {
        return None;
    }
    folder.extend(names);
    let folder = folder.join("/");
    let document = format!("{folder}.crod");
    let below = format!("{folder}/");
    let mut found = None;
    for path in paths {
        if path == document {
            return Some(LayerTarget::Document(document));
        }
        if path.starts_with(&below) {
            found = Some(LayerTarget::Folder(folder.clone()));
        }
    }
    found
}

/// A document or a subfolder of a folder, by name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Branch {
    /// Documents come before subfolders, so this sorts them first.
    folder: bool,
    name: String,
}

/// The order of one layer's documents, kept with the compiled layers, to
/// tell whether two of them are in order other than by name.
#[derive(Debug, Default, Clone)]
pub(crate) struct LayerOrder {
    /// Each document's path in the layer, split at `/`.
    paths: HashMap<FileId, Vec<String>>,
    /// For each folder, by its path split at `/`, the order its `extends`
    /// give: what comes first, then what comes after.
    edges: HashMap<Vec<String>, Vec<(Branch, Branch)>>,
}

impl LayerOrder {
    /// A document's path in the layer.
    pub(crate) fn path(&self, file: FileId) -> Option<String> {
        self.paths.get(&file).map(|parts| parts.join("/"))
    }

    /// Whether the documents `a` and `b` of this layer are in an order that
    /// is not that of their names: one is in a folder that holds the
    /// other's, or `extends` put one first.
    pub(crate) fn ordered(&self, a: FileId, b: FileId) -> bool {
        let (Some(a), Some(b)) = (self.paths.get(&a), self.paths.get(&b)) else {
            return false;
        };
        let depth = common_folders(a, b);
        let (first, second) = (branch(a, depth), branch(b, depth));
        if first.folder != second.folder {
            return true;
        }
        self.edges
            .get(&a[..depth])
            .is_some_and(|edges| reaches(edges, &first, &second) || reaches(edges, &second, &first))
    }
}

/// How many folders two paths start with in common.
fn common_folders(a: &[String], b: &[String]) -> usize {
    let folders = (a.len() - 1).min(b.len() - 1);
    a.iter()
        .zip(b)
        .take(folders)
        .take_while(|(x, y)| x == y)
        .count()
}

/// What holds a document at `depth`: itself, or the subfolder it is in.
fn branch(path: &[String], depth: usize) -> Branch {
    Branch {
        folder: depth + 1 < path.len(),
        name: path[depth].clone(),
    }
}

/// Whether `extends` put `from` before `to`, directly or not.
fn reaches(edges: &[(Branch, Branch)], from: &Branch, to: &Branch) -> bool {
    let mut seen = vec![from];
    let mut todo = vec![from];
    while let Some(at) = todo.pop() {
        for (_, after) in edges.iter().filter(|(before, _)| before == at) {
            if after == to {
                return true;
            }
            if !seen.contains(&after) {
                seen.push(after);
                todo.push(after);
            }
        }
    }
    false
}

/// The module paths of a document's `extends`, each with its span.
fn extends_of(sources: &Sources, file: FileId) -> Vec<(String, Span)> {
    let text = sources.file(file).text();
    if !text.contains("extends") {
        return Vec::new();
    }
    let parse = crol_syntax::parse(text);
    parse
        .document()
        .attributes()
        .filter_map(|attribute| match attribute.kind() {
            Some(ast::AttributeKind::Extends(extends)) => extends.module_path(),
            _ => None,
        })
        .map(|path| {
            let syntax = ast::AstNode::syntax(&path);
            let written: String = syntax.text().to_string().split_whitespace().collect();
            (written, Span::new(file, syntax.text_range()))
        })
        .collect()
}

/// The documents of one folder, and its subfolders.
#[derive(Default)]
struct Folder<'l> {
    /// By name; two documents with one path are an error the caller
    /// reports, and both are kept, in the order given.
    documents: BTreeMap<&'l str, Vec<usize>>,
    folders: BTreeMap<&'l str, Folder<'l>>,
}

/// A layer's documents in order, without its `schema.crod` unless `schema`
/// says, with what tells which are in order other than by name, and the
/// errors in their `extends`.
pub(crate) fn order_layer<'l>(
    sources: &Sources,
    layer: &'l Layer,
    schema: bool,
) -> (Vec<&'l (String, FileId)>, LayerOrder, Vec<Diagnostic>) {
    let documents: Vec<&(String, FileId)> = layer
        .documents
        .iter()
        .filter(|(path, _)| schema || path != crate::SCHEMA_DOCUMENT)
        .collect();
    let mut diagnostics = Vec::new();
    let mut root = Folder::default();
    let mut order = LayerOrder::default();
    for (index, (path, file)) in documents.iter().enumerate() {
        let parts: Vec<&str> = path.split('/').collect();
        let (name, folders) = parts.split_last().expect("split gives one part at least");
        let mut folder = &mut root;
        for part in folders {
            folder = folder.folders.entry(part).or_default();
        }
        folder.documents.entry(name).or_default().push(index);
        order
            .paths
            .insert(*file, parts.iter().map(|p| (*p).to_owned()).collect());
    }

    // What each `extends` that names something in the layer puts first.
    let mut spans: HashMap<(Vec<String>, Branch, Branch), Span> = HashMap::new();
    let paths = || documents.iter().map(|(path, _)| path.as_str());
    for (path, file) in &documents {
        for (written, span) in extends_of(sources, *file) {
            let Some(target) = layer_target(paths(), path, &written) else {
                continue;
            };
            let own = &order.paths[file];
            let (target, is_document) = match &target {
                LayerTarget::Document(target) => (target, true),
                LayerTarget::Folder(target) => (target, false),
            };
            let target: Vec<String> = target.split('/').map(str::to_owned).collect();
            if is_document && target == *own {
                diagnostics.push(Diagnostic::error(
                    span,
                    "`extends` forms a cycle: the document names itself",
                ));
                continue;
            }
            if !is_document && own.starts_with(&target) {
                diagnostics.push(Diagnostic::error(
                    span,
                    format!(
                        "`extends` names the folder `{}/`, which holds this document",
                        target.join("/")
                    ),
                ));
                continue;
            }
            // The deepest folder that holds both, and what holds each there.
            let mut target_path = target.clone();
            if !is_document {
                // A folder is as a document below it.
                target_path.push(String::new());
            }
            let depth = common_folders(own, &target_path);
            let (before, after) = (branch(&target_path, depth), branch(own, depth));
            if before.folder && !after.folder {
                diagnostics.push(Diagnostic::error(
                    span,
                    format!(
                        "`extends` cannot put `{}/` before `{}`: a folder's documents come before its subfolders'",
                        before.name, after.name
                    ),
                ));
                continue;
            }
            let folder = own[..depth].to_vec();
            spans
                .entry((folder.clone(), before.clone(), after.clone()))
                .or_insert(span);
            let edges = order.edges.entry(folder).or_default();
            if !edges.contains(&(before.clone(), after.clone())) {
                edges.push((before, after));
            }
        }
    }

    let mut ordered = Vec::with_capacity(documents.len());
    let mut folder_path = Vec::new();
    walk(
        &root,
        &mut folder_path,
        &order,
        &spans,
        &mut |index| ordered.push(documents[index]),
        &mut diagnostics,
    );
    (ordered, order, diagnostics)
}

/// Puts a folder's documents in order, then each subfolder's.
fn walk<'l>(
    folder: &Folder<'l>,
    path: &mut Vec<String>,
    order: &LayerOrder,
    spans: &HashMap<(Vec<String>, Branch, Branch), Span>,
    out: &mut dyn FnMut(usize),
    diagnostics: &mut Vec<Diagnostic>,
) {
    let edges = order
        .edges
        .get(path.as_slice())
        .map_or(&[][..], Vec::as_slice);
    let documents: BTreeSet<Branch> = folder
        .documents
        .keys()
        .map(|name| Branch {
            folder: false,
            name: (*name).to_owned(),
        })
        .collect();
    let folders: BTreeSet<Branch> = folder
        .folders
        .keys()
        .map(|name| Branch {
            folder: true,
            name: (*name).to_owned(),
        })
        .collect();
    for group in [documents, folders] {
        for branch in sorted(group, edges, path, spans, diagnostics) {
            if branch.folder {
                path.push(branch.name.clone());
                let inner = &folder.folders[branch.name.as_str()];
                walk(inner, path, order, spans, out, diagnostics);
                path.pop();
            } else {
                for index in &folder.documents[branch.name.as_str()] {
                    out(*index);
                }
            }
        }
    }
}

/// The documents, or the subfolders, of a folder in the order `edges` give,
/// and by name where they give none. A cycle is an error, and what is in it
/// goes by name.
fn sorted(
    group: BTreeSet<Branch>,
    edges: &[(Branch, Branch)],
    path: &[String],
    spans: &HashMap<(Vec<String>, Branch, Branch), Span>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Branch> {
    let edges: Vec<&(Branch, Branch)> = edges
        .iter()
        .filter(|(before, after)| group.contains(before) && group.contains(after))
        .collect();
    let mut waiting: HashMap<&Branch, usize> = HashMap::new();
    for (_, after) in &edges {
        *waiting.entry(after).or_default() += 1;
    }
    let mut ready: BTreeSet<&Branch> = group
        .iter()
        .filter(|branch| !waiting.contains_key(branch))
        .collect();
    let mut out = Vec::with_capacity(group.len());
    while let Some(next) = ready.pop_first() {
        out.push(next.clone());
        for (_, after) in edges.iter().filter(|(before, _)| before == next) {
            let count = waiting.get_mut(after).expect("counted above");
            *count -= 1;
            if *count == 0 {
                ready.insert(after);
            }
        }
    }
    if out.len() < group.len() {
        let left: Vec<&Branch> = group.iter().filter(|b| !out.contains(b)).collect();
        let mut in_cycle = edges
            .iter()
            .filter(|(before, after)| left.contains(&before) && left.contains(&after))
            .filter_map(|(before, after)| {
                spans.get(&(path.to_vec(), before.clone(), after.clone()))
            });
        if let Some(first) = in_cycle.next() {
            let mut diagnostic = Diagnostic::error(
                *first,
                "these `extends` put documents of one folder in a cycle",
            );
            for span in in_cycle {
                diagnostic = diagnostic.with_related(*span, "another `extends` in the cycle");
            }
            diagnostics.push(diagnostic);
        }
        out.extend(left.into_iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATHS: [&str; 5] = [
        "a.crod",
        "units/x.crod",
        "units/rebels/x.crod",
        "units/empire/x.crod",
        "units/empire/deep/y.crod",
    ];

    fn target(from: &str, module_path: &str) -> Option<LayerTarget> {
        layer_target(PATHS, from, module_path)
    }

    #[test]
    fn an_extends_names_documents_and_folders_of_the_layer() {
        let document = |p: &str| Some(LayerTarget::Document(p.to_owned()));
        let folder = |p: &str| Some(LayerTarget::Folder(p.to_owned()));
        assert_eq!(
            target("units/empire/x.crod", "super::rebels::x"),
            document("units/rebels/x.crod")
        );
        assert_eq!(
            target("units/empire/x.crod", "super::rebels"),
            folder("units/rebels")
        );
        assert_eq!(target("a.crod", "units::empire"), folder("units/empire"));
        assert_eq!(target("units/x.crod", "super::a"), document("a.crod"));
        // From the root directory, above the layer, or naming nothing in it.
        assert_eq!(target("a.crod", "::units"), None);
        assert_eq!(target("a.crod", "super::units"), None);
        assert_eq!(target("units/x.crod", "nothing"), None);
    }

    fn layer(documents: &[(&str, &str)]) -> (Sources, Layer) {
        let mut sources = Sources::new();
        let documents = documents
            .iter()
            .map(|(path, text)| {
                (
                    (*path).to_owned(),
                    sources.add((*path).to_owned(), (*text).to_owned()),
                )
            })
            .collect();
        (
            sources,
            Layer {
                name: "base".into(),
                documents,
            },
        )
    }

    #[test]
    fn a_folder_comes_before_its_subfolders_and_extends_orders_one_folder() {
        let (sources, layer) = layer(&[
            ("units/rebels/x.crod", ""),
            ("units/empire/x.crod", "#![extends(super::rebels)]"),
            ("units/x.crod", ""),
            ("units/b.crod", "#![extends(c)]"),
            ("units/c.crod", ""),
            ("a.crod", ""),
        ]);
        let (documents, order, errors) = order_layer(&sources, &layer, false);
        assert!(errors.is_empty(), "{errors:?}");
        let paths: Vec<&str> = documents.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "a.crod",
                "units/c.crod",
                "units/b.crod",
                "units/x.crod",
                "units/rebels/x.crod",
                "units/empire/x.crod",
            ]
        );
        let file = |path: &str| layer.documents.iter().find(|(p, _)| p == path).unwrap().1;
        let ordered = |a: &str, b: &str| order.ordered(file(a), file(b));
        assert!(ordered("units/x.crod", "units/empire/x.crod"));
        assert!(ordered("a.crod", "units/rebels/x.crod"));
        assert!(ordered("units/empire/x.crod", "units/rebels/x.crod"));
        assert!(ordered("units/c.crod", "units/b.crod"));
        assert!(!ordered("units/x.crod", "units/b.crod"));
        assert!(!ordered("units/x.crod", "units/c.crod"));
    }
}
