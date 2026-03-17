# Target Architecture

## Goal

The target architecture replaces shared mutable stream objects with immutable read sources, independent cursors, immutable mapping metadata, and concurrent caches. The objective is not just to remove one lock. The objective is to invert the model so that concurrency is the default property of the design.

The central shift is this:

- today, a format object is usually a mutable stream
- in the new architecture, a format object is immutable metadata plus an immutable logical byte source

Sequential reading is still supported, but it becomes a thin adapter on top of immutable random access rather than the foundation of the whole system.

## Non-Negotiable Design Rules

1. A shared object must not own a shared cursor.
2. Positioned reads must not be implemented as `seek + read` on shared state.
3. All format metadata exposed after `open()` or `parse()` must be immutable.
4. Any cache that survives beyond one method call must be explicit, concurrent, and separate from cursor state.
5. Runtime reads may populate caches, but they may not mutate geometry, extent maps, or block mappings.
6. File systems, partitions, and image drivers should build logical sources out of reusable runtime building blocks instead of each implementing their own bespoke mutable `DataStream`.

## The New Core Contract

The current `DataStream` contract should be removed as the foundation of the system.

The concrete implementation target should align with the `regressor-storage` `DataSource` shape rather than introducing a totally separate root abstraction. Earlier drafts used `ReadAtSource` as a conceptual name. The actual Keramics direction should be a `DataSource`-style trait with explicit capability metadata.

### Replacement: immutable random-access `DataSource`

The new root abstraction should look conceptually like this:

```rust
pub trait DataSource: Send + Sync {
    fn read_at(&self, offset: u64, dst: &mut [u8]) -> Result<usize, ErrorTrace>;

    fn size(&self) -> Result<u64, ErrorTrace>;

    fn capabilities(&self) -> DataSourceCapabilities {
        DataSourceCapabilities::default()
    }

    fn telemetry_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

pub type DataSourceHandle = Arc<dyn DataSource>;
```

Important properties of this contract:

- all methods take `&self`
- the source has no caller-visible cursor state
- the source can be safely shared across threads
- `read_at()` is a real positioned read contract, not a seek helper
- capability reporting makes concurrency and seek behavior explicit instead of implicit

### Sequential reading becomes a local adapter

Cursor state still exists, but only in a small disposable object:

```rust
pub struct Cursor {
    source: DataSourceHandle,
    position: u64,
}

impl Cursor {
    pub fn read(&mut self, dst: &mut [u8]) -> Result<usize, ErrorTrace> {
        let read_count = self.source.read_at(self.position, dst)?;
        self.position += read_count as u64;
        Ok(read_count)
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, ErrorTrace> {
        // Update only local state.
    }

    pub fn fork_at(&self, position: u64) -> Self {
        Self {
            source: self.source.clone(),
            position,
        }
    }
}
```

This is the core conceptual change.

- a source is shared and immutable
- a cursor is local and mutable

Two threads can hold two cursors to the same source and make independent progress without affecting one another.

Keramics should adopt this low-level `DataSource` contract, but it should not copy `StorageSession` or `MountedFilesystem` into `keramics-core`. Those remain higher-level orchestration concerns.

Under the current migration plan, the first full implementation of this architecture should live in `keramics-drivers`, where it can expose a clean new API without `keramics-formats` compatibility constraints.

## Source Families

The new model should not produce one custom reader implementation per format. It should provide a small runtime library of reusable source families.

### 1. `OsDataSource`

Purpose:

- the direct replacement for `open_os_data_stream()`
- backed by a real positioned-read implementation

Requirements:

- use platform-specific positioned reads such as `FileExt::read_at` or `seek_read`
- never rely on a shared cursor for normal reads
- expose only immutable `DataSource`

If a platform does not provide true positioned reads for the chosen backend, the fallback must still preserve concurrency semantics, for example by using per-handle cloning or an internal handle pool. The fallback must not reintroduce one global cursor.

### 2. `MemorySource`

Purpose:

- replace `FakeDataStream`
- hold resident or inline file data
- hold decoded data that should be exposed as a source

Properties:

- immutable byte storage
- no locks required
- trivially concurrent

### 3. `SubrangeSource`

Purpose:

- represent partitions, volumes, slices, and embedded regions

Use cases:

- MBR partitions
- GPT partitions
- APM partitions
- any format that exposes a contiguous region of another source

This source is so simple that it should become the standard way to represent partitions rather than implementing a separate mutable `DataStream` for each partition type.

