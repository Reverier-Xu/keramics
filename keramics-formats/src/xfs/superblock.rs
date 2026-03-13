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

use keramics_core::ErrorTrace;
use keramics_encodings::CharacterEncoding;
use keramics_types::{ByteString, Uuid, bytes_to_u16_be, bytes_to_u32_be, bytes_to_u64_be};

use super::constants::*;

/// XFS superblock.
pub struct XfsSuperblock {
    /// Block size.
    pub block_size: u32,

    /// Sector size.
    pub sector_size: u16,

    /// Inode size.
    pub inode_size: u16,

    /// Number of inodes per block.
    pub inodes_per_block: u16,

    /// Number of inodes per block in log2.
    pub inodes_per_block_log2: u8,

    /// Number of blocks per allocation group.
    pub allocation_group_block_size: u32,

    /// Number of allocation groups.
    pub number_of_allocation_groups: u32,

    /// Root inode number.
    pub root_inode_number: u64,

    /// Format version.
    pub format_version: u8,

    /// Secondary feature flags.
    pub secondary_feature_flags: u32,

    /// Incompatible feature flags.
    pub incompatible_feature_flags: u32,

    /// Directory block size.
    pub directory_block_size: u32,

    /// Relative block bits.
    pub relative_block_bits: u8,

    /// Relative inode bits.
    pub relative_inode_bits: u8,

    /// File system identifier.
    pub file_system_identifier: Uuid,

    /// Volume label.
    pub volume_label: ByteString,
}

impl XfsSuperblock {
    /// Creates a new superblock.
    pub fn new(encoding: &CharacterEncoding) -> Self {
        Self {
            block_size: 0,
            sector_size: 0,
            inode_size: 0,
            inodes_per_block: 0,
            inodes_per_block_log2: 0,
            allocation_group_block_size: 0,
            number_of_allocation_groups: 0,
            root_inode_number: 0,
            format_version: 0,
            secondary_feature_flags: 0,
            incompatible_feature_flags: 0,
            directory_block_size: 0,
            relative_block_bits: 0,
            relative_inode_bits: 0,
            file_system_identifier: Uuid::new(),
            volume_label: ByteString::new_with_encoding(encoding),
        }
    }

    /// Determines if the file system stores file types in directory entries.
    pub fn has_file_types(&self) -> bool {
        self.format_version == 5
            || self.secondary_feature_flags & XFS_SUPERBLOCK_FEATURE2_FILE_TYPE != 0
            || self.incompatible_feature_flags & XFS_SUPERBLOCK_INCOMPATIBLE_FEATURE_FILE_TYPE != 0
    }

