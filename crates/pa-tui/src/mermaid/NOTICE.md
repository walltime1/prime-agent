# Vendored Mermaid renderer

`../mermaid.rs` and `tests.rs` are derived from
`crates/codegen/xai-grok-markdown/src/mermaid.rs` in https://github.com/xai-org/grok-build
at commit 482711333c7195dc16a272777f86086d615e2afb.
Copyright 2023-2026 SpaceXAI. Licensed under the Apache License, Version 2.0 (see `LICENSE`).

Local modifications: the upstream test module moved to `tests.rs`; Rust 2024 `let` chains were
rewritten as nested `if let` for this edition-2021 workspace; the unused `MermaidArt::plain_lines`
output was removed (the tests derive the same text from `styled_lines`); the flowchart parser keeps
hyphens inside node ids and quoted edge labels, and sequence diagrams keep a `;` that follows a
message's or note's `:`; and the code was conformed to this workspace's rustfmt and clippy
configuration.
