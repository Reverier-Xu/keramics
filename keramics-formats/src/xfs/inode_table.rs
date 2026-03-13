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

use keramics_core::{DataStreamReference, ErrorTrace};
use keramics_types::{bytes_to_u16_be, bytes_to_u32_be};

use super::constants::*;
use super::inode::XfsInode;
use super::superblock::XfsSuperblock;
use super::util::{get_data_slice, read_data_at_offset};

/// XFS inode table helper.
pub(super) struct XfsInodeTable {
    /// Format version.
    pub format_version: u8,

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

    /// Relative block bits.
    pub relative_block_bits: u8,

    /// Relative inode bits.
    pub relative_inode_bits: u8,
}

impl XfsInodeTable {
    /// Creates a new inode table helper.
    pub fn new() -> Self {
        Self {
            format_version: 0,
            block_size: 0,
            sector_size: 0,
            inode_size: 0,
            inodes_per_block: 0,
            inodes_per_block_log2: 0,
            allocation_group_block_size: 0,
            number_of_allocation_groups: 0,
            relative_block_bits: 0,
            relative_inode_bits: 0,
        }
    }

    /// Initializes the inode table helper.
    pub fn initialize(&mut self, superblock: &XfsSuperblock) -> Result<(), ErrorTrace> {
        self.format_version = superblock.format_version;
        self.block_size = superblock.block_size;
        self.sector_size = superblock.sector_size;
        self.inode_size = superblock.inode_size;
        self.inodes_per_block = superblock.inodes_per_block;
        self.inodes_per_block_log2 = superblock.inodes_per_block_log2;
        self.allocation_group_block_size = superblock.allocation_group_block_size;
        self.number_of_allocation_groups = superblock.number_of_allocation_groups;
        self.relative_block_bits = superblock.relative_block_bits;
        self.relative_inode_bits = superblock.relative_inode_bits;

        Ok(())
    }

