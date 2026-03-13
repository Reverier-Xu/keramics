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

use std::sync::Arc;

use keramics_core::{DataStreamReference, ErrorTrace};
use keramics_encodings::CharacterEncoding;
use keramics_types::ByteString;

use crate::path::Path;

use super::directory_entries::XfsDirectoryEntries;
use super::file_entry::XfsFileEntry;
use super::inode::XfsInode;
use super::inode_table::XfsInodeTable;
use super::superblock::XfsSuperblock;
use super::util::read_data_at_offset;

/// XFS file system.
pub struct XfsFileSystem {
    /// The data stream.
    data_stream: Option<DataStreamReference>,

    /// Character encoding.
    character_encoding: CharacterEncoding,

    /// Format version.
    format_version: u8,

    /// Block size.
    pub block_size: u32,

    /// Sector size.
    pub sector_size: u16,

    /// Inode size.
    pub inode_size: u16,

    /// Root inode number.
    pub root_inode_number: u64,

    /// Directory block size.
    directory_block_size: u32,

    /// Value to indicate if directory entries contain file types.
    has_file_types: bool,

    /// Inode table helper.
    inode_table: Arc<XfsInodeTable>,

    /// Volume label.
    volume_label: Option<ByteString>,
}

impl XfsFileSystem {
    /// Creates a new file system.
    pub fn new() -> Self {
        Self {
            data_stream: None,
            character_encoding: CharacterEncoding::Utf8,
            format_version: 0,
            block_size: 0,
            sector_size: 0,
            inode_size: 0,
            root_inode_number: 0,
            directory_block_size: 0,
            has_file_types: false,
            inode_table: Arc::new(XfsInodeTable::new()),
            volume_label: None,
        }
    }

    /// Retrieves the format version.
    pub fn get_format_version(&self) -> u8 {
        self.format_version
    }

    /// Retrieves the volume label.
    pub fn get_volume_label(&self) -> Option<&ByteString> {
        self.volume_label.as_ref()
    }

