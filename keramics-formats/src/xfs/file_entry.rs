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

use std::sync::{Arc, RwLock};

use keramics_core::{DataStream, DataStreamReference, ErrorTrace, FakeDataStream};
use keramics_datetime::DateTime;
use keramics_types::{ByteString, bytes_to_u16_be, bytes_to_u64_be};

use crate::path_component::PathComponent;

use super::block_range::{XfsBlockRange, XfsBlockRangeType};
use super::block_stream::XfsBlockStream;
use super::constants::*;
use super::directory_entries::XfsDirectoryEntries;
use super::directory_entry::XfsDirectoryEntry;
use super::extent::{normalize_sparse_block_ranges, parse_block_ranges};
use super::file_entries::XfsFileEntriesIterator;
use super::inode::XfsInode;
use super::inode_table::XfsInodeTable;
use super::util::{get_data_slice, read_data_at_offset};

/// XFS file entry.
pub struct XfsFileEntry {
    /// The data stream.
    data_stream: DataStreamReference,

    /// Inode table helper.
    inode_table: Arc<XfsInodeTable>,

    /// The inode number.
    inode_number: u64,

    /// Root inode number.
    root_inode_number: u64,

    /// The inode.
    inode: XfsInode,

    /// The name.
    name: Option<ByteString>,

    /// Directory block size.
    directory_block_size: u32,

    /// Value to indicate if directory entries contain file types.
    has_file_types: bool,

    /// Block ranges.
    block_ranges: Vec<XfsBlockRange>,

    /// Sub directory entries.
    sub_directory_entries: XfsDirectoryEntries,

    /// Symbolic link target.
    symbolic_link_target: Option<ByteString>,
}

impl XfsFileEntry {
    /// Creates a new file entry.
    pub(super) fn new(
        data_stream: &DataStreamReference,
        inode_table: &Arc<XfsInodeTable>,
        inode_number: u64,
        root_inode_number: u64,
        inode: XfsInode,
        name: Option<ByteString>,
        directory_block_size: u32,
        has_file_types: bool,
        sub_directory_entries: XfsDirectoryEntries,
    ) -> Self {
        Self {
            data_stream: data_stream.clone(),
            inode_table: inode_table.clone(),
            inode_number,
            root_inode_number,
            inode,
            name,
            directory_block_size,
            has_file_types,
            block_ranges: Vec::new(),
            sub_directory_entries,
            symbolic_link_target: None,
        }
    }

    /// Retrieves the access time.
    pub fn get_access_time(&self) -> Option<&DateTime> {
        self.inode.access_time.as_ref()
    }

    /// Retrieves the change time.
    pub fn get_change_time(&self) -> Option<&DateTime> {
        self.inode.change_time.as_ref()
    }

    /// Retrieves the creation time.
    pub fn get_creation_time(&self) -> Option<&DateTime> {
        self.inode.creation_time.as_ref()
    }

    /// Retrieves the file mode.
    pub fn get_file_mode(&self) -> u16 {
        self.inode.file_mode
    }

    /// Retrieves the group identifier.
    pub fn get_group_identifier(&self) -> u32 {
        self.inode.group_identifier
    }

    /// Retrieves the inode number.
    pub fn get_inode_number(&self) -> u64 {
        self.inode_number
    }

    /// Retrieves the modification time.
    pub fn get_modification_time(&self) -> Option<&DateTime> {
        self.inode.modification_time.as_ref()
    }

    /// Retrieves the name.
    pub fn get_name(&self) -> Option<&ByteString> {
        self.name.as_ref()
    }

    /// Retrieves the number of links.
    pub fn get_number_of_links(&self) -> u32 {
        self.inode.number_of_links
    }

    /// Retrieves the owner identifier.
    pub fn get_owner_identifier(&self) -> u32 {
        self.inode.owner_identifier
    }

    /// Retrieves the size.
    pub fn get_size(&self) -> u64 {
        if self.inode.is_regular_file() || self.inode.is_symbolic_link() {
            self.inode.data_size
        } else {
            0
        }
    }

