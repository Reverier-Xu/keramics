# Rewrite Plan

## Strategy

This rewrite should be treated as a deliberate major break, not as a compatibility-preserving migration.

Implementation note: the current recommended execution vehicle is now the parallel `keramics-drivers` crate described in `docs/hyperead/06-keramics-drivers-crate-migration-plan.md`.

This document remains useful as a conceptual inventory of the architectural work that must happen, but major new runtime work should not land in `keramics-formats` itself.

Under the updated plan:

- `keramics-drivers` is allowed to expose a second, intentionally different API
- `keramics-drivers` does not need API compatibility with `keramics-formats`
- existing users can stay on deprecated `keramics-formats` until they are ready to migrate
- compatibility pressure should not distort the new architecture

The right implementation strategy is:

- build the new core abstractions first
- port one full complex driver end to end, namely EWF
- then port the rest of the stack onto the same runtime model
- finally remove the legacy `DataStream` world entirely

The rewrite must not be diluted by trying to keep both architectures equally first class for long.

## Hard Policy Decisions

Before implementation starts, the project should explicitly adopt these rules.

1. `DataStream` is legacy and scheduled for removal.
2. `DataStreamReference = Arc<RwLock<dyn DataStream>>` is not allowed in new code.
3. New drivers must be built on immutable `DataSource`-style abstractions compatible with `regressor-storage`.
4. New code must not add `current_offset` to shared domain objects.
5. New code must not populate mapping metadata inside `read()` paths unless it uses explicit concurrency-safe memoization and that choice is justified.
6. Compatibility shims are temporary migration tools, not architectural targets.
7. Keramics must not absorb `StorageSession` or mounted-filesystem orchestration; it remains the lower-level parsing and data-source construction layer.

## Breaking API Inventory

The rewrite should acknowledge its breaking surface explicitly instead of allowing it to remain implicit.

### Core API breaks

- remove `DataStream` as the primary public reading abstraction
- remove `DataStreamReference = Arc<RwLock<dyn DataStream>>` from the main API surface
- replace `open_os_data_stream()` with an immutable source-opening function
- replace macro-centered positioned read helpers with source methods and small utility functions

### Format API breaks

- image types stop implementing the old root stream trait
- partition types stop implementing the old root stream trait
- file-entry APIs stop returning `Arc<RwLock<dyn DataStream>>`
- `read_data_stream()` style methods become `open()`, `parse()`, or `open_source()` style methods depending on the object role
- `get_data_stream()` becomes `open_source()` or `open_cursor()`

### Semantic API breaks

- the shared object is no longer the cursor
- `seek()` mutates only local cursor state
- returned file content handles become safely shareable immutable sources
- cache lifetime becomes image-level or source-level infrastructure, not an accidental consequence of retaining a mutable reader object

## Driver Migration Matrix

The refactor plan should treat each driver family as a destination mapping problem.

| Driver family | Current shape | Target shape | Primary workstream |
| --- | --- | --- | --- |
| Core OS I/O | lock-guarded mutable stream | immutable positioned source | 1 |
| Partitions | mutable slice readers | metadata plus `SubrangeSource` | 4 |
| Extent-backed file systems | custom block streams | metadata plus `ExtentMapSource` | 5 |
| File-system compression | custom compressed streams | metadata plus `CompressionUnitSource` | 5 |
| EWF | mutable image plus caches plus cursor | immutable image plus `ChunkedCompressedSource` | 3 |
| Split and banded images | mutable image readers | metadata plus `SegmentedSource` | 6 |
| Layered block images | mutable child or parent readers | metadata plus `LayeredBlockSource` | 6 |

## Workspace Alignment Rule

The Keramics rewrite should intentionally reduce, not increase, abstraction divergence inside the workspace.

That means:

- adopt a `DataSource` shape that matches `regressor-storage` closely enough that bridges are trivial
- keep the bridge layer explicit only for migration and for legacy consumers
- avoid introducing a second long-term root trait whose only difference is naming

## Workstreams

## Workstream 1: Rewrite `keramics-core` I/O Foundation

### Objective

Replace the root I/O abstraction.

### Deliverables

- `DataSource` trait
- `SharedDataSource = Arc<dyn DataSource>`
- capability types equivalent to `regressor-storage` (`read_concurrency`, `seek_cost`, `preferred_chunk_size`)
- `Cursor` implementation for sequential access
- `OsDataSource`
- `MemorySource`
- `SubrangeSource`
- replacement for current helper macros with real positioned-read helpers or ordinary method calls
- compatibility bridge to and from legacy `DataStream`

### Required removals or demotions

- `DataStream` must stop being the primary contract
- `open_os_data_stream()` must stop returning a lock-guarded mutable stream handle
- `FakeDataStream` must be replaced or reduced to legacy test compatibility only

### Exit criteria

This workstream is complete when:

- core concurrent read tests pass against a shared OS-backed source
- two cursors over one source can move independently
- no core positioned read uses `seek + read` on shared state

## Workstream 2: Build a Shared Runtime Source Library in `keramics-formats`

### Objective

Create reusable logical source implementations so that format drivers no longer need one custom mutable stream type per format.

### Deliverables

- `ExtentMapSource`
- `ChunkedCompressedSource`
- `LayeredBlockSource`
- `ZeroSource` or sparse-region support
- shared cache interfaces and default implementations
- immutable mapping structures or builders for them

### Notes

This is the workstream that turns the redesign from "new core I/O" into "new format runtime architecture."

### Exit criteria

This workstream is complete when:

- partitions, file data sources, and image layers can all be represented without implementing a new root stream trait for each concrete format type
- the runtime library is sufficient to express the full EWF design without fallback to legacy mutable stream objects

## Workstream 3: Rewrite EWF End to End

