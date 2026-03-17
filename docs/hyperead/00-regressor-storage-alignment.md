# Regressor-Storage Alignment

## Why This Changes the Recommendation

The earlier `hyperead` documents intentionally designed a new immutable random-access architecture from first principles. After reviewing `../regressor`, the right conclusion is that Keramics should not invent a brand-new foundational vocabulary if a strong in-house reference already exists.

`regressor-storage` already demonstrates a simpler and more grounded abstraction set:

- a single root read trait, `DataSource`, centered on `read_at(&self, offset, buf)` and `size(&self)`
- explicit capability metadata such as read concurrency and seek cost
- small reusable wrappers such as `SliceDataSource`, `ObservedDataSource`, and `ProbeCachedDataSource`
- OS-backed positioned reads where the platform supports them
- higher-level orchestration built on top of that base without forcing the low-level storage model itself to become stateful

The most important observation is not only that this works, but that `regressor-storage` already contains a bridge layer showing exactly where current Keramics is wrong.

In `../regressor/crates/storage/src/drivers/keramics_bridge.rs`, `stream_to_data_source()` must wrap Keramics `DataStreamReference` inside a mutex and report:

- `DataSourceReadConcurrency::Serialized`
- `DataSourceSeekCost::Expensive`

That bridge is effectively a live architectural diagnosis. It proves that the current Keramics stream model can be adapted into a better abstraction, but only by honestly admitting that the adapted source is serialized and cursor-bound.

Therefore the more reasonable plan is not:

- invent a totally separate root trait unrelated to the rest of the workspace

but rather:

- make Keramics adopt a `DataSource`-style core abstraction compatible with `regressor-storage`
- keep Keramics lower-level than `regressor-storage`
- reuse the same mental model, capability model, and migration bridge strategy
- land the first full implementation in a new `keramics-drivers` crate rather than rewriting `keramics-formats` in place

Under the updated migration plan, `keramics-drivers` is intentionally allowed to expose a second API. It does not need API compatibility with `keramics-formats`; the old crate can remain available as a deprecated baseline while consumers migrate on their own schedule.

## What Keramics Should Adopt Directly

## 1. A `DataSource`-style root trait

Keramics should replace the `DataStream`-centric core with a `DataSource`-style trait whose primary contract is immutable positioned reading.

Conceptually:

```rust
pub trait DataSource: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace>;
    fn size(&self) -> Result<u64, ErrorTrace>;

    fn capabilities(&self) -> DataSourceCapabilities {
        DataSourceCapabilities::default()
    }

    fn telemetry_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn origin_path(&self) -> Option<&Path> {
        None
    }
}
```

This is materially better than the current Keramics root contract because:

- `read_at()` is the first-class operation, not an emulation of `seek + read`
- callers can share a source without sharing a cursor
- concurrency is expressed by the trait contract rather than being hidden behind `RwLock` behavior

## 2. Capability metadata

Keramics should adopt capability reporting in the same spirit as `regressor-storage`.

The minimum useful set is:

- `DataSourceReadConcurrency::{Unknown, Serialized, Concurrent}`
- `DataSourceSeekCost::{Unknown, Cheap, Expensive}`
- `preferred_chunk_size: Option<usize>`

This matters for two reasons.

First, it makes the architecture honest during migration. Legacy bridge-backed sources can explicitly report `Serialized`, while new native implementations can report `Concurrent`.

Second, it lets higher-level consumers adapt behavior. `regressor` already uses these capabilities to choose sequential versus parallel hashing and to size chunking heuristics. Keramics itself, Keramics tools, and downstream consumers can do the same.

## 3. Reusable wrappers instead of ad hoc stream objects

Keramics should adopt the same style of thin reusable wrappers that `regressor-storage` already proves out.

The most important ones are:

- `SliceDataSource`
  - for partitions, contiguous subranges, and embedded regions
- `ObservedDataSource`
  - for telemetry, benchmarks, and profiling
