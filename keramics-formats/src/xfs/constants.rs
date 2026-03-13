/* Copyright 2026 Reverier Xu <reverier.xu@woooo.tech>
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License. You may
 * obtain a copy of the License at https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
 * WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
 * License for the specific language governing permissions and limitations
 * under the License.
 */

/// XFS superblock signature: "XFSB"
pub(crate) const XFS_SUPERBLOCK_SIGNATURE: &[u8] = b"XFSB";

/// XFS inode signature: "IN"
pub(super) const XFS_INODE_SIGNATURE: &[u8] = b"IN";

/// XFS inode data fork format values.
pub(super) const XFS_INODE_FORMAT_LOCAL: u8 = 1;
pub(super) const XFS_INODE_FORMAT_EXTENTS: u8 = 2;
pub(super) const XFS_INODE_FORMAT_BTREE: u8 = 3;

/// XFS file mode mask and file types.
pub(super) const XFS_FILE_MODE_TYPE_MASK: u16 = 0xf000;
pub const XFS_FILE_MODE_TYPE_DIRECTORY: u16 = 0x4000;
pub const XFS_FILE_MODE_TYPE_REGULAR_FILE: u16 = 0x8000;
pub const XFS_FILE_MODE_TYPE_SYMBOLIC_LINK: u16 = 0xa000;

/// XFS superblock feature flags.
pub(super) const XFS_SUPERBLOCK_FEATURE2_FILE_TYPE: u32 = 0x0000_0200;
pub(super) const XFS_SUPERBLOCK_INCOMPATIBLE_FEATURE_FILE_TYPE: u32 = 0x0000_0001;

/// XFS inode feature flags.
pub(super) const XFS_INODE_FLAGS2_BIGTIME: u64 = 0x0000_0000_0000_0008;
pub(super) const XFS_INODE_FLAGS2_NREXT64: u64 = 0x0000_0000_0000_0010;

/// XFS directory related constants.
pub(super) const XFS_DIRECTORY_LEAF_OFFSET: u64 = 0x0000_0008_0000_0000;

/// XFS extent and inode btree signatures.
pub(super) const XFS_BTREE_BLOCK_SIGNATURE_V4: &[u8; 4] = b"BMAP";
pub(super) const XFS_BTREE_BLOCK_SIGNATURE_V5: &[u8; 4] = b"BMA3";
pub(super) const XFS_INODE_BTREE_SIGNATURE_V4: &[u8; 4] = b"IABT";
pub(super) const XFS_INODE_BTREE_SIGNATURE_V5: &[u8; 4] = b"IAB3";

/// XFS inode geometry constants.
pub(super) const XFS_INODES_PER_CHUNK: u64 = 64;
pub(super) const XFS_MAX_INODE_NUMBER: u64 = (1u64 << 56) - 1;

/// Bigtime timestamps are stored relative to 1901-12-13 20:45:52 UTC.
pub(super) const XFS_BIGTIME_EPOCH_OFFSET: i64 = 2_147_483_648;
