# Proposal 0001: Query entries

Status: draft, for discussion (2026-11-08). Nothing here is implemented.

## Summary

A _query entry_ applies one change to every root key whose value matches a
filter, instead of to one key by name:

```crol
\(
    select Deployment
    where
        tags ~= ["europe", "production"], // holds all of these
        tags !~ ["staging"],              // holds none of these
        version == 2,                     // equal to
        name ~= "web-*",                  // matches a pattern
): & ~ {
    tags: & | ["upgraded"],
    version: 3,
}

\(
    select Unit
    where
        tags ~= [@is_trooper],
        class != @mounted_trooper_class,
).combat.weapon_slots.melee.0.damage.amount: round!(& * 1.2)
```

`\( ... )` stands where a key would. Everything after it, the path, the `:`
and the value, means what it means after a key, and `&` reads the value below
of each key the query matches.

The central rule, and the one the rest of the proposal follows from: **a query
reads the value below, never the final value.** For each key, the filter tests
what the chain has produced up to the query's place in it, exactly as `&` would
see it there. That makes the result well defined, ordered and deterministic.
SPEC.md 10.11 gives the reason data cannot select keys today, _"what a patch
selects would depend on what patches do"_, and this rule is the answer to it.

> **Note on the examples.** In the request this proposal answers, the first
> example wrote `tags: & ~ ["upgraded"]`. Lists do not merge: crol rejects it
> today with `~: cannot merge a list with a list`. This proposal writes `& |
> ["upgraded"]`, which adds the tag once, or `& + ["upgraded"]` would always
> append it. In the second example, `melee.0` goes into a `Some(...)` payload, as
> it does in a path today. If `melee` is a `Vec`, write `melee[0]`.

## Motivation

Data written in many files and layers often holds many values of one kind:
units in a game, deployments, products, feature rules. A change that is about a
_kind_ of value, not one value, has to be written once per key:

```crol
// balance/troopers.crod
rifleman.combat.weapon_slots.melee.0.damage.amount: round!(& * 1.2)
grenadier.combat.weapon_slots.melee.0.damage.amount: round!(& * 1.2)
sniper.combat.weapon_slots.melee.0.damage.amount: round!(& * 1.2)
// ...and the one someone adds next month, which nobody remembers to add here
```

A template removes the repeated value, but not the repeated key list:

```crol
melee_buff := { combat.weapon_slots.melee.0.damage.amount: round!(& * 1.2) }

rifleman: & ~ &melee_buff
grenadier: & ~ &melee_buff
```

The list is the problem:

- **It goes stale.** A key that a mod, an extension layer or a teammate adds
  later is not in it, and nothing says so.
- **It repeats what the data already says.** Whether `sniper` is a trooper is
  recorded in `sniper.tags`. The list writes it a second time, in another file,
  where it can disagree.
- **It does not compose across layers.** An extension layer that wants "all
  European production deployments" has to know every key the layers below
  define, which is exactly the coupling layers exist to avoid.

