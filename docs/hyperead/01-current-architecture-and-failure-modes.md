# Current Architecture and Failure Modes

## Scope

This document explains why the current `keramics-formats` read model cannot be made truly concurrent by small fixes. The problem is structural. Cursor state, random-access I/O, caches, and format metadata are fused into the same long-lived mutable objects. As a result, the system behaves like a graph of serialized state machines rather than a graph of immutable read views.

The most visible example is the EWF driver, but the root cause is lower: it starts in `keramics-core`, then propagates through images, partitions, block streams, compressed streams, and file-system file handles.

## The Root Cause Lives in `keramics-core`

The current API makes all reads cursor-mutating operations.

### `DataStreamReference` is a shared mutable stream

In `keramics-core/src/data_stream.rs`, the central reference type is:

```rust
pub type DataStreamReference = Arc<RwLock<dyn DataStream>>;
```

The trait itself is cursor-oriented:

```rust
pub trait DataStream: Send + Sync {
    fn get_offset(&mut self) -> Result<u64, ErrorTrace>;
    fn get_size(&mut self) -> Result<u64, ErrorTrace>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorTrace>;
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, ErrorTrace>;
}
```

Even operations that are logically read-only, such as retrieving the current position or stream size, require `&mut self`. That already tells us the type system does not distinguish immutable, shareable read access from mutable cursor movement.

### Positioned read is implemented as `seek + read`

The default `read_at_position` and `read_exact_at_position` helpers in `keramics-core/src/data_stream.rs` are not true random-access operations. They are cursor mutation helpers:

```rust
fn read_at_position(&mut self, buf: &mut [u8], pos: SeekFrom) -> Result<usize, ErrorTrace> {
    self.seek(pos)?;
    self.read(buf)
}
```

This means that a positioned read is not a stateless read against a logical source. It is a state transition on a shared cursor.

### The helper macros always take a write lock

In `keramics-core/src/macros.rs`, all commonly used helpers acquire `data_stream.write()`.

- `data_stream_get_size!`
- `data_stream_read_at_position!`
- `data_stream_read_exact_at_position!`

The effect is straightforward:

- every read is exclusive
- every positioned read is exclusive
- every nested read through a parent object is exclusive

The `RwLock` is therefore not providing read concurrency. It is effectively a mutex with more overhead.

### The OS backend is still cursor based

In `keramics-core/src/os_data_stream.rs`, `std::fs::File` implements `DataStream` through ordinary `Read` and `Seek`. `open_os_data_stream()` returns one `File` wrapped in one `Arc<RwLock<_>>`.

This has an important consequence: cloning a `DataStreamReference` does not create an independent file reader. It creates another pointer to the same locked file handle and the same mutable cursor semantics.

### The fake backend follows the same model

`keramics-core/src/fake_data_stream.rs` stores `current_offset` in the fake stream and mutates it during `seek()` and `read()`. This is useful for testing the old model, but it also means the old model is embedded into test assumptions and helper types.

## The Pattern Repeats Across `keramics-formats`

The same design shape appears throughout the crate.

### Repeated object shape

Many reader objects look like this:

- an `Option<DataStreamReference>` field
- a `current_offset: u64` field
- an `impl DataStream` block with `read()` and `seek()`

Representative examples include:

- `keramics-formats/src/ewf/image.rs`
- `keramics-formats/src/ext/block_stream.rs`
- `keramics-formats/src/fat/block_stream.rs`
- `keramics-formats/src/xfs/block_stream.rs`
- `keramics-formats/src/ntfs/block_stream.rs`
- `keramics-formats/src/mbr/partition.rs`
- `keramics-formats/src/gpt/partition.rs`
- `keramics-formats/src/apm/partition.rs`
- `keramics-formats/src/splitraw/image.rs`
- `keramics-formats/src/sparsebundle/image.rs`
- `keramics-formats/src/vhd/file.rs`
- `keramics-formats/src/vhdx/file.rs`
- `keramics-formats/src/qcow/file.rs`

This means the architecture is not merely using one mutable source at the bottom. It is creating new layers of mutable cursor-bearing objects on top of that source.

### File-system APIs construct more stateful streams

When file-system code exposes file content, it usually builds another mutable `DataStream` implementation and returns it wrapped in `Arc<RwLock<_>>`.

Examples:

- `keramics-formats/src/ext/file_entry.rs`
- `keramics-formats/src/xfs/file_entry.rs`
- `keramics-formats/src/hfs/file_entry.rs`
- `keramics-formats/src/ntfs/mft_attributes.rs`

That means `get_data_stream()` does not mean "give me an immutable content source that many callers can read concurrently." It means "construct another stateful reader object and put it behind another lock."

### Partition wrappers are also state machines

`MbrPartition`, `GptPartition`, and `ApmPartition` are conceptually simple subranges over a parent image. However they are still implemented as mutable cursors with shared parent I/O. This is important because it shows that the state-machine design is not only used where it is unavoidable. It is used even where a pure immutable view would be the most natural model.

