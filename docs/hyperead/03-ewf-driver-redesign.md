# EWF Driver Redesign

## Why EWF Should Be the First Full Rewrite

EWF is the strongest candidate for the first end-to-end implementation of the new architecture.

Reasons:

- the current driver clearly exposes every major problem in the old model
- the logical chunk map is fully derivable at open time
- the driver contains both metadata parsing and expensive runtime decoding, which makes cache boundaries easy to define
- the format is multi-segment and chunked, so solving it well establishes patterns that other image drivers can reuse

The purpose of this document is to describe the target EWF driver as if the legacy design did not exist.

Under the updated migration strategy, this design should land in `keramics-drivers`, not as an in-place rewrite of `keramics-formats::ewf`. It is also free to expose a new API that matches the new architecture rather than the legacy `keramics-formats` API.

## Design Goals

The rewritten EWF driver must satisfy all of the following.

1. Many threads must be able to read different logical offsets from the same opened EWF image concurrently.
2. Many cursors must be able to advance independently over the same opened EWF image.
3. Segment metadata and chunk descriptors must be immutable after parse completes.
4. Segment file opening, decompression, and caching must be explicit runtime services rather than accidental fields on the cursor object.
5. The runtime read path must never discover new chunk mapping information.
6. The driver must correctly model:
   - multiple segment files
   - stored and compressed chunks
   - section inventory and validation
   - EnCase offset overflow behavior for large segment files
   - header and header2 values
   - hash and digest metadata
   - error2 metadata

## Top-Level Object Model

The rewritten driver should be split into immutable metadata, shared runtime services, and local cursors.

### Proposed type inventory

| Type | Kind | Responsibility |
| --- | --- | --- |
| `EwfImage` | immutable domain object | public parsed image object; exposes metadata and source opening |
| `EwfGeometry` | immutable value | media size, sector size, chunk size, number of chunks |
| `EwfSegmentDescriptor` | immutable descriptor | one segment file, its identity, file size, and section inventory |
| `EwfSectionDescriptor` | immutable descriptor | normalized section information used for validation and debugging |
| `EwfChunkDescriptor` | immutable descriptor | mapping from logical chunk index to stored chunk bytes |
| `EwfHeaderCatalog` | immutable metadata | merged header/header2 values |
| `EwfIntegrityCatalog` | immutable metadata | md5, sha1, error2 ranges, parse-time validations |
| `EwfSegmentRepository` | concurrent runtime service | opens and serves per-segment `DataSource`s |
| `EwfChunkCache` | concurrent runtime service | caches decompressed chunk payloads |
| `EwfMediaDataSource` | immutable logical source | implements `DataSource` for the full logical image |
| `EwfCursor` | local adapter | sequential cursor over `EwfMediaDataSource` |

`EwfImage` should own an `Arc<EwfRuntime>` containing the repository and the shared caches, but `EwfImage` itself should remain logically immutable.

## Key Structural Decisions

### Decision 1: `EwfImage` no longer implements the old root stream trait

The public image type should no longer be the sequential reader. It should be the parsed immutable domain object.

Recommended public shape:

```rust
impl EwfImage {
    pub fn open(source_resolver: Arc<dyn SourceResolver>, file_name: &PathComponent)
        -> Result<Self, ErrorTrace>;

    pub fn open_source(&self) -> Arc<dyn DataSource>;

    pub fn open_cursor(&self) -> EwfCursor;
}
```

### Decision 2: replace `BlockTree<EwfBlockRange>` with a dense chunk index

The current EWF implementation uses a generic `BlockTree<EwfBlockRange>`. That is no longer the best fit.

EWF has fixed logical chunking once geometry is known:

- `chunk_index = logical_offset / chunk_size`
- `chunk_relative_offset = logical_offset % chunk_size`

Therefore the natural runtime structure is:

```rust
Arc<[EwfChunkDescriptor]>
```

This is superior for EWF because:

