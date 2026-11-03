//! CROL, the Composable Rust Object Language.
//!
//! CROL is a text format for configuration and data files that load into
//! Rust data models through serde. It looks like RON, but a document holds
//! operations as well as values: it can declare, replace, patch and remove
//! definitions, including ones written in other files and in
//! lower-priority layers.
//!
//! This crate will be the facade: `CrolComposer`, `CrolConfig`, the serde
//! `Deserializer` and `Serializer`, `from_str` and `to_string`. It is not
//! implemented yet; see `SPEC.md` section 14.3 for the milestones.

#![forbid(unsafe_code)]