    /// Reads the superblock from a buffer.
    pub fn read_data(&mut self, data: &[u8]) -> Result<(), ErrorTrace> {
        if data.len() != 512 {
            return Err(keramics_core::error_trace_new!("Unsupported data size"));
        }
        if &data[0..4] != XFS_SUPERBLOCK_SIGNATURE {
            return Err(keramics_core::error_trace_new!("Unsupported signature"));
        }
        self.block_size = bytes_to_u32_be!(data, 4);
        self.file_system_identifier = Uuid::from_be_bytes(&data[32..48]);
        self.root_inode_number = bytes_to_u64_be!(data, 56) & XFS_MAX_INODE_NUMBER;
        self.allocation_group_block_size = bytes_to_u32_be!(data, 84);
        self.number_of_allocation_groups = bytes_to_u32_be!(data, 88);

        let version_flags: u16 = bytes_to_u16_be!(data, 100);
        self.format_version = (version_flags & 0x000f) as u8;
        self.sector_size = bytes_to_u16_be!(data, 102);
        self.inode_size = bytes_to_u16_be!(data, 104);
        self.inodes_per_block = bytes_to_u16_be!(data, 106);
        self.volume_label.read_data(&data[108..120]);

        self.inodes_per_block_log2 = data[123];
        self.relative_block_bits = data[124];

        let directory_block_log2: u8 = data[192];
        self.secondary_feature_flags = bytes_to_u32_be!(data, 200);
        self.incompatible_feature_flags = bytes_to_u32_be!(data, 216);

        if !(4..=5).contains(&self.format_version) {
            return Err(keramics_core::error_trace_new!(format!(
                "Unsupported format version: {}",
                self.format_version
            )));
        }
        if !(512..=32768).contains(&self.sector_size) {
            return Err(keramics_core::error_trace_new!(format!(
                "Unsupported sector size: {}",
                self.sector_size
            )));
        }
        if !(512..=65536).contains(&self.block_size) {
            return Err(keramics_core::error_trace_new!(format!(
                "Unsupported block size: {}",
                self.block_size
            )));
        }
        if !(256..=2048).contains(&self.inode_size) {
            return Err(keramics_core::error_trace_new!(format!(
                "Unsupported inode size: {}",
                self.inode_size
            )));
        }
        if self.allocation_group_block_size < 5 || self.number_of_allocation_groups == 0 {
            return Err(keramics_core::error_trace_new!(
                "Invalid allocation group geometry"
            ));
        }
        if self.relative_block_bits == 0 || self.relative_block_bits > 31 {
            return Err(keramics_core::error_trace_new!(
                "Invalid allocation group size log2"
            ));
        }
        if self.inodes_per_block_log2 == 0
            || self.inodes_per_block_log2 > (32 - self.relative_block_bits)
        {
            return Err(keramics_core::error_trace_new!(
                "Invalid inodes per block log2 value"
            ));
        }
        if (1u64 << self.inodes_per_block_log2) != self.inodes_per_block as u64 {
            return Err(keramics_core::error_trace_new!(
                "Mismatch between number of inodes per block and log2 values"
            ));
        }
        self.directory_block_size = if directory_block_log2 == 0 {
            self.block_size
        } else {
            let multiplier: u32 = match 1u32.checked_shl(directory_block_log2 as u32) {
                Some(value) => value,
                None => {
                    return Err(keramics_core::error_trace_new!(
                        "Invalid directory block size log2"
                    ));
                }
            };
            match self.block_size.checked_mul(multiplier) {
                Some(value) => value,
                None => {
                    return Err(keramics_core::error_trace_new!(
                        "Directory block size overflow"
                    ));
                }
            }
        };
        self.relative_inode_bits = match self
            .relative_block_bits
            .checked_add(self.inodes_per_block_log2)
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Relative inode bits overflow"
                ));
            }
        };
        if self.relative_inode_bits == 0 || self.relative_inode_bits >= 32 {
            return Err(keramics_core::error_trace_new!(
                "Invalid relative inode bits value"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_data(root_inode_number: u64) -> Vec<u8> {
        let mut data: Vec<u8> = vec![0; 512];

        data[0..4].copy_from_slice(XFS_SUPERBLOCK_SIGNATURE);
        data[4..8].copy_from_slice(&4096u32.to_be_bytes());
        data[56..64].copy_from_slice(&root_inode_number.to_be_bytes());
        data[84..88].copy_from_slice(&8u32.to_be_bytes());
        data[88..92].copy_from_slice(&1u32.to_be_bytes());
        data[100..102].copy_from_slice(&5u16.to_be_bytes());
        data[102..104].copy_from_slice(&512u16.to_be_bytes());
        data[104..106].copy_from_slice(&512u16.to_be_bytes());
        data[106..108].copy_from_slice(&8u16.to_be_bytes());
        data[108..116].copy_from_slice(b"xfs_test");
        data[123] = 3;
        data[124] = 3;
        data[192] = 0;
        data[200..204].copy_from_slice(&XFS_SUPERBLOCK_FEATURE2_FILE_TYPE.to_be_bytes());

        data
    }

    #[test]
    fn test_read_data() -> Result<(), ErrorTrace> {
        let test_data: Vec<u8> = build_test_data(2);

        let mut superblock: XfsSuperblock = XfsSuperblock::new(&CharacterEncoding::Utf8);
        superblock.read_data(&test_data)?;

        assert_eq!(superblock.block_size, 4096);
        assert_eq!(superblock.sector_size, 512);
        assert_eq!(superblock.inode_size, 512);
        assert_eq!(superblock.inodes_per_block, 8);
        assert_eq!(superblock.root_inode_number, 2);
        assert_eq!(superblock.format_version, 5);
        assert_eq!(superblock.directory_block_size, 4096);
        assert_eq!(superblock.volume_label, ByteString::from("xfs_test"));
        assert!(superblock.has_file_types());

        Ok(())
    }

    #[test]
    fn test_read_data_with_invalid_signature() {
        let mut test_data: Vec<u8> = build_test_data(2);
        test_data[0] = 0xff;

        let mut superblock: XfsSuperblock = XfsSuperblock::new(&CharacterEncoding::Utf8);
        let result = superblock.read_data(&test_data);

        assert!(result.is_err());
    }
}
