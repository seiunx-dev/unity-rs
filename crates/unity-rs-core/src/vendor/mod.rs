//! Third-party code copied into this crate rather than depended on.
//!
//! Vendoring is a last resort here: it moves permanent maintenance of someone
//! else's code into this project. Each entry below documents the specific
//! defect that made the dependency unusable as published, and keeps its delta
//! from upstream to the smallest change that fixes it, so the copy can be
//! diffed against the original and dropped when upstream catches up.

// The copies stay as close to the published source as possible, so items
// this crate does not call are kept rather than pruned: a smaller diff
// against upstream is worth more than a smaller file.
// The workspace runs Clippy's `all` and `pedantic` groups; upstream does
// not, and reformatting to satisfy them would make the copy undiffable
// against the source it has to be checked against. Items this crate never
// calls are kept for the same reason.
#![allow(dead_code, clippy::all, clippy::pedantic)]

pub(crate) mod texture2ddecoder;