- `ProbeCachedDataSource`
  - for small repeated header reads during format probing and scanning

These wrappers are more valuable than many bespoke `impl DataStream` blocks because they turn recurring patterns into explicit infrastructure.

## 4. OS-backed positioned I/O

Keramics should adopt the same practical operating-system strategy as `regressor-storage` local sources.

On Unix and Windows:

- use true positioned reads (`FileExt::read_at`, `seek_read`, or equivalent)

On fallback platforms:

- use an internal mutex only where necessary
- report serialized capabilities honestly

This is a better plan than pretending all backends are equally concurrent.

## What Keramics Should Not Copy

## 1. Do not copy `StorageSession` into Keramics

`regressor-storage` owns:

- session orchestration
- mount tables
- driver registry
- virtual path traversal
- mounted filesystem semantics

Keramics should not absorb those responsibilities.

Keramics is the lower-level parser and data-source construction layer. `regressor-storage` or other consumers can continue to own:

- mounting
- navigation
- recursive detection
- virtual paths
- user-facing session policy

This boundary is important. The right outcome is architectural compatibility, not crate duplication.

## 2. Do not rely on `origin_path()` for correctness

`regressor-storage` uses `origin_path()` as a useful hint when a source originates from a host file. That is appropriate for a mounting layer.

Keramics must be stricter.

For multi-file formats such as:

- EWF
- VMDK
- sparsebundle
- split raw

the primary correctness mechanism should remain an explicit resolver abstraction, not host-path inference. `origin_path()` can exist as an optimization hint or convenience, but it must not become the only way to resolve sibling segments.

This is one of the major differences between a low-level format library and a mounting/session layer.

## Revised Keramics Core Architecture

## Core types

`keramics-core` should provide:

- `DataSource`
- `DataSourceCapabilities`
- `DataSourceReadConcurrency`
- `DataSourceSeekCost`
- `SharedDataSource = Arc<dyn DataSource>`
- `SliceDataSource`
- `ObservedDataSource`
- `ProbeCachedDataSource`
- `DataSourceCursor`

`DataSourceCursor` is the local sequential adapter, not the shared storage object. It exists for code that still wants `Read + Seek` style local semantics, but its cursor is local to the cursor instance.

## Legacy bridge module

Keramics should adopt a migration bridge very similar to `regressor-storage`.

The bridge should be explicitly temporary and live in a clearly named compatibility module.

It should provide both directions:

- `data_source_to_stream(source: Arc<dyn DataSource>) -> DataStreamReference`
- `stream_to_data_source(stream: DataStreamReference) -> Arc<dyn DataSource>`

The second direction is especially important because it lets unported Keramics readers participate in the new architecture while still advertising serialized behavior.

This gives the rewrite a practical migration spine instead of forcing an all-or-nothing cutover on day one.

## Revised `keramics-formats` Architecture

Each format module should be split into three roles.

## 1. Parser and metadata model

These types own:

- validated headers
- geometry
- extent and chunk descriptors
- directory, inode, and partition metadata
- integrity metadata

These objects are immutable after open or parse completes.

## 2. Data-source opening layer

These APIs convert metadata plus parent sources into immutable `DataSource`s.

Examples:

- partitions open as `SliceDataSource`
- extent-backed files open as `ExtentDataSource`
- compressed file streams open as `CompressionUnitDataSource`
- EWF media opens as `EwfMediaDataSource`
- layered images open as `LayeredDataSource`

The key point is that the runtime byte source is separate from the parsed metadata object.

## 3. External mounting or navigation layer

This stays outside Keramics proper.

`regressor-storage` can keep turning Keramics objects into mounted filesystems and navigable sessions. Keramics itself should stop at:

- parse the structure
- expose metadata
- open immutable byte sources

## Concrete Revised Recommendations

## Partitions

The `regressor-storage` MBR and GPT drivers are a strong proof of the right pattern.

They parse partition metadata, then expose partitions as `SliceDataSource` over the original source rather than using the Keramics partition object itself as the runtime reader.

