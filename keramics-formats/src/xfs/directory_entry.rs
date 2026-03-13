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

use keramics_core::ErrorTrace;
use keramics_encodings::CharacterEncoding;
use keramics_types::{ByteString, bytes_to_u16_be, bytes_to_u32_be, bytes_to_u64_be};

use super::constants::*;
use super::util::get_data_slice;

/// XFS directory entry.
#[derive(Clone, Debug)]
pub struct XfsDirectoryEntry {
    /// Inode number.
    pub inode_number: u64,
}

impl XfsDirectoryEntry {
    /// Creates a new directory entry.
    pub fn new(inode_number: u64) -> Self {
        Self { inode_number }
    }
}

/// Reads XFS shortform directory entries.
pub(super) fn read_shortform_entries(
    data: &[u8],
    encoding: &CharacterEncoding,
    has_file_types: bool,
    entries: &mut BTreeMap<ByteString, XfsDirectoryEntry>,
) -> Result<(), ErrorTrace> {
    if data.len() < 2 {
        return Err(keramics_core::error_trace_new!(
            "Unsupported shortform directory data size"
        ));
    }
    let number_of_entries_32bit: usize = data[0] as usize;
    let number_of_entries_64bit: usize = data[1] as usize;

    if number_of_entries_32bit != 0 && number_of_entries_64bit != 0 {
        return Err(keramics_core::error_trace_new!(
            "Unsupported shortform directory entry counters"
        ));
    }
    let (number_of_entries, inode_size, mut data_offset): (usize, usize, usize) =
        if number_of_entries_64bit == 0 {
            (number_of_entries_32bit, 4, 6)
        } else {
            (number_of_entries_64bit, 8, 10)
        };

    for _ in 0..number_of_entries {
        if data_offset >= data.len() {
            return Err(keramics_core::error_trace_new!(
                "Shortform directory entry data offset value out of bounds"
            ));
        }
        let name_size: usize = data[data_offset] as usize;
        let mut entry_size: usize = match 3usize.checked_add(name_size) {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Shortform directory entry size value out of bounds"
                ));
            }
        };

        if has_file_types {
            entry_size = match entry_size.checked_add(1) {
                Some(value) => value,
                None => {
                    return Err(keramics_core::error_trace_new!(
                        "Shortform directory entry size value out of bounds"
                    ));
                }
            };
        }
        entry_size = match entry_size.checked_add(inode_size) {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Shortform directory entry size value out of bounds"
                ));
            }
        };

        _ = get_data_slice(data, data_offset, entry_size)?;
        data_offset += 1;
        data_offset += 2;

        let name: ByteString = read_name(data, data_offset, name_size, encoding)?;
        data_offset += name_size;

        if has_file_types {
            data_offset += 1;
        }
        let inode_number: u64 = if inode_size == 4 {
            bytes_to_u32_be!(data, data_offset) as u64
        } else {
            bytes_to_u64_be!(data, data_offset)
        } & XFS_MAX_INODE_NUMBER;
        data_offset += inode_size;

        if name != "." && name != ".." {
            entries.insert(name, XfsDirectoryEntry::new(inode_number));
        }
    }
    Ok(())
}

/// Reads XFS block directory entries.
pub(super) fn read_block_entries(
    data: &[u8],
    encoding: &CharacterEncoding,
    has_file_types: bool,
    entries: &mut BTreeMap<ByteString, XfsDirectoryEntry>,
) -> Result<(), ErrorTrace> {
    let signature: &[u8] = get_data_slice(data, 0, 4)?;
    let (header_size, has_footer): (usize, bool) = match signature {
        b"XD2B" => (16, true),
        b"XD2D" => (16, false),
        b"XDB3" => (64, true),
        b"XDD3" => (64, false),
        b"XD2L" | b"XD2N" | b"XD2F" | b"XDL3" | b"XDN3" | b"XDF3" => return Ok(()),
        _ => {
            return Err(keramics_core::error_trace_new!(format!(
                "Unsupported directory block signature: {:02x}{:02x}{:02x}{:02x}",
                data[0], data[1], data[2], data[3]
            )));
        }
    };
    let entries_end_offset: usize = if has_footer {
        let footer_data: &[u8] = get_data_slice(data, data.len() - 8, 8)?;
        let number_of_entries: usize = bytes_to_u32_be!(footer_data, 0) as usize;
        let hash_data_size: usize = match number_of_entries.checked_mul(8) {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Directory hash table size value out of bounds"
                ));
            }
        };
        match data.len().checked_sub(8 + hash_data_size) {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Invalid directory hash table data size"
                ));
            }
        }
    } else {
        data.len()
    };
    let mut data_offset: usize = header_size;

    while data_offset < entries_end_offset {
        let header_data: &[u8] = get_data_slice(data, data_offset, 4)?;

        if bytes_to_u16_be!(header_data, 0) == 0xffff {
            let size: usize = bytes_to_u16_be!(header_data, 2) as usize;

            if size < 4 {
                return Err(keramics_core::error_trace_new!(
                    "Invalid free directory region size"
                ));
            }
            data_offset += size;
            continue;
        }
        let inode_number: u64 = bytes_to_u64_be!(data, data_offset) & XFS_MAX_INODE_NUMBER;
        let name_size: usize = data[data_offset + 8] as usize;

        let mut entry_size: usize = match 9usize
            .checked_add(name_size)
            .and_then(|value| value.checked_add(2))
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Directory entry size value out of bounds"
                ));
            }
        };

        if has_file_types {
            entry_size += 1;
        }
        let remainder_size: usize = entry_size % 8;
        if remainder_size != 0 {
            entry_size += 8 - remainder_size;
        }
        let name: ByteString = read_name(data, data_offset + 9, name_size, encoding)?;

        if name != "." && name != ".." {
            entries.insert(name, XfsDirectoryEntry::new(inode_number));
        }
        data_offset += entry_size;
    }
    Ok(())
}

/// Reads a directory entry name.
fn read_name(
    data: &[u8],
    data_offset: usize,
    data_size: usize,
    encoding: &CharacterEncoding,
) -> Result<ByteString, ErrorTrace> {
    let data: &[u8] = get_data_slice(data, data_offset, data_size)?;
    let mut name: ByteString = ByteString::new_with_encoding(encoding);
    name.read_data(data);

    Ok(name)
}
