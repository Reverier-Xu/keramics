# Keramics-Drivers Crate Migration Plan

## Decision

The recommended execution strategy is no longer an in-place architectural rewrite of `keramics-formats`.

The recommended strategy is:

1. add a new workspace crate named `keramics-drivers`
2. design its core around a `regressor-storage`-style immutable `DataSource` architecture
3. keep the scope strictly below VFS and session orchestration
4. port format logic from `keramics-formats` into `keramics-drivers` gradually, without taking a dependency on `keramics-formats`
5. switch consumers to `keramics-drivers` when parity and performance are proven
6. deprecate `keramics-formats` once the new crate is complete enough to replace it

This is the lower-risk path because it isolates the new architecture inside a new crate boundary instead of forcing current users of `keramics-formats` to absorb large breakage while the migration is still underway.

## API Compatibility Policy

`keramics-drivers` should be treated as a second API, not as a compatibility-preserving facelift of `keramics-formats`.

That means:

- `keramics-drivers` does not need to preserve `keramics-formats` API shape
- `keramics-drivers` does not need to mimic old naming, type hierarchies, or object lifetimes
- migration pressure from existing `keramics-formats` users must not force the new crate back into the shared-cursor architecture

The role split should be explicit.

- `keramics-formats`
  - deprecated when appropriate
  - still usable by existing users during their migration window
  - old API remains available long enough for an orderly transition
- `keramics-drivers`
  - clean new API
  - optimized for the new architecture rather than for drop-in replacement
  - consumers migrate to it deliberately and follow its new API model

This is an advantage, not a drawback. Because the new crate is parallel rather than in-place, Keramics can optimize the new API for correctness and performance without forcing every current user to migrate immediately.

## Why This Is Better Than Rewriting `keramics-formats` In Place

The in-place rewrite has three major disadvantages.

First, it makes every intermediate step user-visible. A partially migrated `keramics-formats` crate would mix two incompatible architectural centers:

- the old `DataStream` state-machine world
- the new immutable `DataSource` world

Second, it makes regression diagnosis harder. If the old crate is being mutated while also serving as the correctness baseline, it becomes harder to answer a simple question: did behavior change because the architecture improved, or because the migration destabilized the baseline?

Third, it spreads breakage too widely. Existing users of `keramics-formats`, `keramics-vfs`, tools, and any downstream experiments all become exposed to architectural churn before the rewrite is finished.

Creating `keramics-drivers` solves those problems.

- `keramics-formats` stays available as the stable old baseline
- `keramics-drivers` becomes the controlled landing zone for the new architecture
- migration can proceed format by format without destabilizing existing consumers
- deprecation only happens after the new crate has earned replacement status

## Scope and Non-Goals

The new crate should focus on one thing: high-throughput, concurrency-friendly parsing and logical byte-source construction for storage formats.

It should not absorb the higher-level concerns that belong in a mounting or session layer.

### In scope

- immutable positioned-read sources
- capability metadata for read concurrency and seek behavior
- explicit multi-file resolvers
- parser and metadata models for images, partitions, and file systems
- runtime byte-source implementations built from immutable metadata
- shared concurrent caches and decode pipelines
- probe helpers for format detection when needed

### Out of scope

- `StorageSession`
- VFS / virtual path traversal
- mount tables
- recursive auto-mount orchestration
- user-facing driver registry policy
- write-path APIs for modifying images or file systems

The crate is read-optimized and read-focused. Internal cache mutation exists as an implementation detail, but public storage mutation is not part of this migration plan.

## Workspace Strategy

The workspace should eventually contain both crates during the migration window.

- `keramics-formats`
  - frozen baseline
  - only critical bug fixes if absolutely necessary
  - no new architecture work
- `keramics-drivers`
  - new architecture landing zone
  - all new concurrent-read work happens here
  - format logic is ported here one family at a time

Recommended future workspace change:

```toml
[workspace]
members = [
    # existing members...
    "keramics-formats",
    "keramics-drivers",
]
```

The existence of both crates at once is intentional. The migration needs a stable source crate and a destination crate.

## Crate Boundary

`keramics-drivers` should be as self-contained as practical.

### Allowed dependencies

The new crate can depend on stable utility crates that do not pull the old cursor model back in.

Good examples include:

- `keramics-types`
- `keramics-checksums`
- `keramics-compression`
- `keramics-datetime`
- `keramics-encodings`
- `keramics-hashes`
- `keramics-layout-map`
- carefully selected neutral utilities from `keramics-core`