### 4. `SegmentedSource`

Purpose:

- represent logical byte streams that span multiple physical files or bands
- route reads to the correct underlying source without giving the caller direct access to segment-local cursor state

Use cases:

- split raw images
- sparsebundle band files
- multi-extent image layouts that are not fundamentally parent/child overlays

This source family is simpler than a full layered block source. It should exist separately so that straight multi-part layouts do not inherit unnecessary layering complexity.

### 5. `ExtentMapSource`

Purpose:

- represent logical byte streams backed by a set of immutable extents and sparse regions

Use cases:

- ext file data
- FAT cluster chains
- HFS fork extents
- XFS extents
- NTFS block runs when no decompression is required

This should replace most format-specific block stream implementations.

### 6. `CompressionUnitSource`

Purpose:

- represent logical streams whose storage is organized in independently compressed units rather than in plain extents

Use cases:

- NTFS compressed data streams
- NTFS WoF compressed streams
- any future file-system level compressed extent formats

This source family prevents file-system compression from being modeled as one-off mutable reader objects.

### 7. `ChunkedCompressedSource`

Purpose:

- represent fixed-size logical chunks that may be stored compressed or uncompressed

Use cases:

- EWF
- other chunked compressed formats if they appear later

This is the main reusable family for the EWF rewrite.

### 8. `LayeredBlockSource`

Purpose:

- represent block-based logical sources that may delegate missing data to a parent or backing source

Use cases:

- QCOW
- VHD/VHDX differencing images
- VMDK layered images
- PDI layered layouts
- UDIF band/block indirection

The key point is that parent delegation must happen through immutable child and parent sources, not through `Arc<RwLock<MutableReader>>` recursion.

## Legacy-to-Target Mapping Matrix

The rewrite should follow a simple rule: every current state-machine reader must be assigned to one target source family or to a pure metadata role. The mapping below is the intended destination.

| Current area | Representative modules | Target runtime primitive | Notes |
| --- | --- | --- | --- |
| OS file access | `keramics-core/src/os_data_stream.rs` | `OsDataSource` | real positioned reads only |
| inline or resident bytes | `FakeDataStream` call sites in ext, HFS, NTFS, XFS | `MemorySource` | immutable byte storage |
| partitions and slices | `mbr/partition.rs`, `gpt/partition.rs`, `apm/partition.rs` | `SubrangeSource` | partition records become metadata only |
| plain multi-part images | `splitraw/image.rs`, sparsebundle band routing | `SegmentedSource` | logical range to physical file/band routing |
| extent-backed file data | `ext/block_stream.rs`, `fat/block_stream.rs`, `hfs/block_stream.rs`, `xfs/block_stream.rs`, `ntfs/block_stream.rs` | `ExtentMapSource` | sparse regions become explicit extents |
| file-system compression units | `ntfs/compressed_stream.rs`, `ntfs/wof_compressed_stream.rs` | `CompressionUnitSource` | shared per-unit decode caches |
| EWF image data | `ewf/image.rs`, `ewf/file.rs` | `ChunkedCompressedSource` plus `EwfSegmentRepository` | dense immutable chunk index |
| layered block images | `qcow/file.rs`, `vhd/file.rs`, `vhdx/file.rs`, `vmdk/*`, `pdi/*`, `udif/file.rs` | `LayeredBlockSource` | immutable parent/backing delegation |

Any module that does not map cleanly to one of these families should be treated as a signal that the runtime library is still missing an essential primitive.

## Immutable Metadata Objects

Each format should be split into two planes.

### Metadata plane

The metadata plane is built once and then frozen. It contains:

- validated headers
- geometry
- extents or chunk descriptors
- directory and inode metadata
- hashes, digests, and integrity metadata
- any data required to resolve logical offsets to physical storage

The metadata plane must be immutable after construction.

### Runtime plane

The runtime plane contains:

- immutable sources built from metadata
- shared concurrent caches
- services such as lazy-open repositories or decoder registries

The runtime plane may contain internal synchronization, but it may not own caller-visible cursor state.

## Mapping Metadata Must Stop Mutating During Reads

The old design often populates `BlockTree` or similar structures inside the `read()` path. That pattern must stop.

There are only two acceptable models:

1. Fully materialize the mapping during parse/open.
2. Use concurrency-safe memoization for metadata pages, where each lazily computed page is immutable after initialization.

In either model, the read path may consult immutable data or trigger one-time page initialization, but it must not freely mutate a shared mapping structure through `&mut self`.

