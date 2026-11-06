# The CROL guide

This guide is for people who use CROL: writing data files, loading them into a
Rust program, and working with them in an editor. It can be read from top to
bottom, and each page ends with a link to the next.

New here? Start with [getting started](getting-started.md).

## Learn

1. [Getting started](getting-started.md): install the tools, write a file, load
   it into a Rust struct, stack a second file on top.
2. [The language by example](language.md): everything you can write in a file.
3. [Data mode and value mode](modes.md): what `.crol` and `.crod` files are, and
   which one you want.
4. [Layers](layers.md): what they are, why they exist, how to use them.
5. [Variants](variants.md): differences per environment, region or feature.

## Build

6. [Loading data in Rust](rust.md): how values map onto your types, what you can
   do with the result, and how your program can change it and save it.
7. [Schemas](schemas.md): how they are generated from your Rust types, and how
   tools find them.
8. [Project structure](projects.md): the project file, how files name each other,
   and recommended layouts.
9. [Recipes](recipes.md): optional layers, environments, secrets, CI, moving from
   RON.

## Reference

10. [Command-line reference](cli.md): every `crol` command.
11. [Editor setup](editors.md): the language server, Zed and other editors.
12. [Troubleshooting](troubleshooting.md): common errors and what they mean.

## Which page answers my question?

| I want to...                                       | Read                                                    |
| -------------------------------------------------- | ------------------------------------------------------- |
| read one config file into a struct                 | [Getting started](getting-started.md#load-it-into-rust) |
| override settings without editing the shared file  | [Layers](layers.md)                                     |
| have different values in dev and production        | [Variants](variants.md)                                 |
| know why `from_str` rejects what `compose` accepts | [Modes](modes.md#side-by-side)                          |
| get completion in my editor                        | [Schemas](schemas.md), then [Editors](editors.md)       |
| change data from my program and save it            | [Databases](rust.md#changing-data-with-a-database)      |
| read a few fields of many values                   | [Views](rust.md#views)                                  |
| find the keys that hold something                  | [Queries](rust.md#queries)                              |
| know where a value came from                       | [`crol explain`](cli.md#crol-explain)                   |
| know which schema a tool is using                  | [How schemas are found](schemas.md#how-schemas-are-found) |
| lay out a new project                              | [Recommended layouts](projects.md#recommended-layouts)  |

## Other documentation

- The [API docs](https://docs.rs/crol) describe the library.
- [`SPEC.md`](../SPEC.md) is the complete, precise definition of the format, for
  people who implement it or need to settle an argument. You do not need it to
  use CROL.

CROL is in early development: the format and the APIs may change before 1.0.