### Dependencies to avoid in the new architecture core

The new crate should not depend on `keramics-formats` at all.

It should also avoid building itself around the legacy `DataStream` parts of `keramics-core`. Using neutral error or byte helpers is fine. Reintroducing `DataStreamReference`, shared cursors, or `seek + read`-style positioned I/O is not.

### Long-term extraction policy

If some primitives prove broadly reusable later, they can be extracted after the migration stabilizes.

Possible future destinations:

- promote neutral abstractions into `keramics-core`
- or create a tiny common crate used by both `keramics-drivers` and higher-level consumers

That extraction is a cleanup step, not a phase-1 requirement.

## Core Architectural Rule

The new crate should follow the `regressor-storage` low-level pattern, but not its session or VFS layers.

The architectural center is:

- immutable `DataSource`
- explicit source capabilities
- local cursors
- immutable metadata
- shared runtime caches
- explicit sibling or segment resolvers

The architectural center is not:

- `DataStream`
- shared mutable cursor objects
- generic mount orchestration
- path-based virtual filesystem policy

## Proposed Top-Level Module Layout

The new crate should be organized around source infrastructure, runtime helpers, and ported format families.

```text
keramics-drivers/
  src/
    lib.rs
    source/
      mod.rs
      data_source.rs
      capabilities.rs
      cursor.rs
      memory.rs
      slice.rs
      observed.rs
      probe_cache.rs
      segmented.rs
      extent_map.rs
      compression_unit.rs
      chunked_compressed.rs
      layered.rs
    resolver/
      mod.rs
      source_resolver.rs
      local.rs
    runtime/
      mod.rs
      cache.rs
      decode.rs
      prefetch.rs
      metrics.rs
    probe/
      mod.rs
      signatures.rs
      scanner.rs
    image/
      ewf/
      splitraw/
      sparsebundle/
      sparseimage/
      qcow/
      vhd/
      vhdx/
      vmdk/
      pdi/
      udif/
    volume/
      mbr/
      gpt/
      apm/
    filesystem/
      ext/
      fat/
      hfs/
      ntfs/
      xfs/
```

The major goal of this layout is to make the runtime model reusable while letting each format family migrate independently.

## Source Layer Design

The source layer is the heart of the new crate.

### Root trait

`keramics-drivers` should define its own `DataSource` abstraction initially.

Conceptually:

```rust
pub trait DataSource: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Error>;
    fn size(&self) -> Result<u64, Error>;
    fn capabilities(&self) -> DataSourceCapabilities;
}
```

This should live in `keramics_drivers::source`, not in `keramics-formats` and not as a phase-1 mutation of the old `DataStream` contract.

### Capability model

Adopt the `regressor-storage` style capability model directly.

- `DataSourceReadConcurrency::{Unknown, Serialized, Concurrent}`
- `DataSourceSeekCost::{Unknown, Cheap, Expensive}`
- `preferred_chunk_size: Option<usize>`
- optional telemetry identity fields if helpful

This is not decoration. It is the mechanism that lets callers and benchmarks distinguish:

- bridge-backed serialized legacy implementations
- from native concurrent implementations

### Standard wrappers

The following should exist early and become the default building blocks:

- `MemoryDataSource`
- `SliceDataSource`
- `ObservedDataSource`
- `ProbeCachedDataSource`
- `SegmentedDataSource`
- `ExtentMapDataSource`
- `CompressionUnitDataSource`
- `ChunkedCompressedDataSource`
- `LayeredDataSource`

These replace the old habit of making each format object itself implement the root byte-stream abstraction.

### Local sequential adapter

If sequential reading is needed, provide a local `DataSourceCursor`.

The shared source stays immutable.

The cursor owns only:

- current position
- optional per-reader scratch buffers
- optional sequential prefetch hints

No shared domain object should own a shared cursor.

## Resolver Layer Design

Multi-file and sibling-aware formats need an explicit resolver layer.

This is where Keramics should differ from `regressor-storage`'s more convenience-oriented `origin_path()` usage.

`keramics-drivers` should prefer explicit correctness.

### Resolver responsibilities

- open sibling segments by logical name
- open related backing files where the format requires it
- remain usable with non-local sources if a caller provides a custom resolver

### Resolver policy

- `origin_path()` may exist as a convenience hint on some concrete data sources
- but correctness for multi-file formats must not depend on `origin_path()` alone
- EWF, VMDK, sparsebundle, and split raw should all continue to rely on explicit resolvers