The guide's answer today is to have the program
[generate a layer](../docs/language.md#what-is-not-in-the-language). That works,
but it moves a one-line rule into Rust, a build step and a generated file. And
the generated file cannot see what layers above it add.

## What this changes in the design

This reverses two recorded decisions, and the proposal should be judged on that:

1. **SPEC.md appendix A**: _"Decided: no comparisons or conditionals, now or
   later."_ Query filters are comparisons, and a query entry is conditional: it
   applies to a key or it does not.
2. **SPEC.md 10.11**: _"Data cannot select keys by what they hold, since what a
   patch selects would depend on what patches do."_

What it does **not** change, and should not:

- **Expressions stay free of comparisons.** `==`, `!=`, `~=` and `!~` exist only
  inside `where`. No expression produces a boolean from a comparison, and there
  is no `if`. A value is still computed the same way wherever it is written.
- **No new keys.** A query only applies to keys that exist below it. It never
  creates one, so the set of root keys still follows from the entries alone.
- **Determinism (SPEC.md 7.6).** The filter reads values that are already fixed
  by the order of the chain. No result depends on evaluation order, hashing or
  the platform.
- **No functions.** A query is not reusable and takes no arguments. It is one
  entry, applied in place in the chains it matches. SPEC.md 8.3 rejects applying
  one key's operations to another key because _"it would make operations into
  reusable functions"_. A query does not do that. It is one patch that names
  several keys, as `<a + b>` is one entry written under several conditions.

The case for making the exception: the alternative, generating layers, already
gives crol data that depends on data. It just does it outside the language,
where `crol explain`, the language server and layering cannot see it.

## The proposal

### Terminology

SPEC.md 5.1 already calls the steps of a path _selectors_ (`.name`, `[key]`,
`<c>`). To keep that word for one thing, this proposal says **query** for
`\( ... )`, **query entry** for an entry whose target starts with one, and
**filter** for its `select` and `where` clauses.

### Syntax

```crol
\( select TypeName where clause, clause, ... )
```

- `\(` starts a query. `\` is not a token in crol today (`unexpected character`),
  so it can only begin an entry, and a line that starts with it always begins one
  (SPEC.md 5.3).
- `select` takes one type name, or `_` for a value of any type or none.
- `where` is optional. Its clauses are separated by commas, as list elements are,
  and line breaks inside `\( ... )` mean nothing. A trailing comma is allowed.
  Every clause must hold. There is no `or` (see [open questions](#open-questions)).
- `select` and `where` are keywords only directly inside `\( ... )`, as `use` and
  `as` are only inside an attribute (SPEC.md 2.3). A field called `where` stays a
  field.

A clause is `path operator expr`:

- The **path** is written as a target's path is (SPEC.md 5.1), and starts at the
  candidate's value: `tags`, `combat.range.max`, `env["LOG LEVEL"]`, `point.0`.
  It reads like the key of an entry, not like a reference. It names a place in
  the value being tested, with no `&`, just as `web.port` on the left of `:`
  does. A variant selector `<c>` and the optional step `?` are not allowed in it.
- The **expression** is any crol expression, evaluated at the top level of the
  document where the entry is written. It can be a literal, an id, a template or
  a late reference: `version == &root.settings.target_version`. `&` in it is an
  error, since there is no value below a filter.

### Operators

All four use the equality of SPEC.md 3.3: `1` and `1.0` are not equal, and an
id is not equal to a string.

| Clause         | Left side | Right side | Holds when                                          |
| -------------- | --------- | ---------- | --------------------------------------------------- |
| `p == v`       | any       | any        | the value at `p` equals `v`                         |
| `p != v`       | any       | any        | the value at `p` does not equal `v`                 |
| `p ~= [a, b]`  | list      | list       | the list holds every element on the right           |
| `p !~ [a, b]`  | list      | list       | the list holds none of the elements on the right    |
| `p ~= "pat*"`  | string    | string     | the string matches the pattern                      |
| `p !~ "pat*"`  | string    | string     | the string does not match the pattern               |

- **`~=` and `!~` on lists are not each other's negation.** `tags !~ ["a",
  "b"]` holds none of them, and the negation of `tags ~= ["a", "b"]` would be
  "lacks at least one". The two common questions are "has all of these" and "has
  none of these", and those are what the operators answer. An empty list on the
  right always holds, for both.
- **Lists only take lists**, as with `+` and `|` (SPEC.md 6.3): `tags ~=
  "europe"` is an error, because without types a one-element match could not be
  told from a nested list.
- **A pattern** matches the whole string. `*` stands for any run of characters,
  including none, and `?` for exactly one character (a Unicode scalar value).
  `\` makes the next character literal. Since `"\*"` is not a valid string escape,
  that is written `r"web-\*"` or `"web-\\*"`. No other character is special.
- **Through `Some`.** An `Option` field accepts a plain value (`timeout: 30` is
  `Some(30)` once deserialized), but the data holds whatever was written. So
  where the left side is `Some(x)` and the right side is not itself a `Some(...)`,
  the clause compares `x`. This matches how views read paths "through
  `Some(...)`" (SPEC.md 10.11). Then `timeout == 30` holds whichever way
  `timeout` was written.
- Any other combination of kinds is an error naming both, as arithmetic errors do.

Ordering comparisons (`<`, `>=`) are left out. See
[open questions](#open-questions).

### What a query matches, and when

Take one query entry `Q`, in layer `L`, document `d`. Conceptually, `Q` is placed
in the chain of **every** root key, at the position a patch from `d` would take
(SPEC.md 7.2): after the layer's definition of the key, among the layer's patches
in document order, and before every higher layer.

When the chain of key `k` reaches `Q`, with `v` the value so far:

1. If `v` is missing, `Q` does nothing: a query never creates a key.
2. If `select` names a type and `v`'s **type name** is not that name, `Q` does
   nothing. The type name is the one on the value (`Deployment { ... }`,
   `Some(...)`), as `CrolData::query().of_type` reads it (SPEC.md 10.11). An
   untyped `{ ... }` matches only `select _`.
3. Each clause is tested against `v`, the value below, in source order. If one
   does not hold, `Q` does nothing.
4. Otherwise `Q` applies to `k` as the entry `k<path>: value` would, with `&`
   reading `v` (or the step of it the path reaches).

So a query sees the keys defined in its own layer and the layers below, with
every patch below it applied, and the patches above it see what it did:

```crol
// base/deployments.crod
+web_eu: Deployment {
    name: "web-eu",
    version: 2,
    tags: ["europe", "production"],
}
+api_eu: Deployment {
    name: "api-eu",
    version: 2,
    tags: ["europe", "production"],
}
+web_us: Deployment { name: "web-us", version: 2, tags: ["us", "production"] }
+web_staging: Deployment {
    name: "web-staging",
    version: 2,
    tags: ["europe", "production", "staging"],
}
```

```crol
// rollout/upgrade.crod
\(
    select Deployment
    where
        tags ~= ["europe", "production"],
        tags !~ ["staging"],
        version == 2,
        name ~= "web-*",
): & ~ {
    tags: & | ["upgraded"],
    version: 3,
}
```

```crol
// local/new.crod
+web_eu2: Deployment {
    name: "web-eu2",
    version: 2,
    tags: ["europe", "production"],
}
```

```console
$ crol eval base rollout local
web_eu: Deployment {
    name: "web-eu",
    version: 3,
    tags: ["europe", "production", "upgraded"],
}
api_eu: Deployment {
    name: "api-eu",
    version: 2,
    tags: ["europe", "production"],
}
web_us: Deployment { name: "web-us", version: 2, tags: ["us", "production"] }
web_staging: Deployment {
    name: "web-staging",
    version: 2,
    tags: ["europe", "production", "staging"],
}
web_eu2: Deployment {
    name: "web-eu2",
    version: 2,
    tags: ["europe", "production"],
}
```

`api_eu` fails `name`, `web_us` fails the first clause, and `web_staging` the
second. `web_eu2` is defined in a layer above the query, so the query never saw
it. That is the same reason a path entry in `rollout` cannot patch a key that
only `local` defines. To reach it, put the query in `local` or above.

### Why the value below, and not the final value

Read the final value instead, and the first example has no answer. If
`web_eu` matches, its version becomes 3, so `version == 2` no longer holds, so it
does not match, so its version stays 2, so it matches. Any query whose change
touches what it filters on is a paradox, and less obvious ones arise through
references. Solving for a fixed point would make the result depend on the order
it was searched in, which breaks SPEC.md 7.6.

Reading the value below has no such loop. `v` is fixed before `Q` runs, and
`Q`'s change comes after. It also says what the author means: "the deployments
that, at this point in the layering, are version 2". And it gives queries the
same order as every other operation, so two queries in one document compose in
the obvious way:

```crol
\(select Deployment where version == 2).version: 3
\(select Deployment where version == 3).replicas: & * 2 // includes the ones just upgraded
```

The values a filter reads can still contain late references, and those read
final values as always. A clause on a field written `&self.other`, where `Q`
itself sets `other`, needs `Q`'s result to decide whether `Q` applies. That is a
cycle, reported as one (SPEC.md 7.5), with the clause among its steps.

### Missing values

A clause whose path does not exist in `v`, because a field was not set or a step
is missing, **does not hold**, whatever its operator. `version != 2` does not hold
on a deployment without `version`, and `tags !~ ["staging"]` does not hold on one
without `tags`. A filter only holds on what it can see.

The rule is chosen so that a query over many values of one type does not stop at
the first one that leaves an optional field out. Erroring would make `select _`
unusable. Treating `!=` as true on a missing field would apply changes to values
whose author never wrote the field, which is the less safe mistake.

With [`init_defaults`](../docs/rust.md#configuration) and a schema, a field left
out reads its default, as a path does (SPEC.md 5.1).

### Entry kinds

| Entry                            | Allowed | Meaning for each match                         |
| -------------------------------- | ------- | ---------------------------------------------- |
| `\(...): expr`                   | yes     | set the key, `&` reading its value below       |
| `\(...).path: expr`              | yes     | a path entry; every step but the last must exist, as usual |
| `^\(...).path: expr`             | yes     | replace                                        |
| `?\(...).path: expr`             | yes     | ensure                                         |
| `-\(...)`, `-\(...).path`        | yes     | remove the key, or the field                   |
| `-?\(...).path`                  | yes     | remove if set                                  |
| `+\(...)...`                     | no      | error: a match always exists, so a declare would always fail |
| `name = \(...)...`               | no      | error: a bind names one target                 |
| `\(...)` inside a block          | no      | error in this proposal; see [open questions](#open-questions) |

A query entry is always a **patch** (SPEC.md 7.2), even `\(...): expr` without
`&`. It is never a key's definition, and it is not counted against "a layer may
define a key once".

A path step that a matched key lacks is an error, as for any path entry, naming
both the key and the query: the filter said the value is a `Unit`, so the path
is expected to be there. Write `?` steps, or a `where` clause on the path, for
values where it may not be.

### Variants

A query entry can have a condition like any entry: `<prod> { \(...): ... }`, or
`\(...)<prod>.path: ...`. It is a conditional operation of that specificity
(SPEC.md 8.3). The filter is tested in the context being evaluated, against the
value below in that context's chain. So the same query can match a key in
`{prod}` and not in the base.

### Order within a layer

Two patches of one key, from documents of one layer that are not in order, are
an error when they meet (SPEC.md 7.2). Which keys a query patches is not known
until evaluation, so the check moves there: where `Q` passes step 2 for a key
(the type name), and that key has a patch from a document of `L` that is not in
order with `d`, it is an error. The check runs before the clauses are tested,
because their result could itself depend on the order.

In practice, put queries in a layer of their own above the data, or in a document
that the others `extend`. Then they are in order with everything.

## Tooling

- **`crol explain`** lists a query that applied like any patch, with its target
  shortened:

  ```console
  $ crol explain base rollout local web_eu.version
  web_eu.version:
    base/deployments.crod:1:2   +web_eu                definition  2
    rollout/upgrade.crod:1:1    \(select Deployment…)  query       3
  ```

  `crol explain --queries` (or a new `crol query <file>:<line>`) also lists the
  candidates a query tested, and the first clause each one failed. That is the
  question users will ask most: "why did it not match `api_eu`?"
- **Language server.** Complete type names after `select`. Complete field paths
  in clauses from the schema of that type. Show the matched keys as a code lens
  (`matches 3 keys`) and on hover. Warn on a query that matches nothing in any
  evaluated context, on `select T` where the schema has root values of type `T`
  written without a type name (they will never match), and on `~=` or `!~` with a
  one-element list on the right, where `==` was probably meant.
- **Formatter.** One clause per line when the query does not fit on one, with
  `select` and `where` aligned, as in the summary's examples.
- **Tree-sitter grammar, Zed and Helix.** New rules for the query, and
  highlighting for `select`, `where` and the four operators. The editor pins move
  with the grammar, as for every grammar change.
- **Compiled layers.** Query entries lower to a new operation, so
  `binary::FORMAT` changes, and old `.crob` files are compiled again.
- **Evaluating again (SPEC.md 7.9).** A changed document that holds a query
  evaluates everything again, as a changed template does. A change elsewhere can
  only change what a query matches through the keys it changes, so the existing
  rules still find what to evaluate again. One addition: a query's clauses that
  read late references count as reads of every key the query may match.
- **Fingerprints** are over values, and do not change.

## Implementation sketch

- **`crol-syntax`**: lex `\`, `==`, `!=`, `~=` and `!~`. None of them can occur
  in valid text today, so the new tokens change no existing document. Parse
  `query` as a new kind of target start, a `Query` node in `ast.rs`, and the
  clauses. Add the case to the line-break rule in the parser and in `format.rs`.
- **`crol-eval`**: add a `Query` to `ir.rs`, with the type name, clauses and
  span, and an optional `query` on `RootOp`. In `DataPlan`, expand a query
  operation into each `KeyPlan` (base or conditional, by its condition) at its
  layer and document position. The key set is static, because a query creates no
  key. In `Evaluator::apply_chain`, a query op checks steps 1–3 first and is
  skipped if they fail. `chain_outcome`, which works out a chain's result without
  applying it, treats a query op as possibly changing the value. Move the layer
  ordering check for query ops to evaluation. Limits (SPEC.md 7.7): one query op
  per key counts towards the 2 000 000 computed values, and a limit on clauses
  per query (say 64) keeps the work bounded.
- **`crol`**: no API change. `CrolData::explain` gains the query step kind.
  `Query::holding` and the clause `~=` should share one implementation of "a
  list that holds it".
- **`crol-lsp`, `crol-cli`**: as under [Tooling](#tooling).
- **Cost.** Each query costs a type-name check per root key that has a value at
  its position, plus the clauses for the keys of that type. A clause needs the
  full value at its path, but not the rest of the key's value.

## Alternatives considered

1. **Keep the status quo**: templates plus a key list, or a generated layer. This
   costs nothing in the language, and is described under
   [Motivation](#motivation).
2. **Filter on the final value.** Rejected: it is circular, as shown
   [above](#why-the-value-below-and-not-the-final-value).
3. **Select by key name only**: `\(web_*): & ~ { ... }`, a glob over root keys
   with no `where`. It needs no comparisons and keeps appendix A intact, and it
   is cheap and static. But it pushes meaning into key names, and does not answer
   "every trooper", which is about data.
4. **Select by type only**: `\(select Unit)`. Static enough to check without
   evaluating, but rarely precise enough on its own.
5. **Tags as variants**: model `is_trooper` as a variant and write `<is_trooper>`.
   Variants describe contexts of the whole data, not properties of one value, and
   this would merge two separate ideas.
6. **A Rust-side hook** that patches matching keys after composing. It is
   invisible to `crol explain`, to the editor, and to the layers above.

## Drawbacks

- **It is the first comparison in crol**, and so it is the first step towards a
  language with logic. The next requests are easy to predict: `or`, `<`, a
  comparison in a value. The proposal should come with an explicit line for what
  stays out (see [What this changes](#what-this-changes-in-the-design)).
- **Action at a distance.** Today, every operation on `web` names `web`, so a text
  search finds them all. A query does not. `crol explain` and the editor make up
  for it, but only where they are used.
- **The written type name matters more.** `select Deployment` misses a
  deployment written `{ ... }`. Evaluation cannot use the schema (SPEC.md goal 3:
  loading needs no schema), so tooling has to catch it.
- **Every key is a candidate.** A query makes the evaluator check every root key's
  value at the query's position, which removes some of the laziness that
  `CrolData::reload` relies on.

## Open questions

1. **`or`.** Clauses only combine with "and". Two query entries do not give "or"
   when a key matches both, since then both apply. Options: `where (a, b) | (c)`,
   or nothing, and use a tag.
2. **Ordering comparisons.** `version < 3` is the obvious next request. `<`
   cannot follow a path in crol today, because it starts a variant selector. Inside
   `where` it could mean "less than", since a clause's path cannot have one. But
   then `<` means two things, which the grammar went out of its way to avoid
   (SPEC.md 9).
3. **Matching the key itself.** `name ~= "web-*"` reads a field called `name`.
   Filtering on the root key needs a spelling: `@ ~= "web-*"`, or a clause `key
   "web-*"` beside `select`.
4. **Queries inside blocks**, to select among the entries of a map:
   `units: & ~ { \(select _ where tier == 2).cost: & + 10 }`. This is likely just
   as useful, and the rules carry over with "root key" read as "key of this
   block".
5. **Should `select` use the schema** when there is one, to also match untyped
   values whose field type is `T`? That would make the data's meaning depend on
   whether a schema was loaded, which crol avoids today.
6. **A missing value that should be an error.** If "a clause on a missing path
   does not hold" proves too forgiving, a clause could opt in to errors, for
   example with `tags! ~= [...]`.

## If accepted

- SPEC.md: a new section after 5.1 (query targets), additions to 2.5 (tokens), 5.3
  (`\` begins an entry), 7.2 and 7.4 (a query op in chains), 7.7 (limits), 7.9,
  section 9 (grammar), and 13 (errors). Change appendix A's "no comparisons" entry
  to record this exception and what stays out, and remove the last paragraph of
  10.11.
- Grammar, in the notation of SPEC.md 9:

  ```ebnf
  entry        ::= "-" "?"? target
                 | bind? ( "+" | "?" | "^" )? target ":" expr
                 | ...
  target       ::= ( key "?"? | query ) step*
  query        ::= "\" "(" "select" ( identifier | "_" ) ( "where" clauses )? ")"
  clauses      ::= clause ( "," clause )* ","?
  clause       ::= clause_path ( "==" | "!=" | "~=" | "!~" ) expr
  clause_path  ::= ( identifier | string | accessor ) ( "." identifier | "." tuple_index | accessor )*
  ```

  `bind` and `"+"` before a `query` target are errors in the reader, not the
  grammar, so that they get a clear message.
- docs/language.md: a section "Changing many values at once", after "Templates
  and spreads", and a change to "What is not in the language". docs/layers.md: a
  paragraph on where a query sees keys.
- CHANGELOG.md, the tree-sitter grammar and the editor pins, as for any change to
  the grammar.