    /// Retrieves the file entry for a specific identifier (inode number).
    pub fn get_file_entry_by_identifier(
        &self,
        inode_number: u64,
    ) -> Result<XfsFileEntry, ErrorTrace> {
        let data_stream: &DataStreamReference = match self.data_stream.as_ref() {
            Some(data_stream) => data_stream,
            None => {
                return Err(keramics_core::error_trace_new!("Missing data stream"));
            }
        };
        let inode: XfsInode = match self.inode_table.get_inode(data_stream, inode_number) {
            Ok(inode) => inode,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    format!("Unable to retrieve inode: {}", inode_number)
                );
                return Err(error);
            }
        };
        Ok(XfsFileEntry::new(
            data_stream,
            &self.inode_table,
            inode_number,
            self.root_inode_number,
            inode,
            None,
            self.directory_block_size,
            self.has_file_types,
            XfsDirectoryEntries::new(&self.character_encoding),
        ))
    }

    /// Retrieves the file entry for a specific path.
    pub fn get_file_entry_by_path(&self, path: &Path) -> Result<Option<XfsFileEntry>, ErrorTrace> {
        self.get_file_entry_by_path_with_depth(path, false, 0)
    }

    /// Retrieves the root directory (file entry).
    pub fn get_root_directory(&self) -> Result<XfsFileEntry, ErrorTrace> {
        match self.get_file_entry_by_identifier(self.root_inode_number) {
            Ok(file_entry) => Ok(file_entry),
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(
                    error,
                    format!("Unable to retrieve file entry: {}", self.root_inode_number)
                );
                Err(error)
            }
        }
    }

    /// Reads a file system from a data stream.
    pub fn read_data_stream(
        &mut self,
        data_stream: &DataStreamReference,
    ) -> Result<(), ErrorTrace> {
        match self.read_metadata(data_stream) {
            Ok(_) => {}
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to read metadata");
                return Err(error);
            }
        }
        self.data_stream = Some(data_stream.clone());

        Ok(())
    }

    /// Sets the character encoding.
    pub fn set_character_encoding(
        &mut self,
        character_encoding: &CharacterEncoding,
    ) -> Result<(), ErrorTrace> {
        self.character_encoding = character_encoding.clone();

        Ok(())
    }

    /// Retrieves the file entry for a specific path with symlink depth tracking.
    fn get_file_entry_by_path_with_depth(
        &self,
        path: &Path,
        follow_final_symbolic_link: bool,
        recursion_depth: usize,
    ) -> Result<Option<XfsFileEntry>, ErrorTrace> {
        if path.is_empty() || path.is_relative() {
            return Ok(None);
        }
        if recursion_depth > 64 {
            return Err(keramics_core::error_trace_new!(
                "Symbolic link resolution depth value out of bounds"
            ));
        }
        let mut file_entry: XfsFileEntry = match self.get_root_directory() {
            Ok(file_entry) => file_entry,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to retrieve root directory");
                return Err(error);
            }
        };
        let mut current_path: Path = Path::from("/");
        let last_component_index: usize = path.components.len().saturating_sub(1);

        for (path_component_index, path_component) in path.components[1..].iter().enumerate() {
            let component_index: usize = path_component_index + 1;
            let mut sub_file_entry: XfsFileEntry =
                match file_entry.get_sub_file_entry_by_name(path_component) {
                    Ok(Some(file_entry)) => file_entry,
                    Ok(None) => return Ok(None),
                    Err(mut error) => {
                        keramics_core::error_trace_add_frame!(
                            error,
                            format!("Unable to retrieve sub file entry: {}", path_component)
                        );
                        return Err(error);
                    }
                };
            let is_final_component: bool = component_index == last_component_index;

            if sub_file_entry.is_symbolic_link()
                && (!is_final_component || follow_final_symbolic_link)
            {
                let symbolic_link_target: ByteString =
                    match sub_file_entry.get_symbolic_link_target() {
                        Ok(Some(symbolic_link_target)) => symbolic_link_target.clone(),
                        Ok(None) => {
                            return Err(keramics_core::error_trace_new!(
                                "Missing symbolic link target"
                            ));
                        }
                        Err(mut error) => {
                            keramics_core::error_trace_add_frame!(
                                error,
                                "Unable to retrieve symbolic link target"
                            );
                            return Err(error);
                        }
                    };
                let symbolic_link_target_path: Path = Path::from(&symbolic_link_target);
                let remaining_path: Path = if component_index < last_component_index {
                    Path::from(&path.components[component_index + 1..])
                } else {
                    Path::from("")
                };
                let rewritten_path: Path = if symbolic_link_target_path.is_relative() {
                    current_path.new_with_join(&symbolic_link_target_path)
                } else {
                    symbolic_link_target_path
                };
                let rewritten_path: Path = rewritten_path.new_with_join(&remaining_path);

                return self.get_file_entry_by_path_with_depth(
                    &rewritten_path,
                    follow_final_symbolic_link,
                    recursion_depth + 1,
                );
            }
            current_path = current_path.new_with_join_path_components(&[path_component.clone()]);
            file_entry = sub_file_entry;
        }
        Ok(Some(file_entry))
    }

    /// Reads the superblock and initializes the inode table.
    fn read_metadata(&mut self, data_stream: &DataStreamReference) -> Result<(), ErrorTrace> {
        let data: Vec<u8> = match read_data_at_offset(data_stream, 0, 512) {
            Ok(data) => data,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to read superblock data");
                return Err(error);
            }
        };
        let mut superblock: XfsSuperblock = XfsSuperblock::new(&self.character_encoding);

        match superblock.read_data(&data) {
            Ok(_) => {}
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to read superblock");
                return Err(error);
            }
        }
        self.format_version = superblock.format_version;
        self.block_size = superblock.block_size;
        self.sector_size = superblock.sector_size;
        self.inode_size = superblock.inode_size;
        self.root_inode_number = superblock.root_inode_number;
        self.directory_block_size = superblock.directory_block_size;
        self.has_file_types = superblock.has_file_types();
        let volume_label: ByteString = superblock.volume_label.clone();

        match Arc::get_mut(&mut self.inode_table) {
            Some(inode_table) => match inode_table.initialize(&superblock) {
                Ok(_) => {}
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        "Unable to initialize inode table helper"
                    );
                    return Err(error);
                }
            },
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Unable to obtain mutable reference to inode table helper"
                ));
            }
        }
        if !volume_label.is_empty() {
            self.volume_label = Some(volume_label);
        }
        Ok(())
    }
}