### Objective

Use EWF as the first complete driver on the new architecture.

### Deliverables

- immutable `EwfImage`
- `EwfSegmentDescriptor`, `EwfChunkDescriptor`, and supporting metadata objects
- `EwfSegmentRepository`
- `EwfChunkCache`
- `EwfSource`
- `EwfCursor`
- full parse-time chunk map materialization
- concurrency test suite for EWF

### Specific deletion targets

- `impl DataStream for EwfImage`
- shared `current_offset` on `EwfImage`
- runtime `segment_file_cache: LruCache<u16, EwfFile>`
- runtime `chunk_cache: LruCache<u64, Vec<u8>>` in its current form
- long-lived `EwfFile` objects as runtime read dependencies

### Exit criteria

This workstream is complete when the EWF driver satisfies the acceptance criteria in `docs/hyperead/03-ewf-driver-redesign.md`.

## Workstream 4: Port Partition and Slice Types

### Objective

Eliminate trivial mutable stream wrappers.

### Scope

- MBR partitions
- GPT partitions
- APM partitions
- any contiguous view type in image drivers

### Deliverables

- immutable partition metadata objects
- `open_source(parent_source)` returning `SubrangeSource`
- no `impl DataStream` on partition metadata objects

### Exit criteria

This workstream is complete when partition reading no longer depends on any shared cursor state anywhere in the stack.

## Workstream 5: Port File-System Data Paths

### Objective

Make file systems return immutable content sources instead of building new lock-guarded mutable readers.

### Scope

- ext
- FAT
- HFS
- NTFS
- XFS

### Deliverables

- file systems store immutable metadata plus underlying source handles
- file entries return `Arc<dyn DataSource>` or source-opening APIs
- inline and resident data use `MemorySource`
- extent-backed data use `ExtentMapSource`
- compressed NTFS streams are rebuilt on top of the new runtime source model

### Architectural rule

`get_data_stream()` should be replaced with `open_source()` or a similarly named API that makes the new semantics explicit.

### Exit criteria

This workstream is complete when file-system file access no longer allocates `Arc<RwLock<dyn DataStream>>` objects in the new path.

## Workstream 6: Port Layered and Sparse Image Drivers

### Objective

Move remaining image drivers to immutable mapping plus immutable child or parent sources.

### Scope

- Split raw
- Sparsebundle
- Sparseimage
- QCOW
- VHD
- VHDX
- VMDK
- PDI
- UDIF

### Special rules

- parent/backing relationships must become immutable source delegation, not `Arc<RwLock<MutableFile>>` recursion
- runtime map discovery must either be fully precomputed or use explicit once-initialized metadata page caches

### Exit criteria

This workstream is complete when none of the layered image readers require a mutable image object to perform steady-state reads.

## Workstream 7: Delete the Legacy Architecture

### Objective

Remove the old model so that the codebase no longer has two competing architectural centers.

### Deletion targets

- `DataStream` as a first-class contract
- `DataStreamReference`
- legacy helper macros that acquire write locks for reads
- format-specific `impl DataStream` blocks that were only needed because the old model demanded it
- the current unsynchronized `LruCache` where it is no longer justified

### Exit criteria

This workstream is complete when:

- the mainline API is entirely based on immutable sources and local cursors
- legacy adapters, if any still exist, are clearly quarantined and no production driver depends on them

## Recommended Sequencing

The workstreams should be executed in this order.

1. Core I/O foundation
2. Shared runtime source library
3. EWF rewrite
4. Partition and slice port
5. File-system data path port
6. Layered image driver port
7. Legacy deletion and cleanup

This order matters.

EWF should not be rewritten before the runtime source library exists, because otherwise EWF would invent private abstractions that the rest of the crate would later need to undo.

## Cutover Style

The recommended cutover is a broad, branch-based rewrite rather than a long-lived dual stack in the default branch.

### Recommended approach

1. Create the new core and runtime abstractions.
2. Implement the full EWF rewrite.
3. Port representative partitions and file systems.
4. Port the remaining image formats.
5. Replace the public API at once.
6. Delete the legacy path quickly.

### Explicitly not recommended

- keeping `DataStream` as the canonical API and wrapping new sources inside it everywhere
- porting one method at a time while the old and new runtime styles coexist indefinitely
- keeping parallel implementations of every driver for a long time

## Risk Register

### Risk 1: file descriptor pressure for large multi-segment images

Mitigation:

- support eager-open and lazy-open repository modes
- keep the repository abstraction separate from the EWF parser and source model

### Risk 2: memory growth in shared chunk caches

Mitigation:

- byte-bounded caches
- explicit cache policies
- per-format tuning knobs only after the generic infrastructure exists

### Risk 3: hidden legacy assumptions in tests

Mitigation:

- rewrite tests to assert data equivalence and concurrency semantics rather than lock behavior or specific helper types

### Risk 4: accidental reintroduction of shared cursor state

Mitigation:

- code review rule: no `current_offset` on shared domain objects
- code review rule: no `Option<DataStreamReference>` in new code
- code review rule: no new runtime map mutation inside steady-state `read_at()` paths without explicit design approval

### Risk 5: over-generalizing too early

Mitigation:

- build only the runtime source families that are immediately justified by real drivers
- use EWF as the first proving ground before abstracting further

## Definition of Done

The rewrite is done only when all of the following are true.

1. The public read architecture is based on immutable sources and local cursors.
2. EWF fully uses the new model.
3. Partition wrappers no longer implement the old mutable stream model.
4. File systems return immutable content sources.
5. Layered image formats use immutable source delegation instead of nested mutable readers.
6. The codebase contains no central shared-cursor read path.
7. Concurrency tests and benchmarks prove that reads can proceed in parallel on the same opened image.
