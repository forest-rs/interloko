---
id: cai-rh0v
status: open
deps: []
links: []
created: 2026-05-15T18:05:37Z
type: feature
priority: 2
assignee: Bruce Mitchener
tags: [grammar, deferred]
---
# Design structural semantic actions

Define the semantic-action layer for staged grammars so actions emit structural results such as frontend forms or grammar deltas instead of arbitrary host side effects.

## Design

Actions should be deterministic, traceable, and visible in grammar parse reports. They should not call general host code unless a narrow primitive is explicitly allowed by capability policy.

## Acceptance Criteria

A future design note names action result types, trace records, failure diagnostics, and the first action slice before implementation starts.