## Format Family Design Rules

Every migrated format should be split into three layers.

### 1. Parser layer

Consumes one or more `DataSource`s and builds immutable metadata.

### 2. Model layer

Holds the validated immutable metadata structure.

### 3. Runtime source layer

Exposes the logical bytes through one or more `DataSource` implementations built from the immutable model.

This applies equally to:

- images
- partitions
- file systems
- individual file content streams

## Migration Rule: Port Logic, Do Not Wrap `keramics-formats`

The new crate must not depend on `keramics-formats`, not even as a quiet implementation shortcut.

That means:

- do not call `keramics-formats` APIs to parse real workloads
- do not wrap `keramics-formats` image objects as the long-term runtime model
- do not route all new reads through `DataStream` bridges as the actual implementation

The migration method is a one-way logic port.

### Allowed use of old code

`keramics-formats` may be used as:

- a source code reference
- a behavior reference during development
- a temporary comparison oracle outside the new crate if that helps manual verification

### Forbidden use of old code

`keramics-drivers` must not declare a direct dependency on `keramics-formats`, including for production code paths.

The point of the migration is not to build a prettier wrapper around the old architecture. The point is to move the parsing logic into a new architecture.

## Porting Protocol for Each Format

Each migrated format should follow the same protocol.

### Step 1: inventory the old implementation

- list relevant `keramics-formats` source files
- identify parser state, runtime state, caches, and cursor state
- identify where `DataStream` assumptions are embedded

### Step 2: define the destination model in `keramics-drivers`

- immutable metadata types
- runtime `DataSource` types
- capability reporting
- resolver needs
- cache needs

### Step 3: port parsing logic

- copy and adapt the actual parse logic
- remove shared cursor assumptions
- keep temporary parse state local to builders or parsers

### Step 4: port runtime byte serving

- implement native `DataSource` types
- add caches only where needed for performance
- ensure steady-state reads do not mutate metadata

### Step 5: add parity and performance tests

- byte parity
- metadata parity where meaningful
- concurrency correctness
- throughput benchmarks

### Step 6: mark the old format as superseded in the migration tracker

- not removed yet
- but no new architectural work remains on the old module

## Recommended Migration Order

The order should optimize for architectural learning and risk reduction.

## Phase 0: bootstrap the crate

- add `keramics-drivers` to the workspace
- create source, resolver, runtime, and probe modules
- add benchmark and fixture scaffolding
- define the internal review rules that forbid new shared cursor state

## Phase 1: build source primitives

- `MemoryDataSource`
- `SliceDataSource`
- OS-backed local file data source with positioned reads
- `ObservedDataSource`
- `ProbeCachedDataSource`
- `DataSourceCursor`

This phase proves the core `DataSource` model before any complex format port begins.

## Phase 2: build reusable composite sources

- `SegmentedDataSource`
- `ExtentMapDataSource`
- `CompressionUnitDataSource`
- `ChunkedCompressedDataSource`
- `LayeredDataSource`

These are the runtime primitives that the later ports should reuse instead of re-inventing per-format stream types.

## Phase 3: port the easiest structural formats first

- MBR
- GPT
- APM

Why first:

- they are metadata plus simple slices
- they validate the parser/model/runtime split cheaply
- they immediately remove a large amount of unnecessary cursor state from one family of objects

## Phase 4: port the simple multi-part image family

- split raw
- sparsebundle band routing where feasible

Why now:

- this validates segmented-source routing and resolver behavior
- it exercises multi-file logic before moving to chunked compressed formats

## Phase 5: port EWF as the first complex flagship driver

EWF should be the first major image driver fully rewritten in the new crate because it stress-tests the architecture in exactly the areas that matter:

- explicit sibling resolution
- chunk-level immutable mapping
- shared decode caches
- concurrent reads across segments and chunks

The landing zone should be `keramics_drivers::image::ewf`.

## Phase 6: port extent-backed file systems

- ext
- XFS
- FAT
- HFS

These can reuse the extent and slice infrastructure and will validate the file-system-side runtime source model.

## Phase 7: port NTFS

NTFS should come after the simpler file systems because it needs:

- resident data handling
- runlist-backed data
- compression-unit handling
- WoF-specific data paths

NTFS is not the first filesystem to port, but it is the first filesystem that fully stress-tests the compression-unit source family.