### Recommended default

When the format allows it, prefer full materialization at open time.

Reasons:

- simpler invariants
- easier testing
- easier debugging
- easier concurrency analysis
- fewer runtime surprises

EWF should use full materialization. Its chunk map is fully derivable at open time and should not be discovered lazily during reads.

## Cache Design

Caches are required for performance, but they must be treated as separate infrastructure, not as a side effect of cursor-bearing objects.

### Cache boundaries

There are three distinct cache scopes.

#### Shared image-level caches

Examples:

- EWF decompressed chunk cache
- metadata page cache for large layered formats
- segment handle cache or lazy-open repository

Requirements:

- concurrent
- bounded by bytes where practical
- independent from cursor lifetime
- correct even if empty

#### Reader-local scratch state

Examples:

- temporary compressed chunk buffer
- decompression scratch storage
- sequential prefetch window

Requirements:

- no sharing required
- optimized for allocation reduction
- never part of observable object identity

#### Parser-local state

Examples:

- last section pointer while walking EWF sections
- temporary table objects
- geometry accumulation variables

Requirements:

- dropped after parse completes
- never stored in the runtime reader object

### Single-flight cache fill

For expensive objects such as decompressed EWF chunks, the cache should support single-flight behavior. If eight threads request the same missing chunk, only one thread should perform the decode. The others should wait for the same result rather than decompressing eight times.

The exact implementation is flexible. The architectural requirement is not.

### Eviction policy

The old cache code is entry-count based. That is too weak for decoded chunks because chunk sizes differ by format and some caches are storing large buffers.

The new cache policy should be byte-budget based. Entry count may still be used as a secondary heuristic, but not as the main capacity control.

## Resolver and Source Opening

The current `FileResolver` contract is conceptually sound in one respect: the resolver itself is already immutable and shareable. That should be preserved.

The new resolver contract should return immutable sources or source openers rather than mutable stream handles.

Conceptually:

```rust
pub trait SourceResolver: Send + Sync {
    fn open_source(
        &self,
        path_components: &[PathComponent],
    ) -> Result<Option<DataSourceHandle>, ErrorTrace>;
}
```

If a format needs lazy opening, it should wrap this resolver result inside an immutable `LazyOpenSource` implementation rather than exposing mutable `RwLock`-guarded handles.

## Public API Shape

The public API should change in a deliberate and breaking way.

### Old pattern

- `read_data_stream(&DataStreamReference)`
- `get_data_stream() -> DataStreamReference`
- types themselves implement `DataStream`

### New pattern

- `open(source: DataSourceHandle) -> Result<Self, ErrorTrace>` or `parse(source: DataSourceHandle)`
- `open_source() -> Result<Option<DataSourceHandle>, ErrorTrace>`
- `open_cursor() -> Cursor`
- domain types do not implement the root I/O trait unless they truly are sources

Recommended rule:

- domain metadata objects expose immutable sources
- only source objects implement `DataSource`
- only cursor objects implement sequential semantics

### Example transformation

Old partition API shape:

```rust
let mut partition = GptPartition::new(...);
partition.open(&data_stream)?;
partition.seek(...)?;
partition.read(...)?;
```

New shape:

```rust
let partition = GptPartition::parse(...)?;
let source = partition.open_source(parent_source.clone())?;
let mut cursor = Cursor::new(source);
cursor.seek(...)?;
cursor.read(...)?;
```

The partition record itself is now metadata. The source is an immutable view. The cursor is local.

## Porting Rules for Drivers

Every driver rewrite should be checked against the same rules.

### Forbidden in shared runtime objects

- `current_offset`
- `seek()` that mutates shared object state
- `Option<DataStreamReference>`
- `impl DataStream` for metadata objects
- runtime insertion into shared mapping trees without explicit concurrency-safe memoization

### Expected in shared runtime objects

- immutable descriptors
- `Arc`-shared metadata
- concurrent caches with explicit policies
- immutable child/parent source references

### Allowed mutable state

- parser-local temporary variables
- cursor-local position
- cursor-local prefetch hints and scratch buffers
- internal cache synchronization that does not leak into the API contract

## Compatibility Stance

This redesign should be treated as a hard API break.

The project should not try to preserve the old `DataStream` worldview as the primary compatibility layer. A thin adapter may exist temporarily for test migration or a narrow interoperability need, but the new architecture must not be designed around that adapter.

The design center is immutable sources and independent cursors. Everything else is secondary.
