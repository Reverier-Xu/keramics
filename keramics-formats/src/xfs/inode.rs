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

use std::cmp::min;

use keramics_core::ErrorTrace;
use keramics_datetime::{DateTime, PosixTime64Ns};
use keramics_types::{bytes_to_i32_be, bytes_to_u16_be, bytes_to_u32_be, bytes_to_u64_be};

use super::constants::*;

/// XFS inode.
pub struct XfsInode {
    /// File mode.
    pub file_mode: u16,

    /// Inode format version.
    pub format_version: u8,

    /// Data fork format.
    pub data_fork_format: u8,

    /// Owner identifier.
    pub owner_identifier: u32,

    /// Group identifier.
    pub group_identifier: u32,

    /// Number of links.
    pub number_of_links: u32,

    /// Access time.
    pub access_time: Option<DateTime>,

    /// Modification time.
    pub modification_time: Option<DateTime>,

    /// Change time.
    pub change_time: Option<DateTime>,

    /// Creation time.
    pub creation_time: Option<DateTime>,

    /// Data size.
    pub data_size: u64,

    /// Number of extents.
    pub number_of_extents: u64,

    /// Data fork.
    pub data_fork: Vec<u8>,

    /// Inline data.
    pub inline_data: Option<Vec<u8>>,
}

impl XfsInode {
    /// Creates a new inode.
    pub fn new() -> Self {
        Self {
            file_mode: 0,
            format_version: 0,
            data_fork_format: 0,
            owner_identifier: 0,
            group_identifier: 0,
            number_of_links: 0,
            access_time: None,
            modification_time: None,
            change_time: None,
            creation_time: None,
            data_size: 0,
            number_of_extents: 0,
            data_fork: Vec::new(),
            inline_data: None,
        }
    }

    /// Determines if the inode is a directory.
    pub fn is_directory(&self) -> bool {
        self.file_mode & XFS_FILE_MODE_TYPE_MASK == XFS_FILE_MODE_TYPE_DIRECTORY
    }

    /// Determines if the inode is a regular file.
    pub fn is_regular_file(&self) -> bool {
        self.file_mode & XFS_FILE_MODE_TYPE_MASK == XFS_FILE_MODE_TYPE_REGULAR_FILE
    }

    /// Determines if the inode is a symbolic link.
    pub fn is_symbolic_link(&self) -> bool {
        self.file_mode & XFS_FILE_MODE_TYPE_MASK == XFS_FILE_MODE_TYPE_SYMBOLIC_LINK
    }

    /// Reads the inode from a buffer.
    pub fn read_data(&mut self, data: &[u8]) -> Result<(), ErrorTrace> {
        if data.len() < 100 {
            return Err(keramics_core::error_trace_new!("Unsupported data size"));
        }
        if &data[0..2] != XFS_INODE_SIGNATURE {
            return Err(keramics_core::error_trace_new!("Unsupported signature"));
        }
        self.file_mode = bytes_to_u16_be!(data, 2);
        self.format_version = data[4];
        self.data_fork_format = data[5];

        if !matches!(self.format_version, 1..=3) {
            return Err(keramics_core::error_trace_new!(format!(
                "Unsupported inode format version: {}",
                self.format_version
            )));
        }
        self.owner_identifier = bytes_to_u32_be!(data, 8);
        self.group_identifier = bytes_to_u32_be!(data, 12);
        self.number_of_links = if self.format_version == 1 {
            bytes_to_u16_be!(data, 6) as u32
        } else {
            bytes_to_u32_be!(data, 16)
        };

        let flags2: u64 = if self.format_version == 3 {
            bytes_to_u64_be!(data, 120)
        } else {
            0
        };
        let use_bigtime: bool = self.format_version == 3 && flags2 & XFS_INODE_FLAGS2_BIGTIME != 0;

        self.access_time = Some(read_timestamp(&data[32..40], use_bigtime)?);
        self.modification_time = Some(read_timestamp(&data[40..48], use_bigtime)?);
        self.change_time = Some(read_timestamp(&data[48..56], use_bigtime)?);
        self.data_size = bytes_to_u64_be!(data, 56);
        self.number_of_extents = if flags2 & XFS_INODE_FLAGS2_NREXT64 != 0 {
            bytes_to_u64_be!(data, 24)
        } else {
            bytes_to_u32_be!(data, 76) as u64
        };

        let attribute_fork_offset: usize = (data[82] as usize) * 8;
        let core_size: usize = if self.format_version == 3 { 176 } else { 100 };

        if data.len() < core_size {
            return Err(keramics_core::error_trace_new!(
                "Unsupported inode core size"
            ));
        }
        let mut data_fork_size: usize = data.len() - core_size;

        if attribute_fork_offset > 0 {
            if attribute_fork_offset > data_fork_size {
                return Err(keramics_core::error_trace_new!(
                    "Invalid attribute fork offset value out of bounds"
                ));
            }
            data_fork_size = attribute_fork_offset;
        }
        self.data_fork = data[core_size..core_size + data_fork_size].to_vec();
        self.inline_data = if self.data_fork_format == XFS_INODE_FORMAT_LOCAL {
            if self.data_size as usize > self.data_fork.len() {
                return Err(keramics_core::error_trace_new!(
                    "Inline data size value out of bounds"
                ));
            }
            Some(self.data_fork[..min(self.data_fork.len(), self.data_size as usize)].to_vec())
        } else {
            None
        };
        self.creation_time = if self.format_version == 3 {
            Some(read_timestamp(&data[144..152], use_bigtime)?)
        } else {
            None
        };

        Ok(())
    }
}

