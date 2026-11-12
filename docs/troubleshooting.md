# Troubleshooting

The most common errors, what they mean, and what to do about them.

Two commands solve most mysteries:

- `crol eval` shows what your files evaluate to.
- `crol explain` shows how a value got that way.

## Writing entries

**`` `web` is not set, so it cannot be patched ``**

A path like `web.port: 80` changes a value that must already be set. Either `web`
is not defined anywhere, or the layer that defines it is not loaded, or it is
loaded later than this one. Check the order of your layers: lowest first. To
create the key, write `+web: { port: 80 }`.

The same goes for every step of a longer path: `web.limits.cpu: 2` with no `limits`
gives ``web.limits.cpu: `web.limits` is not set, so it cannot be patched``.
Write `web.limits?.cpu: 2` to start from `limits`' default, or `{ .. }` if it has none.
For every path at once, [`init_defaults`](schemas.md#paths-through-fields-left-out)
does the same where there is a default.

**`` +: `web` is already set ``**

`+` gives a key its first value, and something below already set it. To change the
value, use `web: ...` or `web.port: ...`. To set it only if it is missing, use
`?web: ...`.

**`` -: `web` is not set ``**

`-` removes a key, and the key is not set. Check the spelling and the order of
your layers. To remove it only when something sets it, write `-?web`.

**`` `web` is defined twice in the layer `base`; a layer may define a key once ``**

Two entries in one layer give `web` a value without looking at what is below,
possibly in two files. Keep one, or make the second a
[patch](layers.md#definitions-and-patches): `web.port: 81`, or
`web: & ~ { port: 81 }`.

**`` `web` is patched from `local.crod` and `tuning.crod` of the layer `base`, and only their names put the patches in order ``**

Both files patch `web`, the patches can apply together, and nothing but the
file names says which comes first, so renaming a file would change the result.
Move one file into a folder below the other's, or name the other in its
`#![extends(...)]`: see [the order of files in a layer](layers.md#the-order-of-files-in-a-layer).
If the patches have conditions that should never hold together, such as
`<empire>` and `<rebel>`, declare those variants as [one group](variants.md#declaring-variants):
undeclared variants are discrete, and can all be active at once.

**`` these `extends` put documents of one folder in a cycle ``**

`a.crod` extends `b`, and `b.crod` extends `a`, directly or through other files,
so neither can come first. Remove one of the `extends`.

**`` `extends` cannot put `sub/` before `c.crod`: a folder's documents come before its subfolders' ``**

A folder's own files always come before its subfolders, so an `extends` cannot
reverse that. Move `c.crod` into a subfolder of its own, or the files of `sub/`
up a level.

**`` expected `,` or a line break between entries ``**

Two entries on one line need a comma between them, or put each on its own line.

**`` unexpected `:` after an entry: the key before it was read as part of the value; is a `,` missing, or an operand? ``**

The key was read as part of the value before it. On one line, a comma is missing:
in `+a: 1 +b: 2`, the `+` adds `b` to `1`. Else, a line that does not begin an entry
continues the value above it. Look at the end of the line before, for an operator
with nothing after it, as in `a: 1 +`. Look at the start of this line too: an
entry's start, up to its `:`, must be on one line. See
[line breaks and commas](language.md#line-breaks-and-commas).

**`` expected `,` `` inside a list or tuple**

Line breaks do not separate list and tuple elements. Add the comma.

**`` expected `,`: a line break does not separate entries before `-` and an integer or `[` ``**

`-12` on a line of its own would read as a subtraction. To remove an integer key,
end the line before with a comma. `-?12` needs none, as it is never a value, but
it is no error where `12` is not set.

**A value is unexpectedly a number, such as `-7`.** The same rule, silently: after
a value, `-12` on the next line is a subtraction, so `a: 5` then `-12` gives
`a: -7`. Write `a: 5,` to remove key `12` instead.

## References

**`` no bind `port` is visible; did you mean `&self.port`? ``**

`&port` is never a lookup in the surrounding block. Write `&self.port` for a
field of this block, `&root.web.port` for one elsewhere, or
[declare a bind](language.md#binds).

**`&: there is no value below here`**

`&` means the value before this entry, and there is none. If a value below is
optional, add a fallback: `counter: & + 1 ?? 1`. In a template, `&` reads the value
below where the template lands, so this means nothing was below there either.

**`cycle: this value depends on itself`**

A value needs itself to be computed, and the message lists the references
involved. Late references read final values, so `a: &root.a + 1` does not mean
"add one to the old `a`". For that, use `&`: `a: & + 1`.

## Types

**`` `Service` is missing the field `port`; write it, or end the block with `..` to leave out fields that have defaults ``**

Every field must be written, including `Option` fields. Add it, or end the block
with `..` to [leave it out on purpose](language.md#leaving-fields-out).

If the message names the struct you load the root into, no layer sets the field.
Add it to a layer, or write `..` at the top level of any layer to
[let it take its default](rust.md#a-struct-at-the-root). It is reported at the top of
the last file of the last layer, since it belongs to no line.

If the type has `#[crol(explicit_defaults = false)]`, and your program reports this
while `crol check` does not, the loader has no schema. Use
`CrolComposer::schema` or [`crol::from_str_with_schema`](rust.md#types-that-decide-for-themselves).

**`` `Service` has no field `replica`; its fields are ... ``**

A misspelt field, or a field of another type. If you want to ignore unknown
fields, set `allow_unknown_fields`.

**`` `Config` has no field `web`; its fields are ... ``** from `crol check`, on a root key

The schema's root type is a struct, so it describes the whole document, and each
root key is a field of it. If each root value is meant to be a `Config`, build
the schema from an enum or a map of them. See
[which type a document has](schemas.md#which-type-a-document-has).

**`` expected a `Definition` variant, found 8080 ``** from `crol check`

The schema's root type is an enum, so it is the type of each root value, and this
one is not a value of it. If the root keys are meant to be fields, build the schema
from the struct. See [which type a document has](schemas.md#which-type-a-document-has).

**`` ~: cannot merge `Service` with `Limits` ``**

The two sides of `~` have different type names. To change a value to another
type or enum variant, replace it with `:` instead.

**`` expected an id key, found the string key web; write `@web` ``**

The map is keyed by `crol::Id`, and such a map takes only
[id keys](language.md#ids-as-keys): write `@web: 1`. The other way round,
`` expected a string key, found the id key `@web` ``, the map has another
key type, or the value is a struct: only a map keyed by `crol::Id` takes id
keys.

**`` the id key `@web` names no root key; an id key has to name an entry, and a key that names nothing is written as a string ``**

Id keys are compared by what they name, so each has to name something in the
data, through the [scopes](language.md#scopes) around its map. Check the
spelling, or reach it with a path, `@group.web`. If the keys name things that
are not in the data, the map wants `String` keys.

**`+: cannot add an integer to a list`**

Lists only combine with lists: `tags: & + ["x"]`, not `tags: & + "x"`.

## Modes

These appear when what a file holds does not fit its [extension](modes.md).

**`the outer braces of a data-mode document cannot have a type name`**

**`` `type` names the type of a value-mode document; a data-mode document's root has no type ``**

**`a spread cannot be at the root of a data-mode document; the root is not a value`**

**``a data-mode document holds entries (`key: value,`), not one value``**

The file holds one value, but it is read as data. If it is a single value, name
it `.crol` and read it with `crol::from_str`. If it is meant to be a layer, remove
what data mode does not allow.

**`variants need layered data: a value-mode document is evaluated in no context`**

**`` `extends` names layers below a document, and a value-mode document has no layers ``**

The other way around: use `CrolComposer`.

## Variants

**`` unknown variant `prd` ``**

A condition names a variant that is not declared. Check the spelling, and that
what you passed to `--group` and `--discrete`, the project file and
`CrolVariants` agree.

**`` composer: `dev` and `prod` are alternatives in one group; at most one of them can be active ``**

`with_variant` was called for two variants in one group. A group means "at most
one". If they can be active together, declare them as discrete variants, or
give one [a parent](variants.md#declaring-variants).

## Databases

**`` the write to `retries` did not take effect: `retries<!prod>` in the layer `base` decides it ``**

`commit` found that the write changed nothing in the transaction's context, because
the operation it names applies there and wins. Change that operation, or write in a
context where it does not apply, with `with_variant`. Nothing was changed.

**`` `inner?.port` is not set in this context ``**

A read reached something that is not there: a key that is missing, a field left out
for its default, or a `None`. `update` reads first, so it fails the same way, and that
ends the transaction. To write through a `None` without reading it, use `set`.

**`` `tree` has no `combat.range`, and no default for it ``**

A [view](rust.md#views) reads a field that this value lacks. Make the field an
`Option`, give it `#[crol(default)]`, or read fewer values: `of_type` keeps a query to
values that have the field.

**`` a view needs a path to a root key or below it ``**

A view reads one value, and the root is not one. Give `view` a path to a key.

**`` the context {prod} was not evaluated ``**

`CrolConfig::contexts` or `each_variant` left that context out. Call
`db.evaluate(["prod"])` before `select`. A transaction does this for itself.

**`` user.crod: the active layer holds `+extra: 1`, which a database does not write ``**

The file was edited by hand, or it is not the database's. A database owns its active
layer, and writes only `^target: value`, `target: expression` and `-target`, in
variant blocks. Move everything else to [a layer below](rust.md#the-active-layer).

**`` user.crod changed since it was read; nothing was saved ``**

Something else wrote the file after it was loaded or saved. A `CrolDb` is for one
process. Load the file again, and repeat the transaction.

**`` there is no active layer to save to: use `save_as` ``**

The composer had no `load_active`. Add it, or call `save_as` with a new file.

**`` composer: the active layer `user.crod` is the last layer ``**

`load` or `load_active` came after `load_active`. Load the other layers first.

**`` `with_variant` comes before a transaction's first operation ``**

A transaction has one context. Call `with_variant` straight after `transaction()`, and
use a second transaction for another context.

**`` a write needs a path to a root key or below it ``**

The root itself cannot be written. Write its keys one at a time.

## Finding files and schemas

**`` a `.crol` file is a value document and cannot be a layer ``**

A `.crol` file was given as a layer, or lies in a layer's directory. Move it out,
or rename it to `.crod` if it is data. On the command line, a `.crol` file is read
as a value document only when it is the one file given.

**`` a `.crod` file is a data document and cannot be read as a value document ``**

A project or schema file has to be a `.crol` file. Rename it.

**`` a value document has no variants, so `--context` does not apply ``**

Variants need layers. If the file is data, name it `.crod`.

**`no layers given, and no project.crol in this directory or above`**

Name the layers on the command line, or [add a project
file](projects.md#the-project-file). If the message goes on to say that `base` is
read as a key, you gave `crol eval` or `crol explain` a layer in the place of the
key: [write it `./base`](cli.md#crol-eval).

**`` the module path `data::local` names nothing ``**

There is no such file or directory. [Module paths](projects.md#module-paths) start
from the document's directory unless they start with `::`, and cannot name files
such as `my-file.crod`. In a project, every layer's directory has to exist.

**`` `Project` is missing the fields `options`, `schema`, ... ``**

End the `Project { ... }` block with `..`, as for any struct.

**`the documents name different schemas; choose one with --schema`**

Several documents have a different `#![schema(...)]`, and there is no project to
decide. Pass `--schema`, or make them agree.

**`this document names another schema than the project's, which is used instead`**

A warning: the project's schema applies to all layers. Remove the attribute from
the document, or make it agree.

**`` no Cargo.toml in /some/dir or above; run this in a Rust project ``**

`crol init` sets up a Cargo project. Run `cargo init` first, or write a
[`project.crol`](projects.md#the-project-file) by hand.

**`project.crol already exists; it was left as it is`**

`crol init` never overwrites a project file. Edit the one you have.

**`&default: defaults come from a schema, and no schema is given`**

There is no schema to read the default from. Give your program the schema with
`CrolComposer::schema`, and tools one with `--schema`, `schema` in `project.crol`
or `#![schema(...)]`. A value-mode document never has defaults. See
[the limits](schemas.md#using-default).

**`` schema: `fleet` is not cached, and fetching schemas is off ``**

`--offline`, or `remote_schemas: false`, stops `crol` from fetching. Without
fetching, a crate has to be in your own Cargo workspace, among its dependencies,
or in Cargo's registry cache.


## In your editor

**No completion of field names, and no type errors.** There is no schema. Check
that `schema` is set in `project.crol`, or that the file has a `#![schema(...)]`.
Run `crol check` in the same directory: if that sees no type errors either, the
problem is not in the editor.

**Everything is underlined, as if the file were on its own.** The server has not
found your project. It looks in your workspace folder and above, so open the
project's folder, not a directory next to it, or set `project`.

**Errors about keys that exist in another file.** A `.crod` file that is not in
any layer only knows itself and what its `#![extends(...)]` names. Add an `extends`,
or add the file's directory to the `layers` in `project.crol`.

**An error on `#![extends(...)]`.** It names nothing, forms a cycle, or what it
names has errors of its own, of which the message shows the first. Other errors in
the document are then warnings when the `extends` cannot be followed at all, since
what is missing may be why.

**A `project.crol` has one error, and the rest shows nothing.**
A problem with the project file itself stops the server from loading the layers,
and is shown on the file to blame. Fix that one first.

**Warnings that end in `which may be why`.** A layer or an `extends` did not load,
and this may only be a result of that. Fix the error on the file it names, or on
`project.crol`; what is still wrong then shows as an error again.

**Zed cannot start the server.** Zed looks for `crol` in your settings and on the
`PATH`. With no `crol` there, it tries to download a release, and none exists yet.
Set `binary.path`.

**Helix shows no colours.** Run `hx --health crol`. A cross at the parser means the
grammar is not built, and one at the queries that they are not in
`~/.config/helix/runtime/queries/crol`. See [the setup](editors.md#helix).

**No diagnostics in one layer.** Check whether it is marked `read_only`.

## Asking for help

If you are stuck, or think you have found a bug, please
[open an issue](https://github.com/DreamLogics/crol/issues), with the smallest
file that shows the problem and the output of `crol --version`.