- lookup is direct and lock free
- the mapping is immutable
- the format already defines chunk granularity
- the driver no longer needs a tree structure for normal reads

`BlockTree` may still remain useful elsewhere, but EWF should use the representation that matches the format rather than the representation that matched the legacy implementation style.

### Decision 3: `EwfFile` disappears as a runtime concept

The current `EwfFile` mixes parsed section headers with an active data stream. In the redesign, that should split into:

- immutable `EwfSegmentDescriptor`
- repository-managed segment source access

After that split, there is no need for a long-lived `EwfFile` object in the runtime read path.

## Immutable Metadata Layout

### `EwfGeometry`

This value object should contain:

- `bytes_per_sector`
- `sectors_per_chunk`
- `chunk_size`
- `number_of_sectors`
- `number_of_chunks`
- `media_size`
- `error_granularity`
- `media_type`

The geometry object is the contract that everything else depends on. If it is invalid, the image must fail to open.

### `EwfSegmentDescriptor`

Each segment descriptor should contain at least:

- `segment_number`
- `file_name`
- `file_size`
- `sections: Arc<[EwfSectionDescriptor]>`
- any segment-local parse warnings or validation notes

It should not contain an open file handle.

### `EwfSectionDescriptor`

Each section descriptor should contain normalized information such as:

- `section_type`
- `file_offset`
- `data_offset`
- `next_offset`
- `section_size`

This is mostly useful for validation, debug output, and future tooling, but it also prevents the runtime from needing to retain parser-only intermediate objects.

### `EwfChunkDescriptor`

Each logical chunk descriptor should contain:

- `chunk_index`
- `logical_offset`
- `logical_size`
- `segment_index`
- `stored_offset`
- `stored_size`
- `storage_kind`
- optional checksum policy or checksum location metadata

`storage_kind` should be a dedicated enum, for example:

```rust
enum EwfChunkStorageKind {
    Stored,
    ZlibCompressed,
}
```

If future EWF variants require more detail, that enum can grow without changing the core read model.

### `EwfHeaderCatalog`

The merged header metadata should contain:

- all parsed header and header2 values
- normalized category/value representation if the project wants to preserve more structure than the current flattened map
- acquisition source metadata if later desired

This object must be immutable and query-only.

### `EwfIntegrityCatalog`

This object should contain:

- md5 hash if present
- sha1 hash if present
- `error2` range data if present
- any parse-time checksum validation results
- table/table2 consistency state

The important point is that integrity metadata is not part of cursor state and not part of the chunk cache.

## Parsing Pipeline

The parser should be explicit and multi-stage.

### Stage 1: root segment discovery

Input:

- a resolver
- a starting file name

Output:

- base image name
- naming schema
- a list of segment locators in expected order

Rules:

- determine naming schema from the provided file name
- derive subsequent segment names deterministically
- stop only when a terminal section sequence proves the set is complete
- fail if a required segment is missing or numbering is inconsistent

### Stage 2: segment inventory pass

For each segment:

1. open the segment source
2. validate the file header
3. read and normalize section headers
4. record file size and section inventory

This stage should not yet materialize final chunk descriptors. It should produce a clean segment inventory that later stages can consume without holding live parse state.

### Stage 3: metadata materialization pass

Using the segment inventory:

1. parse volume and disk/data sections
2. establish geometry exactly once
3. parse header and header2 sections
4. parse digest/hash sections
5. parse error2 sections
6. parse table and table2 sections
7. materialize `EwfChunkDescriptor`s

This pass should be the only place where parser-local state such as `last_sectors_section_header` or `chunk_data_offset_overflow` exists.

### Stage 4: final validation and freeze

After all descriptors are materialized, validate at least the following.

- segment numbering is contiguous and correct
- the image-level set identifier is consistent where required
- exactly one geometry definition exists
- `number_of_chunks` matches the descriptor count
- logical size implied by chunk descriptors matches `media_size`
- table2 mirrors are consistent where present
- last-chunk sizing is valid
- no chunk points outside its segment file