    /// Retrieves an inode by inode number.
    pub fn get_inode(
        &self,
        data_stream: &DataStreamReference,
        inode_number: u64,
    ) -> Result<XfsInode, ErrorTrace> {
        let inode_offset: u64 = match self.get_inode_offset(data_stream, inode_number) {
            Ok(inode_offset) => inode_offset,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to determine inode offset");
                return Err(error);
            }
        };
        let data: Vec<u8> =
            match read_data_at_offset(data_stream, inode_offset, self.inode_size as usize) {
                Ok(data) => data,
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(error, "Unable to read inode data");
                    return Err(error);
                }
            };
        let mut inode: XfsInode = XfsInode::new();

        match inode.read_data(&data) {
            Ok(_) => {}
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to read inode data");
                return Err(error);
            }
        }
        Ok(inode)
    }

    /// Converts a filesystem block number into an absolute block number.
    pub fn fs_block_number_to_absolute_block_number(
        &self,
        fs_block_number: u64,
    ) -> Result<u64, ErrorTrace> {
        let allocation_group_number: u64 = fs_block_number >> self.relative_block_bits;

        if allocation_group_number >= self.number_of_allocation_groups as u64 {
            return Err(keramics_core::error_trace_new!(format!(
                "Filesystem block allocation group number: {} value out of bounds",
                allocation_group_number
            )));
        }
        let allocation_group_block_mask: u64 = (1u64 << self.relative_block_bits) - 1;
        let allocation_group_block_number: u64 = fs_block_number & allocation_group_block_mask;

        if allocation_group_block_number >= self.allocation_group_block_size as u64 {
            return Err(keramics_core::error_trace_new!(format!(
                "Filesystem relative block number: {} value out of bounds",
                allocation_group_block_number
            )));
        }
        let absolute_block_number: u128 = match (allocation_group_number as u128)
            .checked_mul(self.allocation_group_block_size as u128)
            .and_then(|value| value.checked_add(allocation_group_block_number as u128))
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Absolute block number value out of bounds"
                ));
            }
        };
        match u64::try_from(absolute_block_number) {
            Ok(value) => Ok(value),
            Err(_) => Err(keramics_core::error_trace_new!(
                "Absolute block number value out of bounds"
            )),
        }
    }

    /// Determines the inode offset.
    fn get_inode_offset(
        &self,
        data_stream: &DataStreamReference,
        inode_number: u64,
    ) -> Result<u64, ErrorTrace> {
        let inode_number: u64 = inode_number & XFS_MAX_INODE_NUMBER;
        let maximum_inode_number: u64 = ((self.number_of_allocation_groups as u64)
            << self.relative_inode_bits)
            .saturating_sub(1);

        if inode_number == 0 || inode_number > maximum_inode_number {
            return Err(keramics_core::error_trace_new!(format!(
                "Invalid inode number: {} value out of bounds",
                inode_number
            )));
        }
        let allocation_group_inode_bits: u8 = self.relative_inode_bits;
        let allocation_group_number: u64 = inode_number >> allocation_group_inode_bits;

        if allocation_group_number >= self.number_of_allocation_groups as u64 {
            return Err(keramics_core::error_trace_new!(format!(
                "Invalid allocation group number: {} value out of bounds",
                allocation_group_number
            )));
        }
        let allocation_group_inode_mask: u64 = (1u64 << allocation_group_inode_bits) - 1;
        let allocation_group_inode_number: u64 = inode_number & allocation_group_inode_mask;
        let allocation_group_block_number: u64 =
            allocation_group_inode_number >> self.inodes_per_block_log2;

        if allocation_group_block_number >= self.allocation_group_block_size as u64 {
            return Err(keramics_core::error_trace_new!(format!(
                "Invalid allocation group block number: {} value out of bounds",
                allocation_group_block_number
            )));
        }
        if !self.inode_chunk_exists(
            data_stream,
            allocation_group_number,
            allocation_group_inode_number,
        )? {
            return Err(keramics_core::error_trace_new!(format!(
                "Missing inode chunk for inode: {}",
                inode_number
            )));
        }
        let inode_index: u64 =
            allocation_group_inode_number & ((1u64 << self.inodes_per_block_log2) - 1);
        let file_system_block_number: u128 = match (allocation_group_number as u128)
            .checked_mul(self.allocation_group_block_size as u128)
            .and_then(|value| value.checked_add(allocation_group_block_number as u128))
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Invalid inode block number value out of bounds"
                ));
            }
        };
        let inode_offset: u128 = match file_system_block_number
            .checked_mul(self.block_size as u128)
            .and_then(|value| value.checked_add((inode_index as u128) * (self.inode_size as u128)))
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Invalid inode offset value out of bounds"
                ));
            }
        };
        match u64::try_from(inode_offset) {
            Ok(value) => Ok(value),
            Err(_) => Err(keramics_core::error_trace_new!(
                "Invalid inode offset value out of bounds"
            )),
        }
    }

    /// Determines if an inode chunk exists in the inode btree.
    fn inode_chunk_exists(
        &self,
        data_stream: &DataStreamReference,
        allocation_group_number: u64,
        allocation_group_inode_number: u64,
    ) -> Result<bool, ErrorTrace> {
        let (root_block_number, number_of_levels): (u32, u32) =
            match self.read_allocation_group_inode_header(data_stream, allocation_group_number) {
                Ok(values) => values,
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to read allocation group inode header"
                    );
                    return Err(error);
                }
            };
        if root_block_number == 0 || number_of_levels == 0 {
            return Ok(false);
        }
        self.search_inode_btree_for_aginode(
            data_stream,
            allocation_group_number,
            root_block_number as u64,
            allocation_group_inode_number,
            0,
        )
    }

    /// Reads the AGI information for a specific allocation group.
    fn read_allocation_group_inode_header(
        &self,
        data_stream: &DataStreamReference,
        allocation_group_number: u64,
    ) -> Result<(u32, u32), ErrorTrace> {
        let allocation_group_offset: u128 = match (allocation_group_number as u128)
            .checked_mul(self.allocation_group_block_size as u128)
            .and_then(|value| value.checked_mul(self.block_size as u128))
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Allocation group offset value out of bounds"
                ));
            }
        };
        let agi_offset: u128 =
            match allocation_group_offset.checked_add((self.sector_size as u128) * 2) {
                Some(value) => value,
                None => {
                    return Err(keramics_core::error_trace_new!(
                        "Allocation group inode offset value out of bounds"
                    ));
                }
            };
        let agi_offset: u64 = match u64::try_from(agi_offset) {
            Ok(value) => value,
            Err(_) => {
                return Err(keramics_core::error_trace_new!(
                    "Allocation group inode offset value out of bounds"
                ));
            }
        };
        let data: Vec<u8> =
            match read_data_at_offset(data_stream, agi_offset, self.sector_size as usize) {
                Ok(data) => data,
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(error, "Unable to read AGI data");
                    return Err(error);
                }
            };
        if &data[0..4] != b"XAGI" {
            return Err(keramics_core::error_trace_new!("Unsupported AGI signature"));
        }
        Ok((bytes_to_u32_be!(data, 20), bytes_to_u32_be!(data, 24)))
    }

    /// Searches the inode btree for a specific allocation group inode number.
    fn search_inode_btree_for_aginode(
        &self,
        data_stream: &DataStreamReference,
        allocation_group_number: u64,
        allocation_group_block_number: u64,
        allocation_group_inode_number: u64,
        recursion_depth: usize,
    ) -> Result<bool, ErrorTrace> {
        if recursion_depth > 128 {
            return Err(keramics_core::error_trace_new!(
                "Inode btree recursion depth value out of bounds"
            ));
        }
        let file_system_block_number: u128 = match (allocation_group_number as u128)
            .checked_mul(self.allocation_group_block_size as u128)
            .and_then(|value| value.checked_add(allocation_group_block_number as u128))
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Inode btree block number value out of bounds"
                ));
            }
        };
        let block_offset: u128 = match file_system_block_number.checked_mul(self.block_size as u128)
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Inode btree block offset value out of bounds"
                ));
            }
        };
        let block_offset: u64 = match u64::try_from(block_offset) {
            Ok(value) => value,
            Err(_) => {
                return Err(keramics_core::error_trace_new!(
                    "Inode btree block offset value out of bounds"
                ));
            }
        };
        let data: Vec<u8> =
            match read_data_at_offset(data_stream, block_offset, self.block_size as usize) {
                Ok(data) => data,
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to read inode btree block"
                    );
                    return Err(error);
                }
            };
        let header_size: usize = if &data[0..4] == XFS_INODE_BTREE_SIGNATURE_V5 {
            56
        } else if &data[0..4] == XFS_INODE_BTREE_SIGNATURE_V4 {
            16
        } else {
            return Err(keramics_core::error_trace_new!(
                "Unsupported inode btree signature"
            ));
        };
        let level: u16 = bytes_to_u16_be!(data, 4);
        let number_of_records: usize = bytes_to_u16_be!(data, 6) as usize;

        if level == 0 {
            for record_index in 0..number_of_records {
                let record_data: &[u8] =
                    get_data_slice(&data, header_size + record_index * 16, 16)?;
                let first_inode_number: u64 = bytes_to_u32_be!(record_data, 0) as u64;

                if allocation_group_inode_number >= first_inode_number
                    && allocation_group_inode_number < first_inode_number + XFS_INODES_PER_CHUNK
                {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let records_data_size: usize = data.len() - header_size;
        let number_of_key_value_pairs: usize = records_data_size / 8;

        if number_of_records > number_of_key_value_pairs {
            return Err(keramics_core::error_trace_new!(
                "Inode btree record count value out of bounds"
            ));
        }
        let mut record_index: usize = 0;

        for key_index in 0..number_of_records {
            let key_data: &[u8] = get_data_slice(&data, header_size + key_index * 4, 4)?;
            let key_inode_number: u64 = bytes_to_u32_be!(key_data, 0) as u64;

            if allocation_group_inode_number < key_inode_number {
                break;
            }
            record_index += 1;
        }
        if record_index > 0 {
            let pointer_offset: usize =
                header_size + (number_of_key_value_pairs + record_index - 1) * 4;
            let pointer_data: &[u8] = get_data_slice(&data, pointer_offset, 4)?;
            let child_block_number: u64 = bytes_to_u32_be!(pointer_data, 0) as u64;

            if child_block_number >= self.allocation_group_block_size as u64 {
                return Err(keramics_core::error_trace_new!(format!(
                    "Inode btree child block number: {} value out of bounds",
                    child_block_number
                )));
            }
            return self.search_inode_btree_for_aginode(
                data_stream,
                allocation_group_number,
                child_block_number,
                allocation_group_inode_number,
                recursion_depth + 1,
            );
        }
        Ok(false)
    }
}
