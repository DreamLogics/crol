# Troubleshooting

The most common errors, what they mean, and what to do about them.

Two commands solve most mysteries:

- `crol eval` shows what your files evaluate to.
- `crol explain` shows how a value got that way.

## Writing entries

**`` `web` does not exist, so it cannot be patched ``**

A path like `web.port: 80` changes a value that must already exist. Either `web`
is not defined anywhere, or the layer that defines it is not loaded, or it is
loaded later than this one. Check the order of your layers: lowest first. To
create the key, write `+web: { port: 80 }`.

**`` +: `web` already exists ``**

`+` means "this is new", and something below already wrote it. To change the
value, use `web: ...` or `web.port: ...`. To set it only if it is missing, use
`?web: ...`.

**`` -: `web` does not exist ``**

`-` removes a key, and the key is not there. Check the spelling and the order of
your layers.

**`` `web` is defined twice in the layer `base`; a layer may define a key once ``**

Two entries in one layer give `web` a value without looking at what is below,
possibly in two files. Keep one, or make the second a
[patch](layers.md#definitions-and-patches): `web.port: 81`, or
`web: & ~ { port: 81 }`.

**`` expected `,` or a line break between entries ``**

Two entries on one line need a comma between them, or put each on its own line.

**`` unexpected `:` after an entry ``**

A line that does not begin an entry continues the value above it, so this line was
read as part of that value. Look at the end of the line before, for an operator
with nothing after it, as in `a: 1 +`. Look at the start of this line too: an
entry's start, up to its `:`, must be on one line. See
[line breaks and commas](language.md#line-breaks-and-commas).

**`` expected `,` `` inside a list or tuple**

Line breaks do not separate list and tuple elements. Add the comma.

**`` expected `,`: a line break does not separate entries before `-` and an integer or `[` ``**

`-12` on a line of its own would read as a subtraction. To remove an integer key,
end the line before with a comma.

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
optional, add a fallback: `counter: & + 1 | 1`. Note that a template has nothing
below it, since it is no key.

**`cycle: this value depends on itself`**

A value needs itself to be computed, and the message lists the references
involved. Late references read final values, so `a: &root.a + 1` does not mean
"add one to the old `a`". For that, use `&`: `a: & + 1`.

Currently, some cycles between whole root keys, as in `+a: &root.b, +b: &root.a`,
make `crol check` exit with status 1 without a message. If `crol` fails silently,
look for such a pair.

## Types

**`` `Service` is missing the field `port`; write it, or end the block with `..` to leave out fields that have defaults ``**

Every field must be written, including `Option` fields. Add it, or end the block
with `..` to [leave it out on purpose](language.md#leaving-fields-out).

If the message names the struct you load the root into, `..` is not an option,
since a data-mode file [has no block at the top](rust.md#a-struct-at-the-root).

If the type has `#[crol(explicit_defaults = false)]`, and your program reports this
while `crol check` does not, the loader has no schema. Use
`CrolComposer::schema` or [`crol::from_str_with_schema`](rust.md#types-that-decide-for-themselves).

**`` `Service` has no field `replica`; its fields are ... ``**

A misspelt field, or a field of another type. If you want to ignore unknown
fields, set `allow_unknown_fields`.

**`` expected `Config { ... }`, found 8080 ``** from `crol check`

The schema's root type is the type of each root value, not of the root as a
whole. See [which type a document has](schemas.md#which-type-a-document-has).

**`` ~: cannot merge `Service` with `Limits` ``**

The two sides of `~` have different type names. To change a value to another
type or enum variant, replace it with `:` instead.

**`+: cannot add an integer to a list`**

Lists only combine with lists: `tags: & + ["x"]`, not `tags: & + "x"`.

## Modes

These appear when what a file holds does not fit its [extension](modes.md).

**`the outer braces of a data-mode document cannot have a type name`**

**`` `type` names the type of a value-mode document; a data-mode document's root has no type ``**

**``a spread or `..` cannot be at the root of a data-mode document; the root is not a value``**

**``a data-mode document holds entries (`key: value,`), not one value``**

The file holds one value, but it is read as data. If it is a single value, name
it `.crov` and read it with `crol::from_str`. If it is meant to be a layer, remove
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

## Finding files and schemas

**`` a `.crov` file is a value document and cannot be a layer ``**

A `.crov` file was given as a layer, or lies in a layer's directory. Move it out,
or rename it to `.crod` if it is data. On the command line, a `.crov` file is read
as a value document only when it is the one file given.

**`` a `.crod` file is a data document and cannot be read as a value document ``**

A project or schema file has to be a `.crov` file. Rename it.

**`` the `.crol` extension is retired ``**

Files used to end in `.crol`, whatever their mode. Rename the file to the name in
the message: `.crod` for a layer, `.crov` for a single value, a project file or a
schema file. A `project.crol` becomes `project.crov`.

**`` a value document has no variants, so `--context` does not apply ``**

Variants need layers. If the file is data, name it `.crod`.

**`no layers given, and no project.crov in this directory or above`**

Name the layers on the command line, or [add a project
file](projects.md#the-project-file).

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
[`project.crov`](projects.md#the-project-file) by hand.

**`project.crov already exists; it was left as it is`**

`crol init` never overwrites a project file. Edit the one you have.

**`&default: defaults come from a schema, and no schema is given`**

There is no schema to read the default from. Give your program the schema with
`CrolComposer::schema`, and tools one with `--schema`, `schema` in `project.crov`
or `#![schema(...)]`. A value-mode document never has defaults. See
[the limits](schemas.md#using-default).

**`` schema: `fleet` is not cached, and fetching schemas is off ``**

`--offline`, or `remote_schemas: false`, stops `crol` from fetching. If the crate
is in your own Cargo workspace, this can also appear when you pass a file name
without a directory: `crol check ./config.crod` works where
`crol check config.crod` does not. A project file does not have this problem.

## Warnings

**`` `web` is patched from 2 documents of the layer `base`, so the order of the patches depends on the file names ``**

The result would change if a file were renamed. Move the patches into one file,
or into separate layers. `CrolConfig::warn_cross_file_patches` turns the warning
off in your program, and `warn_cross_file_patches: false` in the project file's
`options` does so for tools.

`compose` drops warnings. Use `compose_data` and `warnings()` if you want to log
them.

## In your editor

**No completion of field names, and no type errors.** There is no schema. Check
that `schema` is set in `project.crov`, or that the file has a `#![schema(...)]`.
Run `crol check` in the same directory: if that sees no type errors either, the
problem is not in the editor.

**Everything is underlined, as if the file were on its own.** The server has not
found your project. It looks in your workspace folder and above, so open the
project's folder, not a directory next to it, or set `project`.

**Errors about keys that exist in another file.** A `.crod` file that is not in
any layer is evaluated alone, and the server does not follow `#![extends(...)]`
yet, so what it builds on is missing. Add the file's directory to the `layers` in
`project.crov`.

**A `.crov` file or a `project.crol` has one error, and the rest shows nothing.**
A problem with the project stops the server from loading its layers, and is shown
on the file to blame. Fix that one first.

**Zed cannot start the server.** Zed looks for `crol` in your settings and on the
`PATH`. With no `crol` there, it tries to download a release, and none exists yet.
Set `binary.path`.

**No diagnostics in one layer.** Check whether it is marked `read_only`.

## Asking for help

If you are stuck, or think you have found a bug, please
[open an issue](https://github.com/DreamLogics/crol/issues), with the smallest
file that shows the problem and the output of `crol --version`.
