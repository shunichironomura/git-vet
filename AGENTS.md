# Instruction for Coding Agents

## No fallbacks, workarounds, or partial correctness

- This project is not yet published, so breaking changes are acceptable for simpler/clever/clean design and implementation.
  - DO NOT implement fallbacks/workarounds/partial patch for backward compatibility or any other purposes. If a major design change, such as data model modification, is needed, just make the change and update the existing codebase accordingly.

## Type Safety: Encode Semantics in Types, Not Conventions

- **Functional core, imperative shell** The core logic should be as functional as possible with semantics-encoding types and use as little mutable pattern as possible.
- **No flat-string encodings of structured data.**
- **No string-matched control flow**
- **Stringify only at boundaries.** Rendering for diagnostics, debug output, file/wire serialization, or third-party APIs is fine — but the conversion happens at the boundary, not throughout the functional core. Inside the core, pattern-match on the typed variant.

## Useful resources

- Read `SPEC.md` for the specifications.
- Read `PLAN.md` for the implementation plan of the specifications.
