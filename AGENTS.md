## Forest Engineering Tenets

These tenets govern all forest-rs projects. They are non-negotiable.

1. **We Build to Endure.** Systems that are difficult to outgrow, difficult to entangle, easy to reason about, easy to measure. Optimize for structural strength, not short-term applause.
2. **Modularity Is Power.** Every subsystem: narrow responsibility, minimal dependency surface, replaceable internals, stable API. Monoliths are a last resort.
3. **Incrementalism Everywhere.** Full rebuilds are failure modes. Deltas over rewrites. Patches over full uploads. Caches over recomputation. Budgeted work over spikes.
4. **Introspection Is Non-Optional.** If we cannot measure it, we cannot improve it. Every system exposes: time (CPU + GPU), memory (live + fragmentation), work units, bandwidth. Diagnostics are architecture.
5. **Explicit Over Implicit.** No hidden state. No invisible scheduling. No accidental lifetime behavior. No magical performance characteristics. Predictability is a feature.
6. **Long-Term > Short-Term.** Clean structure over clever shortcuts. Extensibility over demo velocity. Architectural leverage over temporary wins.
7. **Replaceability Is a Constraint.** Major subsystems tolerate different backends, techniques, allocators, platforms. If something cannot be replaced, it must be small and contained.
8. **Calm Interfaces.** Internal complexity may be aggressive. Public APIs must be calm: boring, obvious, stable, intentional.
9. **No Sacred Subsystems.** Refactor without attachment. Remove complexity when possible. Evolve forward.


## Non-negotiables (Definition of Done)

- `cargo fmt` passes.
- `cargo clippy` passes (`-D warnings`).
- `cargo doc` passes.
- Public APIs are documented (types/functions; public fields/variants where it matters).
- Tests updated/added when behavior changes.