Only after this stage should the driver create `EwfImage` and its runtime services.

## Chunk Descriptor Materialization Rules

### Chunk indexing

Chunk materialization should be based on logical chunk index rather than arbitrary byte ranges.

For each table-derived chunk:

- compute `chunk_index`
- compute `logical_offset = chunk_index * chunk_size`
- compute `logical_size`
  - normally equal to `chunk_size`
  - for the final chunk, equal to the remaining bytes to `media_size`

This guarantees direct runtime lookup without tree traversal.

### Stored offset overflow handling

The current driver handles the EnCase 6.7 overflow behavior while building block ranges. The new driver should keep the same logic, but it must remain strictly parse-time.

That means:

- detect overflow transitions while parsing table entries
- fold the result into the final `stored_offset` and `stored_size` values of each chunk descriptor
- never revisit the overflow logic during reads

### Table2 handling

The new driver should parse `table2` not as a runtime data source but as a validation source.

Recommended behavior:

- when `table2` is present, parse it during open
- compare normalized entry content against the corresponding `table`
- record success or mismatch in `EwfIntegrityCatalog`
- do not keep `table2` in the runtime read path

### `error2` handling

The `error2` section should be parsed into immutable metadata. It should not alter the byte-reading semantics of the image source unless the project deliberately chooses to expose a richer "read result with warnings" API later.

Recommended rule for the initial rewrite:

- preserve raw byte read semantics
- expose acquisition error metadata through query APIs
- keep runtime `read_at()` focused on bytes only

## Runtime Read Path

The runtime EWF source should implement a pure positioned read algorithm and expose itself as a `DataSource`.

### Step-by-step read flow

Given `(logical_offset, dst)`:

1. clamp to `media_size`
2. compute `chunk_index` and chunk-relative offset
3. resolve the immutable `EwfChunkDescriptor`
4. dispatch by `storage_kind`
5. copy the relevant slice into `dst`
6. continue to the next chunk until `dst` is full or media ends

### Stored chunk path

For stored chunks:

- obtain the segment source from `EwfSegmentRepository`
- issue `read_exact_at(stored_offset + relative_offset, dst_slice)`
- do not allocate a chunk-sized intermediate buffer for normal partial reads

Optional integrity behavior:

- a full-chunk validation mode may read and verify the trailing checksum
- normal partial reads should not be forced to materialize the whole chunk only to verify the trailer

### Compressed chunk path

For compressed chunks:

1. look up the logical chunk in `EwfChunkCache`
2. if present, copy from the cached decompressed bytes
3. if absent, run a single-flight decode:
   - read the stored compressed bytes from the appropriate segment source
   - decompress into a new immutable chunk buffer sized to `logical_size`
   - store the result in the cache
   - copy the requested slice

The cache key should be the logical chunk index. That is stable, format-native, and unambiguous.

### Reader-local scratch storage

Each cursor or decode context may keep:

- one temporary buffer for compressed chunk bytes
- one decompression scratch arena if the codec benefits from it
- one optional sequential prefetch hint

This storage must remain reader local and must not be required for correctness.

## `EwfSegmentRepository`

The repository is the abstraction that replaces the old segment file cache.

### Responsibilities

- map segment indices to immutable segment sources
- lazily open segment sources if configured to do so
- optionally enforce a file descriptor budget if the project decides that very large segment sets must not keep all files open

### Recommended implementation policy

Provide two operating modes behind the same interface.

#### Eager-open mode

Best for:

- typical segment counts
- lowest read latency
- simplest implementation

Behavior:

- open all segment sources during `EwfImage::open()`
- store them in an immutable `Arc<[Arc<dyn DataSource>]>`

#### Lazy-open mode

Best for:

- extreme segment counts
- environments with strict file descriptor budgets

Behavior:

