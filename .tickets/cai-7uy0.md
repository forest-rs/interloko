---
id: cai-7uy0
status: open
deps: []
links: []
created: 2026-05-15T18:05:29Z
type: feature
priority: 2
assignee: Bruce Mitchener
tags: [grammar, deferred]
---
# Design source-authored grammar declarations

Define how Loko or environment source can declare grammar extensions without pretending the current Rust-hosted adapters are user-authored syntax.

## Design

Keep declarations structural and inspectable. They should produce grammar MOP records, not execute host code directly. This depends on capability policy, hygiene, and structural semantic-action results.

## Acceptance Criteria

A future goal can state supported declaration syntax, owned modules, capability checks, and tests for inspection/provenance before implementation starts.