## Phase 8: port layered image formats

- QCOW
- VHD
- VHDX
- VMDK
- PDI
- UDIF
- sparseimage

These should come after the generic layered and chunked runtime primitives have proven themselves elsewhere.

## Phase 9: consumer cutover

Once enough coverage exists, switch consuming crates one by one.

Potential future adopters include:

- tools
- VFS layers
- Python bindings
- external experiments using Keramics parsing crates

Consumer cutover should happen only after format parity and concurrency behavior are acceptable.

## Phase 10: deprecate `keramics-formats`

`keramics-formats` can be deprecated only after all of the following are true.

- the required format set exists in `keramics-drivers`
- parity tests and real fixtures pass
- concurrency benchmarks show the new crate is meaningfully better where expected
- dependent crates have migration paths
- no critical user path still relies exclusively on `keramics-formats`

Deprecation should happen before deletion.

Recommended sequence:

1. document `keramics-formats` as legacy
2. stop adding new features there
3. switch internal documentation and examples to `keramics-drivers`
4. migrate first-party consumers
5. only then consider removal on a later major version boundary

## EWF-Specific Implication of the New Crate Strategy

Under the new plan, the EWF redesign should not be implemented in `keramics-formats/src/ewf`.

Instead:

- the old EWF module stays untouched except for unavoidable maintenance
- the new implementation lands in `keramics_drivers::image::ewf`
- all chunk map, cache, resolver, and concurrent-read logic is authored natively in the new crate

This is important because EWF is the format most likely to pressure the architecture into large internal rewrites. Putting that work in `keramics-drivers` preserves the old implementation as a stable baseline until the new one is proven.

## Compatibility Strategy During the Migration Window

Because the rewrite is isolated in a new crate, compatibility becomes a consumer-level concern rather than a format-level constraint.

Recommended policy:

- `keramics-drivers` is allowed to be clean and breaking internally
- compatibility adapters, if required, should live at the consumer boundary
- do not distort the new crate just to preserve the old `DataStream` worldview
- do not treat `keramics-drivers` as a drop-in replacement crate

If some consumer still needs a legacy stream-like interface during cutover, provide a thin adapter outside the new architecture core. The new crate itself should stay centered on immutable `DataSource` semantics.

## Risks and Mitigations

## Risk 1: duplicated logic during the migration window

This is real and unavoidable.

Mitigation:

- accept duplication as a temporary migration cost
- keep the destination architecture cleaner rather than prematurely deduplicating across old and new crates

## Risk 2: port drift from the old behavior

Mitigation:

- format-by-format parity tests
- fixture-based validation
- explicit migration checklists per format family

## Risk 3: the new crate accidentally re-imports the old architecture

Mitigation:

- no dependency on `keramics-formats`
- no root dependence on `DataStream`
- review rule: no shared `current_offset` fields in shared domain objects
- review rule: no positioned reads implemented as shared `seek + read`

## Risk 4: abstraction over-generalization too early

Mitigation:

- build only the runtime source families justified by immediate ports
- use MBR/GPT/APM, split raw, and EWF as staged proving grounds

## Risk 5: consumer confusion about which crate is canonical

Mitigation:

- document clearly that `keramics-formats` is the frozen baseline and `keramics-drivers` is the migration target
- switch documentation and examples deliberately once `keramics-drivers` is ready

## Success Criteria

The crate strategy is successful only when all of the following are true.

1. `keramics-drivers` has a stable `DataSource`-centered architecture.
2. The new crate does not depend on `keramics-formats`.
3. EWF and the core partition or filesystem families run natively in `keramics-drivers`.
4. Shared-cursor state is absent from the new runtime model.
5. Benchmarks demonstrate actual concurrency benefits on the formats that motivated the rewrite.
6. First-party consumers can move to `keramics-drivers` without requiring the new crate to inherit the old `DataStream` architecture.
7. `keramics-formats` can be deprecated with confidence rather than abandoned prematurely.

## Final Recommendation

The new default plan should be:

- do not rewrite `keramics-formats` in place
- create `keramics-drivers`
- give it a `regressor-storage`-style low-level source architecture
- port format logic into it natively, one family at a time
- let it expose a clean second API without `keramics-formats` compatibility constraints
- use `keramics-formats` only as the frozen source baseline during the transition
- deprecate `keramics-formats` only after the new crate is truly ready

This is the cleanest path to a high-performance concurrent-read architecture with the smallest migration blast radius.
