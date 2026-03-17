# Validation and Benchmarks

## Purpose

This document defines how the rewrite proves that it is correct and that it actually achieves real concurrent reading rather than merely changing type names.

The validation strategy must test three things separately.

1. Byte correctness.
2. Concurrency correctness.
3. Throughput and contention behavior.

All three are required. Passing the old functional tests alone is not enough, because the point of the rewrite is architectural behavior, not only logical output.

## Acceptance Criteria

The new architecture is acceptable only if it can demonstrate all of the following.

1. Two cursors over the same opened source can read independently without affecting one another.
2. Two direct `read_at()` calls against the same shared source can run concurrently without contending on a single global cursor lock.
3. EWF chunk decoding is correct under both cold-cache and hot-cache conditions.
4. Shared caches remain correct under high parallel access.
5. Throughput improves as concurrent readers increase, at least until storage or CPU becomes the real bottleneck.
6. Capability reporting is accurate enough that higher-level consumers can distinguish serialized bridge-backed sources from native concurrent sources.

## Test Layers

## Layer 1: Core I/O Tests

These tests validate the new `DataSource` foundation.

### Required tests

- `OsDataSource` reads exact bytes at arbitrary offsets.
- `MemorySource` reads exact bytes at arbitrary offsets.
- `SubrangeSource` clamps and offsets correctly.
- two cursors over one shared source maintain independent positions.
- many threads reading the same shared source at different offsets always receive correct results.
- capability reporting correctly reflects native positioned-read backends versus fallback serialized backends.

### Required concurrency tests

- N threads, same file, random offsets, fixed seed, compare against a reference file reader.
- N threads, same file, same offset, verify identical results under repeated load.
- N threads, overlapping reads, verify no cursor interference.

These tests should exist in `keramics-core` because they validate the root abstraction rather than a specific format.

## Layer 2: EWF Parser Tests

The EWF parser must be validated independently from the runtime reader.

### Required parser tests

- segment naming schema derivation
- segment enumeration and numbering validation
- section header normalization
- volume or disk section geometry extraction
- header and header2 parsing
- digest and hash parsing
- table parsing
- table2 mirror validation
- overflow handling for large-offset table entries
- final chunk sizing logic
- `error2` parsing and storage

### Required metadata invariants

- descriptor count equals declared chunk count when the format provides one
- no chunk points beyond its segment file
- logical chunk coverage is complete and ordered
- logical media size is exactly represented by the descriptor set

## Layer 3: EWF Read Correctness Tests

These tests validate the runtime `EwfSource` and `EwfCursor`.

### Required byte-correctness tests

- full sequential read of the image, compare md5 with the known expected value
- full sequential read through a cursor, compare to direct `read_at()` stitched result
- random 4 KiB reads across the entire image, compare against a known raw reference image when available
- reads that start and end inside the same chunk
- reads that cross chunk boundaries
- reads that cross segment boundaries
- reads that touch the final short chunk

### Required cache-behavior tests

- cold-cache compressed chunk decode returns correct bytes
- hot-cache repeated read returns the same bytes
- concurrent same-chunk requests return the same bytes and do not corrupt cache state
- concurrent different-chunk requests return correct bytes

### Required capability tests

- bridge-backed EWF or legacy-stream-backed sources report `Serialized`
- native rewritten EWF sources report `Concurrent`
- preferred chunk size matches the actual chunk geometry exposed by the image

### Required integrity tests

- parser rejects malformed table checksum data
- parser records table2 mismatches when present
- optional chunk-integrity verification behaves deterministically if the feature is enabled

## Layer 4: Cross-Format Regression Tests

The architecture rewrite is not complete if only EWF works.

### Partition tests

- MBR partition as immutable subrange source
- GPT partition as immutable subrange source
- APM partition as immutable subrange source

### File-system tests

- ext file content through `ExtentMapSource`
- FAT file content through `ExtentMapSource`
- HFS fork content through immutable source views
- NTFS resident and non-resident data content
- XFS regular file content

### Layered image tests

- QCOW backing file delegation
- VHDX parent delegation
- VHD differencing chain reads
- VMDK layered extent reads

Each of these should include at least one concurrency test that verifies two readers can read different logical regions at the same time.

## Stress Tests

Stress tests should deliberately target the new concurrency boundaries.

### EWF stress scenarios

1. Many threads, same chunk, repeated reads.
2. Many threads, adjacent chunks, repeated reads.
3. Many threads, random chunks, repeated reads.
4. Many threads, different segments, repeated reads.
5. Many cursors performing sequential scans at different starting offsets.

### Layered image stress scenarios

1. many threads reading child-only blocks
2. many threads reading parent-only blocks
3. many threads reading alternating child and parent blocks

### File-system stress scenarios

1. many threads reading the same regular file
2. many threads reading different files in the same file system
3. mixed metadata and file-content queries from the same mounted file system object

## Benchmark Plan

The benchmark suite must explicitly measure concurrency scaling.

## Benchmark 1: direct source random reads

Measure:

- 4 KiB random reads
- 64 KiB random reads
- 1, 2, 4, 8, 16 readers

Targets:

- same file
- same opened `Arc<dyn DataSource>`

Goal:

- prove the new core source abstraction is not serialized by a single shared cursor lock

## Benchmark 2: EWF random chunk reads

Measure:

- cold-cache random reads
- warm-cache random reads
- same-chunk hotspot
- uniform random chunk distribution
- 1, 2, 4, 8, 16 readers

Goal:

- prove chunk cache concurrency correctness
- prove cache misses are the expensive path, not lock contention on one global image object

## Benchmark 3: EWF sequential multi-reader scan

Measure:

- multiple cursors scanning the same image from different offsets
- aggregate throughput
- CPU time spent in decompression versus waiting

Goal:

- show that many cursors can coexist on one opened image object without interfering with one another

## Benchmark 4: layered image parent delegation

Measure:

- reads that hit only child blocks
- reads that hit only parent blocks
- mixed reads

Goal:

- prove parent or backing delegation no longer serializes on nested mutable reader locks

## Instrumentation

The benchmarks should collect at least:

- elapsed time
- bytes read
- aggregate throughput
- cache hit and miss counts where applicable
- chunk decode counts for EWF
- optional lock wait metrics if the selected cache implementation exposes them

Recommended secondary profiling:

- allocation profiling for compressed chunk paths
- flamegraphs for EWF hot loops
- lock contention profiling if shared caches become bottlenecks

## Regression Gates

Before the rewrite is merged, it should meet the following gates.

1. All old byte-correctness tests that remain relevant still pass.
2. New concurrency tests pass reliably under repeated execution.
3. No benchmark demonstrates obvious serialization on a single shared cursor lock.
4. EWF random-read throughput with multiple readers shows positive scaling before storage or CPU saturation.
5. Cache corruption, duplicate decode races, and cursor interference are absent under stress.
6. Capability metadata matches observed backend behavior closely enough to drive scheduling decisions safely.

## Final Sign-Off Checklist

- core source tests complete
- EWF parser tests complete
- EWF read correctness tests complete
- partition and file-system regression tests complete
- layered image regression tests complete
- concurrency stress suite complete
- benchmark suite complete
- profiling review complete

Only after this checklist is complete should the rewrite be considered architecturally finished.
