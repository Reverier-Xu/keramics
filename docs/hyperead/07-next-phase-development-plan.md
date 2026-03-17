# Next-Phase Development Plan

## Context

This document turns the architecture and migration direction in
`docs/hyperead/06-keramics-drivers-crate-migration-plan.md` into an immediate
execution plan based on the current state of `keramics-drivers` relative to
`keramics-formats`.

The main conclusion is that the migration has already moved beyond the early
bootstrap phases. The next phase should not be another wide porting pass.
Instead, it should finish the reusable runtime infrastructure that later ports
were supposed to share, then use EWF as the proof that the architecture can
handle the hardest concurrent-read path cleanly.

## Migration Checkpoint

| Planned phase | Current state | Notes |
| --- | --- | --- |
| Phase 0: bootstrap the crate | Mostly complete | `keramics-drivers` is a real workspace crate with its own API boundary and no dependency on `keramics-formats`. |
| Phase 1: build source primitives | Largely complete | `DataSource`, capabilities, cursor, local file source, memory, slice, observed, and probe-cache wrappers already exist. |
| Phase 2: build reusable composite sources | Partial | `SegmentedDataSource` and `ExtentMapDataSource` exist, but `CompressionUnitDataSource`, `ChunkedCompressedDataSource`, and `LayeredDataSource` are still missing. |
| Phase 3: port structural volume formats | Complete enough | MBR, GPT, and APM are already native in `keramics-drivers`. |
| Phase 4: port simple multi-part images | Complete enough | Split raw, sparsebundle, and sparseimage are already present. |
| Phase 5: port EWF as the flagship driver | Partial | Native EWF exists, but the implementation has not yet been split into the parser/model/runtime shape described by the redesign docs. |
| Phase 6: port extent-backed file systems | Mostly complete | ext, FAT, HFS, and XFS are present, but HFS and XFS still have important feature gaps. |
| Phase 7: port NTFS | Partial | Basic NTFS works, but the compression-heavy and reparse-heavy paths that stress the new architecture are still behind. |
| Phase 8: port layered image formats | Underway | QCOW, VHD, VHDX, VMDK, PDI, and UDIF exist, but they do not yet share the planned generic layered runtime primitives. |

## Current Drivers vs Formats Snapshot

### Areas where `keramics-drivers` is already close to replacement status

- Volume systems: MBR, GPT, and APM.
- Simple multi-file and extent-routed images: split raw, sparsebundle, sparseimage.
- Strong early file-system ports: ext and FAT.
- Relatively mature image ports: VHD, VHDX, UDIF.

These areas suggest that the crate boundary, immutable source model, and basic
parser-to-runtime split are working well enough to support more advanced work.

### Areas where `keramics-formats` still defines the harder parity targets

- EWF still needs the full flagship redesign treatment.
- NTFS still needs compression-unit, WoF, and broader attribute-path work.
- HFS still needs overflow-extent handling.
- XFS still needs the more complex btree-backed data paths.
- QCOW and VMDK still need deeper layered-image and backing-chain coverage.

## Architectural Gap Summary

The main gap is no longer basic format coverage. The main gap is reusable
runtime infrastructure.

The migration plan explicitly called for the following shared building blocks:

- `CompressionUnitDataSource`
- `ChunkedCompressedDataSource`
- `LayeredDataSource`
- a dedicated `runtime/` module for shared cache and decode helpers

Today, the crate has already proven the simpler source wrappers, but the more
advanced decode and layering behavior is still being implemented inside
individual drivers. That is acceptable for initial ports, but it should not
become the steady-state direction because it recreates per-format runtime
fragmentation inside the new crate.

## Recommended Next Phase

The next phase should be: finish Phase 2 properly, then consolidate the first
flagship driver around those primitives.

### Step 1: finish the shared runtime and composite source layer

Add the missing generic building blocks before doing another broad parity push:

- `CompressionUnitDataSource`
- `ChunkedCompressedDataSource`
- `LayeredDataSource`
- `runtime::cache`
- `runtime::decode`
- optional `runtime::metrics` hooks once shared caches exist

Why this comes first:

- NTFS compressed data paths should reuse it.
- EWF chunk decode should reuse it.
- QCOW, VMDK, VHDX parent/backing behavior should reuse it.
- UDIF and future compressed formats should stop carrying bespoke runtime code.

### Step 2: widen resolver support beyond simple local sibling opening

The resolver layer should grow explicit patterns for:

- sibling segment lookup
- parent or backing file resolution
- image-specific related-file opening

Why this comes second:

- EWF, VMDK, QCOW, PDI, and sparsebundle all depend on multi-file correctness.
- A stronger resolver contract reduces the temptation to hide file-discovery
  policy inside individual drivers.

### Step 3: refactor EWF into the target flagship shape

Once the runtime and resolver layers are stronger, EWF should be the first
driver deliberately reshaped around them.

That EWF pass should aim for:

- a clearer parser/model/runtime split
- an explicit segment repository abstraction
- immutable section and chunk metadata catalogs
- shared chunk decode cache logic built on common runtime pieces
- a local cursor or equivalent sequential adapter over immutable shared state

Why EWF is still the right proving ground:

- it combines explicit sibling resolution, chunk mapping, compressed reads, and
  shared cache pressure in one format family
- it is the clearest test of whether `keramics-drivers` is actually escaping
  the old shared-cursor model rather than only renaming it

### Step 4: use the new primitives to close the highest-value parity gaps

After EWF proves the reusable runtime path, the next ports should focus on the
gaps that directly benefit from those new abstractions:

1. NTFS compressed and WoF-backed data paths
2. QCOW backing chains and compressed clusters
3. VMDK multi-extent and compressed-grain coverage
4. HFS overflow extents
5. XFS btree-backed file data

## Sequencing Rules

To keep the migration aligned with the original redesign goals:

- do not add more bespoke per-driver cache systems when a shared runtime helper
  should exist instead
- do not push deeper NTFS compression work before `CompressionUnitDataSource`
  exists
- do not push deeper QCOW or VMDK parity before the resolver and layered-source
  building blocks are ready
- keep `keramics-formats` as the behavioral baseline instead of trying to wrap
  it into `keramics-drivers`

## Exit Criteria For This Next Phase

This phase should be considered complete when all of the following are true:

- the missing shared runtime primitives exist and are used by at least one real
  driver
- EWF has been refactored to use the common infrastructure rather than a
  format-local runtime shape
- at least one of NTFS or a layered image driver has been migrated onto the new
  shared primitives after EWF
- parity fixtures and clippy-clean tests exist for the new infrastructure

At that point, the project can move into the next parity-focused wave with much
lower risk of architectural drift.