- store only segment locators at open time
- open on first use through a thread-safe `LazyOpenSource`
- optionally add a bounded handle pool if the project later proves it necessary

The default recommendation is eager-open mode until real workloads justify the added complexity of pooled lazy handles.

## Shared Cache Layout

The EWF runtime should explicitly separate cache scopes.

| Cache | Scope | Key | Value | Notes |
| --- | --- | --- | --- | --- |
| Segment source table | shared | segment index | `Arc<dyn DataSource>` | immutable after open in eager mode |
| Lazy-open state | shared | segment index | open handle or opener state | only needed in lazy mode |
| Decompressed chunk cache | shared | logical chunk index | immutable chunk bytes | sharded, byte-bounded, single-flight |
| Integrity verification state | shared optional | logical chunk index | verified flag or result | only if on-demand chunk verification is enabled |
| Decode scratch | reader local | none | temporary buffers | reused per cursor |

## Public API for EWF

Recommended public shape:

```rust
impl EwfImage {
    pub fn open(
        resolver: Arc<dyn SourceResolver>,
        file_name: &PathComponent,
    ) -> Result<Self, ErrorTrace>;

    pub fn geometry(&self) -> &EwfGeometry;

    pub fn header_values(&self) -> &EwfHeaderCatalog;

    pub fn integrity(&self) -> &EwfIntegrityCatalog;

    pub fn open_source(&self) -> Arc<dyn DataSource>;

    pub fn open_cursor(&self) -> EwfCursor;
}
```

Recommended cursor shape:

```rust
pub struct EwfCursor {
    source: Arc<dyn DataSource>,
    position: u64,
    scratch: EwfReaderScratch,
}
```

### Capability reporting

The EWF media data source should report explicit runtime characteristics.

- In the bridge-backed migration state:
  - `read_concurrency = Serialized`
  - `seek_cost = Expensive`
  - `preferred_chunk_size = Some(chunk_size)`
- In the final native implementation:
  - `read_concurrency = Concurrent`
  - `seek_cost = Expensive`
  - `preferred_chunk_size = Some(chunk_size)`

This follows the `regressor-storage` capability model and allows higher-level consumers to adapt scheduling and chunking strategy correctly.

### Resolver policy

Even though `regressor-storage` uses `origin_path()` as a useful host-file hint, Keramics should continue to treat EWF sibling resolution as an explicit resolver responsibility.

That means:

- the EWF core parser should continue to depend on an explicit resolver interface for correctness
- `origin_path()` may help convenience integrations, but it must not replace the resolver contract for multi-segment formats

Notably absent from the public API:

- `impl DataStream for EwfImage`
- `current_offset` on the shared image object
- `segment_file_cache` on the shared image object
- `chunk_cache` hidden inside the cursor object

## Suggested Module Layout

The EWF module should be reorganized around parser, model, and runtime concerns.

```text
ewf/
  mod.rs
  parser/
    image_parser.rs
    segment_parser.rs
    section_parser.rs
    table_parser.rs
    header_parser.rs
  model/
    image.rs
    geometry.rs
    segment.rs
    section.rs
    chunk.rs
    integrity.rs
  runtime/
    source.rs
    cursor.rs
    repository.rs
    cache.rs
    decoder.rs
```

The existing low-level section parsing files can remain, but they should become parser dependencies rather than long-lived runtime components.

## Acceptance Criteria for the EWF Rewrite

The EWF rewrite is complete only when all of the following are true.

1. `EwfImage` is immutable after successful open.
2. `EwfImage` does not own a shared cursor.
3. The EWF runtime uses true positioned reads from segment sources.
4. Chunk mapping is fully materialized before the first byte read.
5. Parallel readers can read different chunks concurrently without contending on a single image-wide lock.
6. The image can expose many independent cursors over the same opened metadata object.
7. The driver preserves existing logical correctness while adding explicit concurrency tests, capability-reporting tests, and performance benchmarks.