    /// Retrieves the symbolic link target.
    pub fn get_symbolic_link_target(&mut self) -> Result<Option<&ByteString>, ErrorTrace> {
        if self.symbolic_link_target.is_none() && self.is_symbolic_link() {
            let mut target: ByteString =
                ByteString::new_with_encoding(&self.sub_directory_entries.encoding);

            if let Some(inline_data) = self.inode.inline_data.as_ref() {
                target.read_data(inline_data.as_slice());
            } else {
                self.read_block_ranges()?;

                let mut block_stream: XfsBlockStream = match self.get_block_stream() {
                    Ok(block_stream) => block_stream,
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            "Unable to retrieve block stream"
                        );
                        return Err(error);
                    }
                };
                let mut data: Vec<u8> = vec![0; self.inode.data_size as usize];

                match DataStream::read_exact(&mut block_stream, &mut data) {
                    Ok(_) => {}
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            "Unable to read symbolic link target data from block stream"
                        );
                        return Err(error);
                    }
                }
                target.read_data(data.as_slice());
            }
            self.symbolic_link_target = Some(target);
        }
        Ok(self.symbolic_link_target.as_ref())
    }

    /// Retrieves the default data stream.
    pub fn get_data_stream(&mut self) -> Result<Option<DataStreamReference>, ErrorTrace> {
        if !self.inode.is_regular_file() {
            return Ok(None);
        }
        if let Some(inline_data) = self.inode.inline_data.as_ref() {
            let data_stream: FakeDataStream =
                FakeDataStream::new(inline_data, self.inode.data_size);

            Ok(Some(Arc::new(RwLock::new(data_stream))))
        } else {
            self.read_block_ranges()?;

            match self.get_block_stream() {
                Ok(block_stream) => Ok(Some(Arc::new(RwLock::new(block_stream)))),
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(error, "Unable to retrieve block stream");
                    Err(error)
                }
            }
        }
    }

    /// Retrieves the number of sub file entries.
    pub fn get_number_of_sub_file_entries(&mut self) -> Result<usize, ErrorTrace> {
        if self.is_directory() && !self.sub_directory_entries.is_read() {
            match self.read_sub_directory_entries() {
                Ok(_) => {}
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to read sub directory entries"
                    );
                    return Err(error);
                }
            }
        }
        Ok(self.sub_directory_entries.get_number_of_entries())
    }

    /// Retrieves a specific sub file entry.
    pub fn get_sub_file_entry_by_index(
        &mut self,
        sub_file_entry_index: usize,
    ) -> Result<XfsFileEntry, ErrorTrace> {
        if self.is_directory() && !self.sub_directory_entries.is_read() {
            match self.read_sub_directory_entries() {
                Ok(_) => {}
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to read sub directory entries"
                    );
                    return Err(error);
                }
            }
        }
        let (name, directory_entry): (&ByteString, &XfsDirectoryEntry) = match self
            .sub_directory_entries
            .get_entry_by_index(sub_file_entry_index)
        {
            Some(entry) => entry,
            None => {
                return Err(keramics_core::error_trace_new!(format!(
                    "Unable to retrieve sub file entry: {}",
                    sub_file_entry_index
                )));
            }
        };
        self.create_sub_file_entry(name.clone(), directory_entry)
    }

    /// Retrieves a specific sub file entry.
    pub fn get_sub_file_entry_by_name(
        &mut self,
        sub_file_entry_name: &PathComponent,
    ) -> Result<Option<XfsFileEntry>, ErrorTrace> {
        if self.is_directory() && !self.sub_directory_entries.is_read() {
            match self.read_sub_directory_entries() {
                Ok(_) => {}
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to read sub directory entries"
                    );
                    return Err(error);
                }
            }
        }
        match self
            .sub_directory_entries
            .get_entry_by_name(sub_file_entry_name)
        {
            Ok(Some((name, directory_entry))) => {
                let file_entry: XfsFileEntry =
                    match self.create_sub_file_entry(name.clone(), directory_entry) {
                        Ok(file_entry) => file_entry,
                        Err(mut error) => {
                            keramics_core::error_trace_add_frame!(
                                error,
                                "Unable to create sub file entry"
                            );
                            return Err(error);
                        }
                    };
                Ok(Some(file_entry))
            }
            Ok(None) => Ok(None),
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to retrieve sub file entry");
                Err(error)
            }
        }
    }

    /// Retrieves a sub file entries iterator.
    pub fn sub_file_entries(&mut self) -> XfsFileEntriesIterator<'_> {
        XfsFileEntriesIterator::new(self)
    }

    /// Determines if the file entry is a directory.
    pub fn is_directory(&self) -> bool {
        self.inode.is_directory()
    }

    /// Determines if the file entry is the root directory.
    pub fn is_root_directory(&self) -> bool {
        self.inode_number == self.root_inode_number
    }

    /// Determines if the file entry is a symbolic link.
    pub fn is_symbolic_link(&self) -> bool {
        self.inode.is_symbolic_link()
    }

    /// Creates a sub file entry.
    fn create_sub_file_entry(
        &self,
        name: ByteString,
        directory_entry: &XfsDirectoryEntry,
    ) -> Result<XfsFileEntry, ErrorTrace> {
        let inode: XfsInode = match self
            .inode_table
            .get_inode(&self.data_stream, directory_entry.inode_number)
        {
            Ok(inode) => inode,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    format!("Unable to retrieve inode: {}", directory_entry.inode_number)
                );
                return Err(error);
            }
        };
        Ok(XfsFileEntry::new(
            &self.data_stream,
            &self.inode_table,
            directory_entry.inode_number,
            self.root_inode_number,
            inode,
            Some(name),
            self.directory_block_size,
            self.has_file_types,
            XfsDirectoryEntries::new(&self.sub_directory_entries.encoding),
        ))
    }

    /// Retrieves the block stream for file data.
    fn get_block_stream(&self) -> Result<XfsBlockStream, ErrorTrace> {
        let number_of_blocks: u64 = self
            .inode
            .data_size
            .div_ceil(self.inode_table.block_size as u64);
        let normalized_block_ranges: Vec<XfsBlockRange> = match normalize_sparse_block_ranges(
            self.block_ranges.clone(),
            self.inode_table.block_size as u64,
            self.inode.data_size,
        ) {
            Ok(block_ranges) => block_ranges,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to normalize block ranges");
                return Err(error);
            }
        };
        let mut block_stream: XfsBlockStream =
            XfsBlockStream::new(self.inode_table.block_size, self.inode.data_size);

        match block_stream.open(
            &self.data_stream,
            number_of_blocks,
            &normalized_block_ranges,
        ) {
            Ok(_) => {}
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to open block stream");
                return Err(error);
            }
        }
        Ok(block_stream)
    }

    /// Reads the block ranges.
    fn read_block_ranges(&mut self) -> Result<(), ErrorTrace> {
        if !self.block_ranges.is_empty() || self.inode.data_fork_format == XFS_INODE_FORMAT_LOCAL {
            return Ok(());
        }
        let number_of_extents: usize = match usize::try_from(self.inode.number_of_extents) {
            Ok(value) => value,
            Err(_) => {
                return Err(keramics_core::error_trace_new!(
                    "Number of extents value out of bounds"
                ));
            }
        };
        let mut block_ranges: Vec<XfsBlockRange> = match self.inode.data_fork_format {
            XFS_INODE_FORMAT_EXTENTS => {
                parse_block_ranges(&self.inode.data_fork, number_of_extents)?
            }
            XFS_INODE_FORMAT_BTREE => self.read_block_ranges_from_btree()?,
            _ => {
                return Err(keramics_core::error_trace_new!(format!(
                    "Unsupported data fork format: {}",
                    self.inode.data_fork_format
                )));
            }
        };

        for block_range in block_ranges.iter_mut() {
            if block_range.range_type == XfsBlockRangeType::Sparse
                || block_range.number_of_blocks == 0
            {
                continue;
            }
            block_range.physical_block_number = match self
                .inode_table
                .fs_block_number_to_absolute_block_number(block_range.physical_block_number)
            {
                Ok(block_number) => block_number,
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to convert filesystem block number to absolute block number"
                    );
                    return Err(error);
                }
            };
        }
        block_ranges.sort_by_key(|block_range| block_range.logical_block_number);
        self.block_ranges = block_ranges;

        Ok(())
    }

    /// Reads block ranges from an extent btree.
    fn read_block_ranges_from_btree(&self) -> Result<Vec<XfsBlockRange>, ErrorTrace> {
        if self.inode.data_fork.len() < 4 {
            return Err(keramics_core::error_trace_new!(
                "Unsupported extent btree root data size"
            ));
        }
        let level: u16 = bytes_to_u16_be!(self.inode.data_fork, 0);
        let number_of_records: usize = bytes_to_u16_be!(self.inode.data_fork, 2) as usize;
        let records: &[u8] = &self.inode.data_fork[4..];
        let mut block_ranges: Vec<XfsBlockRange> = Vec::new();

        if level == 0 {
            return parse_block_ranges(records, number_of_records);
        }
        let number_of_key_value_pairs: usize = records.len() / 16;

        if number_of_records > number_of_key_value_pairs {
            return Err(keramics_core::error_trace_new!(
                "Extent btree root record count value out of bounds"
            ));
        }
        let pointer_base_offset: usize = number_of_key_value_pairs * 8;

        for record_index in 0..number_of_records {
            let pointer_data: &[u8] =
                get_data_slice(records, pointer_base_offset + record_index * 8, 8)?;
            let child_block_number: u64 = bytes_to_u64_be!(pointer_data, 0);

            self.read_block_ranges_from_btree_node(child_block_number, 1, &mut block_ranges)?;
        }
        Ok(block_ranges)
    }

    /// Reads block ranges from an extent btree node.
    fn read_block_ranges_from_btree_node(
        &self,
        block_number: u64,
        recursion_depth: usize,
        block_ranges: &mut Vec<XfsBlockRange>,
    ) -> Result<(), ErrorTrace> {
        if recursion_depth > 256 {
            return Err(keramics_core::error_trace_new!(
                "Extent btree recursion depth value out of bounds"
            ));
        }
        let absolute_block_number: u64 = match self
            .inode_table
            .fs_block_number_to_absolute_block_number(block_number)
        {
            Ok(block_number) => block_number,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    "Unable to convert filesystem block number to absolute block number"
                );
                return Err(error);
            }
        };
        let block_offset: u64 =
            match absolute_block_number.checked_mul(self.inode_table.block_size as u64) {
                Some(value) => value,
                None => {
                    return Err(keramics_core::error_trace_new!(
                        "Extent btree block offset value out of bounds"
                    ));
                }
            };
        let data: Vec<u8> = match read_data_at_offset(
            &self.data_stream,
            block_offset,
            self.inode_table.block_size as usize,
        ) {
            Ok(data) => data,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    "Unable to read extent btree block data"
                );
                return Err(error);
            }
        };
        let expected_signature: &[u8; 4] = if self.inode_table.format_version == 5 {
            XFS_BTREE_BLOCK_SIGNATURE_V5
        } else {
            XFS_BTREE_BLOCK_SIGNATURE_V4
        };

        if &data[0..4] != expected_signature {
            return Err(keramics_core::error_trace_new!(
                "Unsupported extent btree signature"
            ));
        }
        let level: u16 = bytes_to_u16_be!(data, 4);
        let number_of_records: usize = bytes_to_u16_be!(data, 6) as usize;
        let header_size: usize = if self.inode_table.format_version == 5 {
            56
        } else {
            24
        };
        let records: &[u8] = get_data_slice(&data, header_size, data.len() - header_size)?;

        if level == 0 {
            block_ranges.extend(parse_block_ranges(records, number_of_records)?);
            return Ok(());
        }
        let number_of_key_value_pairs: usize = records.len() / 16;

        if number_of_records > number_of_key_value_pairs {
            return Err(keramics_core::error_trace_new!(
                "Extent btree branch record count value out of bounds"
            ));
        }
        let pointer_base_offset: usize = number_of_key_value_pairs * 8;

        for record_index in 0..number_of_records {
            let pointer_data: &[u8] =
                get_data_slice(records, pointer_base_offset + record_index * 8, 8)?;
            let child_block_number: u64 = bytes_to_u64_be!(pointer_data, 0);

            self.read_block_ranges_from_btree_node(
                child_block_number,
                recursion_depth + 1,
                block_ranges,
            )?;
        }
        Ok(())
    }

    /// Reads the sub directory entries.
    fn read_sub_directory_entries(&mut self) -> Result<(), ErrorTrace> {
        if !self.is_directory() {
            return Err(keramics_core::error_trace_new!(
                "Unsupported non-directory inode"
            ));
        }
        match self.inode.data_fork_format {
            XFS_INODE_FORMAT_LOCAL => {
                let inline_data: &[u8] = match self.inode.inline_data.as_ref() {
                    Some(data) => data,
                    None => {
                        return Err(keramics_core::error_trace_new!(
                            "Missing inline directory data"
                        ));
                    }
                };
                match self
                    .sub_directory_entries
                    .read_inline_data(inline_data, self.has_file_types)
                {
                    Ok(_) => {}
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            "Unable to read directory entries from inline data"
                        );
                        return Err(error);
                    }
                }
            }
            XFS_INODE_FORMAT_EXTENTS | XFS_INODE_FORMAT_BTREE => {
                self.read_block_ranges()?;

                match self.sub_directory_entries.read_block_data(
                    &self.data_stream,
                    self.inode_table.block_size,
                    self.directory_block_size,
                    &self.block_ranges,
                    self.has_file_types,
                ) {
                    Ok(_) => {}
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            "Unable to read directory entries from block data"
                        );
                        return Err(error);
                    }
                }
            }
            _ => {
                return Err(keramics_core::error_trace_new!(format!(
                    "Unsupported directory data fork format: {}",
                    self.inode.data_fork_format
                )));
            }
        }
        Ok(())
    }
}