Keramics should do the same internally.

That means:

- `MbrPartition`, `GptPartition`, and `ApmPartition` should become metadata records
- `open_source(parent: Arc<dyn DataSource>)` should return `SliceDataSource`
- shared partition objects should no longer own `current_offset`

## Extent-backed files

The `regressor-storage` XFS data-source layer is another strong reference. It models:

- inline data as a direct in-memory source
- extent-backed data as a `DataSource` that reads through immutable extent metadata and a parent source

Keramics should move ext, FAT, HFS, NTFS non-compressed runs, and XFS toward the same model.

The destination is not one custom stream type per file system. The destination is one common extent-backed data-source pattern with filesystem-specific extent descriptors.

## EWF

The revised EWF target should be:

- `EwfImage` as immutable metadata
- `EwfMediaDataSource` implementing `DataSource`
- `EwfSegmentResolver` as an explicit dependency for multi-file resolution
- `EwfChunkCache` as shared concurrent runtime infrastructure

Capability reporting should be explicit.

### During migration

If `EwfMediaDataSource` still wraps the legacy mutable `EwfImage` through a bridge, it must report:

- `read_concurrency = Serialized`
- `seek_cost = Expensive`
- `preferred_chunk_size = Some(chunk_size)`

### End state

When EWF is fully rewritten around immutable chunk descriptors and segment repositories, it should report:

- `read_concurrency = Concurrent`
- `seek_cost = Expensive`
- `preferred_chunk_size = Some(chunk_size)`

This is a much better contract than the current one because it gives downstream consumers accurate information even before every format is fully ported.

## Revised Migration Plan

The earlier `hyperead` plan should be adjusted to follow the bridge strategy that `regressor-storage` already validates.

### Phase 1: Add `DataSource` to `keramics-core`

- add the trait and capability types
- add OS-backed positioned source support
- add `SliceDataSource`, `ObservedDataSource`, and `ProbeCachedDataSource`
- add the compatibility bridge to and from legacy `DataStream`

### Phase 2: Move probing and scanning to `DataSource`

- port `FormatScanner` and any probe paths to immutable positioned reads
- use small probe-window caching where beneficial

### Phase 3: Port partitions and contiguous slices

- MBR
- GPT
- APM
- any other pure subrange abstraction

These are the lowest-risk wins and immediately delete a large amount of pointless cursor state.

### Phase 4: Port file content sources

- inline or resident data to in-memory sources
- extent-backed files to extent data sources
- compressed file streams to compression-unit data sources

### Phase 5: Rewrite EWF natively

- keep the resolver explicit
- eliminate `EwfImage` shared cursor state
- materialize immutable chunk descriptors
- add real concurrent chunk caching

### Phase 6: Port layered image drivers

- QCOW
- VHD/VHDX
- VMDK
- PDI
- UDIF
- split raw
- sparse image families

### Phase 7: Remove `DataStream` as the architectural center

- bridge remains only if narrowly required for legacy compatibility
- production drivers should no longer depend on it

## Desired End State with `regressor-storage`

The best end state is not two independently evolving root I/O abstractions.

The best end state is one of these:

1. Keramics adopts a trait shape intentionally source-compatible with `regressor-storage::DataSource`.
2. The shared `DataSource` abstraction is eventually extracted into a tiny common crate used by both projects.

Either outcome is better than maintaining:

- `DataStream` in Keramics
- `DataSource` in Regressor
- and a permanent lock-based bridge between them

## Decision Summary

The revised recommendation is therefore:

- keep the earlier immutable, concurrent architecture goals
- replace the bespoke root vocabulary with a `regressor-storage`-style `DataSource` core
- preserve explicit resolver-based multi-file handling because Keramics is lower-level than `StorageSession`
- use bridge-backed capability reporting as the migration backbone
- let `regressor-storage` continue to own mounting and virtual filesystem orchestration above Keramics

This is the most practical and internally consistent route to true concurrent reads in Keramics.