## EWF Is the Clearest Example of the Problem

The EWF driver mixes almost every kind of shared mutable state in one object.

### `EwfImage` combines metadata, caches, runtime I/O, and the cursor

`keramics-formats/src/ewf/image.rs` stores all of the following inside one mutable runtime object:

- image metadata such as geometry, header values, hashes, media size, and media type
- the logical-to-physical mapping in `block_tree`
- a `segment_file_cache`
- a decompressed `chunk_cache`
- the sequential read cursor in `current_offset`

This is the opposite of a concurrency-friendly design. Shared immutable metadata and shared mutable caches are already mixed together, and both are then mixed again with a cursor.

### EWF segment access is serialized through mutable cached objects

`EwfImage::read_data_from_blocks()` uses `segment_file_cache.get_mut()` to obtain `&mut EwfFile`. Each `EwfFile` contains a `DataStreamReference`, which again resolves to a write-locked seekable stream.

The result is a full serialization chain:

1. caller must gain mutable access to `EwfImage`
2. `EwfImage` mutates or consults the segment cache
3. `EwfImage` mutates or consults the chunk cache
4. `EwfFile` performs a write-locked positioned read on the segment file

Two concurrent readers of different offsets in the same EWF image cannot proceed independently. They contend immediately on the top-level image object and then again at the segment level.

### The EWF chunk cache is only safe because the whole image is serialized

The current chunk cache is `LruCache<u64, Vec<u8>>`, implemented in `keramics-formats/src/lru_cache.rs`.

This cache has several issues in the context of a future concurrent reader:

- it is not synchronized
- it is entry-count based rather than byte-size based
- `get()` does not update usage, so it is not a true LRU policy
- it assumes single-threaded mutation of the cache owner

Today the cache is not racy only because the entire `EwfImage` is already forced behind mutable access. If the outer serialization were relaxed without redesigning the cache boundary, the cache would immediately become unsafe.

### Parsing and runtime reading are not clearly separated

The same `EwfImage` object:

- discovers segment files
- parses section headers
- parses volume, digest, hash, header, header2, table, and error sections
- builds the logical chunk map
- later acts as the runtime sequential reader

This is a strong signal that the abstraction is upside down. The object that should become a frozen, immutable metadata description of the image instead remains the live cursor-bearing reader.

### EWF-specific parse state leaks into runtime structure

The current EWF parse path uses transient section-order state such as:

- `last_sectors_section_header`
- `block_media_offset`
- `chunk_data_offset_overflow`

These are perfectly valid parse-time concerns, especially for handling table layouts and the EnCase 6.7 offset overflow behavior, but they should exist only inside a builder or parser. They should not define the shape of the long-lived runtime reader.

### `EwfFile` mixes immutable and mutable responsibilities

`EwfFile` stores:

- `segment_number`
- parsed `sections`
- a `data_stream`

That means it is simultaneously:

- a segment metadata object
- an active I/O handle

Those are different lifetimes and different concurrency domains. Segment metadata should be immutable and freely shareable. Segment I/O handles should be independent read-at sources or lazy-open providers.

## Additional Non-EWF Evidence That the Model Is Wrong

EWF is not the only driver that mutates metadata during reads.

### QCOW and VHDX mutate mapping state while reading

`QcowFile` and `VhdxFile` fill their block trees on demand during `read_data_from_blocks()`. That means runtime reads are not only performing I/O. They are also discovering and mutating mapping metadata.

This is another direct blocker for true concurrency. Even if the underlying I/O became read-at based, the object would still be mutating shared block mapping state during reads.

### Parent/backing relationships are also stateful and lock based

`VhdxFile` stores `parent_file: Option<Arc<RwLock<VhdxFile>>>`.

`QcowFile` stores `backing_file: Option<Arc<RwLock<QcowFile>>>`.

This means layered image reads recurse into other mutable, locked reader objects rather than into immutable view graphs.

The architecture therefore serializes not only local reading, but also cross-layer reading.

## Why Small Fixes Are Not Enough

The following changes would not solve the problem:

- replacing `RwLock` with `Mutex`
- replacing `Mutex` with `RwLock`
- changing some `get()` methods to `&self`
- cloning more `Arc`s
- adding more caches
- making `current_offset` atomic
- only rewriting EWF without changing the core read abstraction

These are insufficient because the core contract is wrong. The architecture treats a read source as a shared mutable cursor. As long as that contract remains in place, every driver will either stay serialized or re-invent a local escape hatch around a globally serialized core.

## The Design Mandate

The rewrite must enforce four hard rules.

1. Shared domain objects must be immutable after parse/open completes.
2. Positioned random access must be the primary I/O primitive.
3. Cursor state must live only in disposable local reader objects.
4. Runtime reads must never rely on mutating shared mapping metadata.

The rest of the proposal is built around those four rules.
