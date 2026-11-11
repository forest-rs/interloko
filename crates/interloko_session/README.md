# Retained Interloko sessions

`interloko_session` is a small `no_std + alloc` embedding boundary over accepted
compiler artifacts. A session owns its accepted source, host declarations,
compilation and revision. It exposes environment-object discovery, compiler
inspection, typed scalar calls, domain mutation and rewind.

```rust
use interloko_compiler::session::FunctionValue;
use interloko_session::Session;

let mut session = Session::new("answer.loko", "define function answer() 42 end;")?;
assert_eq!(session.revision(), 0);
assert_eq!(
    session.call("answer", &[], None).expect("accepted function").value,
    FunctionValue::Integer(42),
);

// A rejected candidate retains accepted code and its revision.
assert!(session.replace_source(0, "define function answer() missing end;").is_err());
assert_eq!(session.revision(), 0);

session.replace_source(0, "define function answer() 43 end;")?;
assert_eq!(session.revision(), 1);
# Ok::<(), interloko_session::SessionError>(())
```

## Editing and inspection

Every edit supplies the expected accepted revision. Stale requests fail before
changing state. Successful source replacement, mutation and rewind advance that
revision; rejected operations preserve it. Source replacement compiles a
candidate before publication and retains the same explicit host registry.

`objects`, `inspect` and `dump` expose existing compiler evidence. An EOP selector
is scoped to the current compilation: rediscover selectors after accepted edits.
The session revision is not a persistent object identity or a runtime node id.
Compiler types remain available from `interloko_compiler` rather than being
duplicated in this crate.

Use `with_host_registry` to supply host declarations and `call_with_host` to supply
an execution handler. The session does not grant authority over host effects.
Calls execute accepted code without advancing the source revision; their optional
traces come from the existing compiler/runtime interface.

## Boundaries

- Replacement starts a new mutation base and clears the old mutation log only
  after successful compilation. Mutation replay is an explicit caller decision.
- Rewind restores compiler mutation history, not external effects or VM heap state.
- Source replacement recompiles the candidate. This wrapper does not add
  incremental compilation, a scheduler or a persistent runtime image.
- Construction uses the compiler's builtin grammar environment. Custom staged
  grammar registry injection is not exposed by this initial session wrapper.
- Filesystem, transport, UI, document composition and execution scheduling belong
  to callers. This crate has no notebook, graph or Understory dependency.
