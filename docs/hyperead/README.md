# Hyperead

This directory contains a full redesign proposal for replacing the current cursor-driven, state-machine based read model in `keramics-formats` with a truly concurrent read architecture.

The proposal is intentionally radical.

- It does not optimize for API compatibility.
- It does not optimize for minimal change.
- It does not optimize for incrementalism unless incremental steps help implementation discipline.
- It assumes that a major, breaking rewrite is acceptable and preferable.

The document set is organized as follows:

0. `docs/hyperead/00-regressor-storage-alignment.md`
   - The updated recommended direction after reviewing `../regressor` and `regressor-storage`.
   - Explains what Keramics should copy directly, what it should not copy, and how the migration should use a `DataSource`-style bridge.
   - This is now the best starting point for implementation planning.

1. `docs/hyperead/01-current-architecture-and-failure-modes.md`
   - Why the current model cannot truly support concurrent reads.
   - What is structurally wrong in `keramics-core` and how that pattern spreads through `keramics-formats`.
   - Why EWF is the clearest example of the problem.

2. `docs/hyperead/02-target-architecture.md`
   - The new core architecture.
   - The replacement for `DataStream`, `DataStreamReference`, shared cursors, and mutable runtime readers.
   - The reusable building blocks that should be shared by all drivers.

3. `docs/hyperead/03-ewf-driver-redesign.md`
   - The detailed EWF-specific design.
   - The immutable metadata model, segment repository, chunk descriptors, caches, and runtime read path.
   - The driver that should be used as the first full implementation of the new model.

4. `docs/hyperead/04-rewrite-plan.md`
   - The original conceptual refactor inventory.
   - Still useful for architectural work decomposition, but superseded as the main execution vehicle by the `keramics-drivers` plan.

5. `docs/hyperead/05-validation-and-benchmarks.md`
   - The correctness, concurrency, stress, and performance plan.
   - The acceptance gates that must be met before the rewrite is considered complete.

6. `docs/hyperead/06-keramics-drivers-crate-migration-plan.md`
   - The current recommended execution plan.
   - Defines `keramics-drivers` as a parallel, intentionally separate API rather than a compatibility layer over `keramics-formats`.
   - Explains the staged porting plan and the eventual deprecation path for `keramics-formats`.

7. `docs/hyperead/07-next-phase-development-plan.md`
   - A checkpoint document comparing the current `keramics-drivers` implementation against the migration plan and `keramics-formats` coverage.
   - Recommends the next execution phase: finish shared runtime primitives first, then use EWF to validate the architecture before a broader parity push.

Recommended reading order:

1. Read `docs/hyperead/06-keramics-drivers-crate-migration-plan.md` first.
2. Read `docs/hyperead/00-regressor-storage-alignment.md` second.
3. Read the failure analysis third.
4. Read the target architecture fourth.
5. Read the EWF redesign fifth.
6. Read the rewrite plan sixth.
7. Read the next-phase plan seventh.
8. Use the validation document as the implementation completion checklist.