/// Reads an XFS timestamp from a buffer.
fn read_timestamp(data: &[u8], use_bigtime: bool) -> Result<DateTime, ErrorTrace> {
    if use_bigtime {
        let timestamp_value: u64 = bytes_to_u64_be!(data, 0);
        let seconds: i64 = (timestamp_value / 1_000_000_000) as i64 - XFS_BIGTIME_EPOCH_OFFSET;
        let fraction: u32 = (timestamp_value % 1_000_000_000) as u32;

        Ok(DateTime::PosixTime64Ns(PosixTime64Ns::new(
            seconds, fraction,
        )))
    } else {
        let seconds: i32 = bytes_to_i32_be!(data, 0);
        let fraction: u32 = bytes_to_u32_be!(data, 4);

        Ok(DateTime::PosixTime64Ns(PosixTime64Ns::new(
            seconds as i64,
            fraction,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_data() -> Result<(), ErrorTrace> {
        let mut data: Vec<u8> = vec![0; 256];

        data[0..2].copy_from_slice(XFS_INODE_SIGNATURE);
        data[2..4].copy_from_slice(&XFS_FILE_MODE_TYPE_REGULAR_FILE.to_be_bytes());
        data[4] = 3;
        data[5] = XFS_INODE_FORMAT_LOCAL;
        data[8..12].copy_from_slice(&1000u32.to_be_bytes());
        data[12..16].copy_from_slice(&1001u32.to_be_bytes());
        data[16..20].copy_from_slice(&2u32.to_be_bytes());
        data[32..36].copy_from_slice(&10i32.to_be_bytes());
        data[40..44].copy_from_slice(&20i32.to_be_bytes());
        data[48..52].copy_from_slice(&30i32.to_be_bytes());
        data[56..64].copy_from_slice(&3u64.to_be_bytes());
        data[76..80].copy_from_slice(&0u32.to_be_bytes());
        data[144..148].copy_from_slice(&40i32.to_be_bytes());
        data[176..179].copy_from_slice(b"abc");

        let mut inode: XfsInode = XfsInode::new();
        inode.read_data(&data)?;

        assert_eq!(inode.file_mode, XFS_FILE_MODE_TYPE_REGULAR_FILE);
        assert_eq!(inode.owner_identifier, 1000);
        assert_eq!(inode.group_identifier, 1001);
        assert_eq!(inode.number_of_links, 2);
        assert_eq!(inode.data_size, 3);
        assert_eq!(inode.inline_data, Some(b"abc".to_vec()));
        assert!(inode.is_regular_file());
        assert!(!inode.is_directory());

        Ok(())
    }

    #[test]
    fn test_read_data_with_invalid_signature() {
        let data: Vec<u8> = vec![0; 256];

        let mut inode: XfsInode = XfsInode::new();
        let result = inode.read_data(&data);

        assert!(result.is_err());
    }
}
