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

use std::collections::BTreeMap;

use keramics_core::{DataStreamReference, ErrorTrace};
use keramics_encodings::CharacterEncoding;
use keramics_types::ByteString;

use crate::path_component::PathComponent;

use super::block_range::{XfsBlockRange, XfsBlockRangeType};
use super::constants::*;
use super::directory_entry::{XfsDirectoryEntry, read_block_entries, read_shortform_entries};
use super::util::read_data_at_offset;

/// XFS directory entries.
pub struct XfsDirectoryEntries {
    /// Character encoding.
    pub encoding: CharacterEncoding,

    /// Entries.
    entries: BTreeMap<ByteString, XfsDirectoryEntry>,

    /// Value to indicate the directory entries were read.
    is_read: bool,
}

impl XfsDirectoryEntries {
    /// Creates new directory entries.
    pub fn new(encoding: &CharacterEncoding) -> Self {
        Self {
            encoding: encoding.clone(),
            entries: BTreeMap::new(),
            is_read: false,
        }
    }

    /// Retrieves a specific directory entry.
    pub fn get_entry_by_index(
        &self,
        entry_index: usize,
    ) -> Option<(&ByteString, &XfsDirectoryEntry)> {
        self.entries.iter().nth(entry_index)
    }

    /// Retrieves a specific directory entry by name.
    pub fn get_entry_by_name(
        &self,
        name: &PathComponent,
    ) -> Result<Option<(&ByteString, &XfsDirectoryEntry)>, ErrorTrace> {
        let lookup_name: ByteString = match name.to_byte_string(&self.encoding) {
            Ok(byte_string) => byte_string,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    "Unable to convert path component to byte string"
                );
                return Err(error);
            }
        };
        Ok(self.entries.get_key_value(&lookup_name))
    }

    /// Retrieves the number of entries.
    pub fn get_number_of_entries(&self) -> usize {
        self.entries.len()
    }

    /// Determines if the directory entries were read.
    pub fn is_read(&self) -> bool {
        self.is_read
    }

    /// Reads the directory entries from block data.
    pub fn read_block_data(
        &mut self,
        data_stream: &DataStreamReference,
        block_size: u32,
        directory_block_size: u32,
        block_ranges: &[XfsBlockRange],
        has_file_types: bool,
    ) -> Result<(), ErrorTrace> {
        for block_range in block_ranges.iter() {
            if block_range.range_type == XfsBlockRangeType::Sparse
                || block_range.number_of_blocks == 0
                || block_range.logical_block_number >= XFS_DIRECTORY_LEAF_OFFSET
            {
                continue;
            }
            let range_physical_offset: u64 =
                block_range.physical_block_number * (block_size as u64);
            let range_size: u64 = block_range.number_of_blocks * (block_size as u64);
            let mut range_offset: u64 = 0;
            let directory_block_size: u64 = directory_block_size as u64;
            let range_logical_offset: u64 = block_range.logical_block_number * (block_size as u64);

            while range_offset + directory_block_size <= range_size {
                let logical_offset: u64 = range_logical_offset + range_offset;

                if logical_offset >= XFS_DIRECTORY_LEAF_OFFSET {
                    break;
                }
                let data: Vec<u8> = match read_data_at_offset(
                    data_stream,
                    range_physical_offset + range_offset,
                    directory_block_size as usize,
                ) {
                    Ok(data) => data,
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            "Unable to read directory block data"
                        );
                        return Err(error);
                    }
                };
                match read_block_entries(&data, &self.encoding, has_file_types, &mut self.entries) {
                    Ok(_) => {}
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            "Unable to read directory entries from block data"
                        );
                        return Err(error);
                    }
                }
                range_offset += directory_block_size;
            }
        }
        self.is_read = true;

        Ok(())
    }

    /// Reads the directory entries from inline data.
    pub fn read_inline_data(
        &mut self,
        data: &[u8],
        has_file_types: bool,
    ) -> Result<(), ErrorTrace> {
        match read_shortform_entries(data, &self.encoding, has_file_types, &mut self.entries) {
            Ok(_) => {}
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    "Unable to read directory entries from inline data"
                );
                return Err(error);
            }
        }
        self.is_read = true;

        Ok(())
    }
}
