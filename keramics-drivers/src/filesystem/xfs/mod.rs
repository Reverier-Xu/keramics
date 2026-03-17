/* Copyright 2024-2026 Joachim Metz <joachim.metz@gmail.com>
 * Copyright 2026 Reverier Xu <reverier.xu@woooo.tech>
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
use std::collections::BTreeMap;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{
    DataSource, DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget,
    MemoryDataSource,
};

const XFS_SUPERBLOCK_SIGNATURE: &[u8; 4] = b"XFSB";
const XFS_INODE_SIGNATURE: &[u8; 2] = b"IN";

const XFS_INODE_FORMAT_LOCAL: u8 = 1;
const XFS_INODE_FORMAT_EXTENTS: u8 = 2;
const XFS_INODE_FORMAT_BTREE: u8 = 3;

const XFS_FILE_MODE_TYPE_MASK: u16 = 0xf000;
const XFS_FILE_MODE_TYPE_DIRECTORY: u16 = 0x4000;
const XFS_FILE_MODE_TYPE_REGULAR_FILE: u16 = 0x8000;
const XFS_FILE_MODE_TYPE_SYMBOLIC_LINK: u16 = 0xa000;

const XFS_SUPERBLOCK_FEATURE2_FILE_TYPE: u32 = 0x0000_0200;
const XFS_SUPERBLOCK_INCOMPATIBLE_FEATURE_FILE_TYPE: u32 = 0x0000_0001;

const XFS_DIRECTORY_LEAF_OFFSET: u64 = 0x0000_0008_0000_0000;

const XFS_INODE_BTREE_SIGNATURE_V4: &[u8; 4] = b"IABT";
const XFS_INODE_BTREE_SIGNATURE_V5: &[u8; 4] = b"IAB3";

const XFS_INODES_PER_CHUNK: u64 = 64;
const XFS_MAX_INODE_NUMBER: u64 = (1u64 << 56) - 1;

#[derive(Clone)]
struct XfsRuntime {
    source: DataSourceReference,
    inode_table: XfsInodeTable,
    root_inode_number: u64,
    directory_block_size: u32,
    has_file_types: bool,
    format_version: u8,
    volume_label: Option<String>,
}

#[derive(Clone)]
struct XfsSuperblock {
    block_size: u32,
    sector_size: u16,
    inode_size: u16,
    inodes_per_block_log2: u8,
    allocation_group_block_size: u32,
    number_of_allocation_groups: u32,
    root_inode_number: u64,
    format_version: u8,
    secondary_feature_flags: u32,
    incompatible_feature_flags: u32,
    directory_block_size: u32,
    relative_block_bits: u8,
    relative_inode_bits: u8,
    volume_label: Option<String>,
}

#[derive(Clone)]
struct XfsInodeTable {
    block_size: u32,
    sector_size: u16,
    inode_size: u16,
    inodes_per_block_log2: u8,
    allocation_group_block_size: u32,
    number_of_allocation_groups: u32,
    relative_block_bits: u8,
    relative_inode_bits: u8,
}

#[derive(Clone)]
struct XfsInode {
    file_mode: u16,
    data_fork_format: u8,
    data_size: u64,
    number_of_extents: u64,
    data_fork: Vec<u8>,
    inline_data: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct XfsDirectoryEntry {
    inode_number: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum XfsBlockRangeType {
    InFile,
    Sparse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct XfsBlockRange {
    logical_block_number: u64,
    physical_block_number: u64,
    number_of_blocks: u64,
    range_type: XfsBlockRangeType,
}

/// Immutable XFS file entry.
#[derive(Clone)]
pub struct XfsFileEntry {
    runtime: Arc<XfsRuntime>,
    inode_number: u64,
    inode: XfsInode,
    name: Option<String>,
}

/// Immutable XFS file system.
pub struct XfsFileSystem {
    runtime: Arc<XfsRuntime>,
}

impl XfsFileSystem {
    /// Opens and parses an XFS file system.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let superblock = XfsSuperblock::read_at(source.as_ref())?;
        let inode_table = XfsInodeTable::from_superblock(&superblock);
        let runtime = Arc::new(XfsRuntime {
            source,
            inode_table,
            root_inode_number: superblock.root_inode_number,
            directory_block_size: superblock.directory_block_size,
            has_file_types: superblock.has_file_types(),
            format_version: superblock.format_version,
            volume_label: superblock.volume_label.clone(),
        });

        Ok(Self { runtime })
    }

    /// Retrieves the format version.
    pub fn format_version(&self) -> u8 {
        self.runtime.format_version
    }

    /// Retrieves the volume label.
    pub fn volume_label(&self) -> Option<&str> {
        self.runtime.volume_label.as_deref()
    }

    /// Retrieves the root directory.
    pub fn root_directory(&self) -> Result<XfsFileEntry, ErrorTrace> {
        self.file_entry_by_identifier(self.runtime.root_inode_number)
    }

    /// Retrieves a file entry by inode number.
    pub fn file_entry_by_identifier(&self, inode_number: u64) -> Result<XfsFileEntry, ErrorTrace> {
        let inode = self
            .runtime
            .inode_table
            .get_inode(self.runtime.source.as_ref(), inode_number)?;

        Ok(XfsFileEntry {
            runtime: self.runtime.clone(),
            inode_number,
            inode,
            name: None,
        })
    }

    /// Retrieves a file entry by absolute path.
    pub fn file_entry_by_path(&self, path: &str) -> Result<Option<XfsFileEntry>, ErrorTrace> {
        if path.is_empty() || !path.starts_with('/') {
            return Ok(None);
        }

        let path_components = split_absolute_path(path);
        self.file_entry_by_components(&path_components, false, 0)
    }

    fn file_entry_by_components(
        &self,
        path_components: &[String],
        follow_final_symbolic_link: bool,
        recursion_depth: usize,
    ) -> Result<Option<XfsFileEntry>, ErrorTrace> {
        if recursion_depth > 64 {
            return Err(ErrorTrace::new(
                "Symbolic link resolution depth value out of bounds".to_string(),
            ));
        }

        let mut file_entry = self.root_directory()?;

        if path_components.is_empty() {
            return Ok(Some(file_entry));
        }

        for (component_index, path_component) in path_components.iter().enumerate() {
            let is_final_component = component_index + 1 == path_components.len();
            let sub_file_entry = match file_entry.sub_file_entry_by_name(path_component)? {
                Some(file_entry) => file_entry,
                None => return Ok(None),
            };

            if sub_file_entry.is_symbolic_link()
                && (!is_final_component || follow_final_symbolic_link)
            {
                let target = sub_file_entry
                    .symbolic_link_target()?
                    .ok_or_else(|| ErrorTrace::new("Missing symbolic link target".to_string()))?;
                let remaining_components = if is_final_component {
                    Vec::new()
                } else {
                    path_components[component_index + 1..].to_vec()
                };
                let rewritten_path_components = rewrite_path_components(
                    &path_components[..component_index],
                    target.as_str(),
                    &remaining_components,
                );

                return self.file_entry_by_components(
                    &rewritten_path_components,
                    follow_final_symbolic_link,
                    recursion_depth + 1,
                );
            }

            file_entry = sub_file_entry;
        }

        Ok(Some(file_entry))
    }
}

impl XfsFileEntry {
    /// Retrieves the inode number.
    pub fn inode_number(&self) -> u64 {
        self.inode_number
    }

    /// Retrieves the name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Retrieves the size.
    pub fn size(&self) -> u64 {
        if self.inode.is_regular_file() || self.inode.is_symbolic_link() {
            self.inode.data_size
        } else {
            0
        }
    }

    /// Determines if the file entry is a directory.
    pub fn is_directory(&self) -> bool {
        self.inode.is_directory()
    }

    /// Determines if the file entry is a symbolic link.
    pub fn is_symbolic_link(&self) -> bool {
        self.inode.is_symbolic_link()
    }

    /// Retrieves the symbolic link target if present.
    pub fn symbolic_link_target(&self) -> Result<Option<String>, ErrorTrace> {
        if !self.is_symbolic_link() {
            return Ok(None);
        }

        let data = if let Some(inline_data) = self.inode.inline_data.as_ref() {
            inline_data.clone()
        } else {
            self.build_data_source()?.read_all()?
        };

        Ok(Some(String::from_utf8_lossy(&data).into_owned()))
    }

    /// Retrieves the number of sub file entries.
    pub fn number_of_sub_file_entries(&self) -> Result<usize, ErrorTrace> {
        Ok(self.read_directory_entries()?.len())
    }

    /// Retrieves a specific sub file entry by name.
    pub fn sub_file_entry_by_name(&self, name: &str) -> Result<Option<XfsFileEntry>, ErrorTrace> {
        let entries = self.read_directory_entries()?;
        let directory_entry = match entries.get(name) {
            Some(directory_entry) => directory_entry,
            None => return Ok(None),
        };
        let inode = self
            .runtime
            .inode_table
            .get_inode(self.runtime.source.as_ref(), directory_entry.inode_number)?;

        Ok(Some(Self {
            runtime: self.runtime.clone(),
            inode_number: directory_entry.inode_number,
            inode,
            name: Some(name.to_string()),
        }))
    }

    /// Opens the default data source of the file entry.
    pub fn open_source(&self) -> Result<Option<DataSourceReference>, ErrorTrace> {
        if !self.inode.is_regular_file() {
            return Ok(None);
        }

        Ok(Some(self.build_data_source()?))
    }

    fn build_data_source(&self) -> Result<DataSourceReference, ErrorTrace> {
        if let Some(inline_data) = self.inode.inline_data.as_ref() {
            return Ok(Arc::new(MemoryDataSource::new(inline_data.clone())));
        }

        let block_ranges = self.read_block_ranges()?;
        let normalized_block_ranges = normalize_sparse_block_ranges(
            block_ranges,
            self.runtime.inode_table.block_size as u64,
            self.inode.data_size,
        )?;
        let mut extents = Vec::new();

        for block_range in normalized_block_ranges {
            let logical_offset = block_range
                .logical_block_number
                .checked_mul(self.runtime.inode_table.block_size as u64)
                .ok_or_else(|| ErrorTrace::new("XFS logical extent offset overflow".to_string()))?;
            let size = min(
                block_range.number_of_blocks * self.runtime.inode_table.block_size as u64,
                self.inode.data_size.saturating_sub(logical_offset),
            );

            extents.push(ExtentMapEntry {
                logical_offset,
                size,
                target: match block_range.range_type {
                    XfsBlockRangeType::InFile => {
                        let absolute_block_number = self
                            .runtime
                            .inode_table
                            .fs_block_number_to_absolute_block_number(
                                block_range.physical_block_number,
                            )?;

                        ExtentMapTarget::Data {
                            source: self.runtime.source.clone(),
                            source_offset: absolute_block_number
                                .checked_mul(self.runtime.inode_table.block_size as u64)
                                .ok_or_else(|| {
                                    ErrorTrace::new(
                                        "XFS physical extent offset overflow".to_string(),
                                    )
                                })?,
                        }
                    }
                    XfsBlockRangeType::Sparse => ExtentMapTarget::Zero,
                },
            });
        }

        Ok(Arc::new(ExtentMapDataSource::new(extents)?))
    }

    fn read_directory_entries(&self) -> Result<BTreeMap<String, XfsDirectoryEntry>, ErrorTrace> {
        if !self.is_directory() {
            return Ok(BTreeMap::new());
        }

        if let Some(inline_data) = self.inode.inline_data.as_ref() {
            return read_shortform_entries(inline_data, self.runtime.has_file_types);
        }

        let block_ranges = self.read_block_ranges()?;
        read_block_directory_entries(
            self.runtime.source.as_ref(),
            self.runtime.inode_table.block_size,
            self.runtime.directory_block_size,
            &block_ranges,
            self.runtime.has_file_types,
        )
    }

    fn read_block_ranges(&self) -> Result<Vec<XfsBlockRange>, ErrorTrace> {
        if self.inode.data_fork_format == XFS_INODE_FORMAT_LOCAL {
            return Ok(Vec::new());
        }

        let number_of_extents = usize::try_from(self.inode.number_of_extents)
            .map_err(|_| ErrorTrace::new("Number of extents value out of bounds".to_string()))?;

        match self.inode.data_fork_format {
            XFS_INODE_FORMAT_EXTENTS => {
                parse_block_ranges(&self.inode.data_fork, number_of_extents)
            }
            XFS_INODE_FORMAT_BTREE => Err(ErrorTrace::new(
                "XFS btree data fork format is not supported yet in keramics-drivers".to_string(),
            )),
            value => Err(ErrorTrace::new(format!(
                "Unsupported XFS data fork format: {}",
                value,
            ))),
        }
    }
}

impl XfsSuperblock {
    fn read_at(source: &dyn DataSource) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(0, &mut data)?;

        if &data[0..4] != XFS_SUPERBLOCK_SIGNATURE {
            return Err(ErrorTrace::new("Unsupported XFS signature".to_string()));
        }

        let block_size = read_u32_be(&data, 4)?;
        let root_inode_number = read_u64_be(&data, 56)? & XFS_MAX_INODE_NUMBER;
        let allocation_group_block_size = read_u32_be(&data, 84)?;
        let number_of_allocation_groups = read_u32_be(&data, 88)?;
        let version_flags = read_u16_be(&data, 100)?;
        let format_version = (version_flags & 0x000f) as u8;
        let sector_size = read_u16_be(&data, 102)?;
        let inode_size = read_u16_be(&data, 104)?;
        let inodes_per_block = read_u16_be(&data, 106)?;
        let volume_label = decode_string(&data[108..120]);
        let inodes_per_block_log2 = data[123];
        let relative_block_bits = data[124];
        let directory_block_log2 = data[192];
        let secondary_feature_flags = read_u32_be(&data, 200)?;
        let incompatible_feature_flags = read_u32_be(&data, 216)?;

        if !(4..=5).contains(&format_version) {
            return Err(ErrorTrace::new(format!(
                "Unsupported XFS format version: {}",
                format_version,
            )));
        }
        if !(512..=32768).contains(&sector_size) {
            return Err(ErrorTrace::new(format!(
                "Unsupported XFS sector size: {}",
                sector_size,
            )));
        }
        if !(512..=65536).contains(&block_size) {
            return Err(ErrorTrace::new(format!(
                "Unsupported XFS block size: {}",
                block_size,
            )));
        }
        if !(256..=2048).contains(&inode_size) {
            return Err(ErrorTrace::new(format!(
                "Unsupported XFS inode size: {}",
                inode_size,
            )));
        }
        if allocation_group_block_size < 5 || number_of_allocation_groups == 0 {
            return Err(ErrorTrace::new(
                "Invalid XFS allocation group geometry".to_string(),
            ));
        }
        if relative_block_bits == 0 || relative_block_bits > 31 {
            return Err(ErrorTrace::new(
                "Invalid XFS allocation group size log2".to_string(),
            ));
        }
        if inodes_per_block_log2 == 0 || inodes_per_block_log2 > (32 - relative_block_bits) {
            return Err(ErrorTrace::new(
                "Invalid XFS inodes per block log2 value".to_string(),
            ));
        }
        if (1u64 << inodes_per_block_log2) != inodes_per_block as u64 {
            return Err(ErrorTrace::new(
                "Mismatch between XFS number of inodes per block and log2 values".to_string(),
            ));
        }

        let directory_block_size = if directory_block_log2 == 0 {
            block_size
        } else {
            let multiplier = 1u32
                .checked_shl(directory_block_log2 as u32)
                .ok_or_else(|| {
                    ErrorTrace::new("Invalid XFS directory block size log2".to_string())
                })?;

            block_size
                .checked_mul(multiplier)
                .ok_or_else(|| ErrorTrace::new("XFS directory block size overflow".to_string()))?
        };
        let relative_inode_bits = relative_block_bits
            .checked_add(inodes_per_block_log2)
            .ok_or_else(|| ErrorTrace::new("XFS relative inode bits overflow".to_string()))?;

        if relative_inode_bits == 0 || relative_inode_bits >= 32 {
            return Err(ErrorTrace::new(
                "Invalid XFS relative inode bits value".to_string(),
            ));
        }

        Ok(Self {
            block_size,
            sector_size,
            inode_size,
            inodes_per_block_log2,
            allocation_group_block_size,
            number_of_allocation_groups,
            root_inode_number,
            format_version,
            secondary_feature_flags,
            incompatible_feature_flags,
            directory_block_size,
            relative_block_bits,
            relative_inode_bits,
            volume_label: if volume_label.is_empty() {
                None
            } else {
                Some(volume_label)
            },
        })
    }

    fn has_file_types(&self) -> bool {
        self.format_version == 5
            || self.secondary_feature_flags & XFS_SUPERBLOCK_FEATURE2_FILE_TYPE != 0
            || self.incompatible_feature_flags & XFS_SUPERBLOCK_INCOMPATIBLE_FEATURE_FILE_TYPE != 0
    }
}

impl XfsInodeTable {
    fn from_superblock(superblock: &XfsSuperblock) -> Self {
        Self {
            block_size: superblock.block_size,
            sector_size: superblock.sector_size,
            inode_size: superblock.inode_size,
            inodes_per_block_log2: superblock.inodes_per_block_log2,
            allocation_group_block_size: superblock.allocation_group_block_size,
            number_of_allocation_groups: superblock.number_of_allocation_groups,
            relative_block_bits: superblock.relative_block_bits,
            relative_inode_bits: superblock.relative_inode_bits,
        }
    }

    fn get_inode(
        &self,
        source: &dyn DataSource,
        inode_number: u64,
    ) -> Result<XfsInode, ErrorTrace> {
        let inode_offset = self.get_inode_offset(source, inode_number)?;
        let data = read_data_at_offset(source, inode_offset, self.inode_size as usize)?;

        XfsInode::read_data(&data)
    }

    fn fs_block_number_to_absolute_block_number(
        &self,
        fs_block_number: u64,
    ) -> Result<u64, ErrorTrace> {
        let allocation_group_number = fs_block_number >> self.relative_block_bits;

        if allocation_group_number >= self.number_of_allocation_groups as u64 {
            return Err(ErrorTrace::new(format!(
                "XFS filesystem block allocation group number: {} value out of bounds",
                allocation_group_number,
            )));
        }

        let allocation_group_block_mask = (1u64 << self.relative_block_bits) - 1;
        let allocation_group_block_number = fs_block_number & allocation_group_block_mask;

        if allocation_group_block_number >= self.allocation_group_block_size as u64 {
            return Err(ErrorTrace::new(format!(
                "XFS filesystem relative block number: {} value out of bounds",
                allocation_group_block_number,
            )));
        }

        allocation_group_number
            .checked_mul(self.allocation_group_block_size as u64)
            .and_then(|value| value.checked_add(allocation_group_block_number))
            .ok_or_else(|| {
                ErrorTrace::new("XFS absolute block number value out of bounds".to_string())
            })
    }

    fn get_inode_offset(
        &self,
        source: &dyn DataSource,
        inode_number: u64,
    ) -> Result<u64, ErrorTrace> {
        let inode_number = inode_number & XFS_MAX_INODE_NUMBER;
        let maximum_inode_number = ((self.number_of_allocation_groups as u64)
            << self.relative_inode_bits)
            .saturating_sub(1);

        if inode_number == 0 || inode_number > maximum_inode_number {
            return Err(ErrorTrace::new(format!(
                "Invalid XFS inode number: {} value out of bounds",
                inode_number,
            )));
        }

        let allocation_group_number = inode_number >> self.relative_inode_bits;
        let allocation_group_inode_mask = (1u64 << self.relative_inode_bits) - 1;
        let allocation_group_inode_number = inode_number & allocation_group_inode_mask;
        let allocation_group_block_number =
            allocation_group_inode_number >> self.inodes_per_block_log2;

        if allocation_group_block_number >= self.allocation_group_block_size as u64 {
            return Err(ErrorTrace::new(format!(
                "Invalid XFS allocation group block number: {} value out of bounds",
                allocation_group_block_number,
            )));
        }
        if !self.inode_chunk_exists(
            source,
            allocation_group_number,
            allocation_group_inode_number,
        )? {
            return Err(ErrorTrace::new(format!(
                "Missing XFS inode chunk for inode: {}",
                inode_number,
            )));
        }

        let inode_index =
            allocation_group_inode_number & ((1u64 << self.inodes_per_block_log2) - 1);
        let file_system_block_number = allocation_group_number
            .checked_mul(self.allocation_group_block_size as u64)
            .and_then(|value| value.checked_add(allocation_group_block_number))
            .ok_or_else(|| {
                ErrorTrace::new("Invalid XFS inode block number value out of bounds".to_string())
            })?;

        file_system_block_number
            .checked_mul(self.block_size as u64)
            .and_then(|value| value.checked_add(inode_index * (self.inode_size as u64)))
            .ok_or_else(|| {
                ErrorTrace::new("Invalid XFS inode offset value out of bounds".to_string())
            })
    }

    fn inode_chunk_exists(
        &self,
        source: &dyn DataSource,
        allocation_group_number: u64,
        allocation_group_inode_number: u64,
    ) -> Result<bool, ErrorTrace> {
        let (root_block_number, number_of_levels) =
            self.read_allocation_group_inode_header(source, allocation_group_number)?;

        if root_block_number == 0 || number_of_levels == 0 {
            return Ok(false);
        }

        self.search_inode_btree_for_aginode(
            source,
            allocation_group_number,
            root_block_number as u64,
            allocation_group_inode_number,
            0,
        )
    }

    fn read_allocation_group_inode_header(
        &self,
        source: &dyn DataSource,
        allocation_group_number: u64,
    ) -> Result<(u32, u32), ErrorTrace> {
        let allocation_group_offset = allocation_group_number
            .checked_mul(self.allocation_group_block_size as u64)
            .and_then(|value| value.checked_mul(self.block_size as u64))
            .ok_or_else(|| {
                ErrorTrace::new("XFS allocation group offset value out of bounds".to_string())
            })?;
        let agi_offset = allocation_group_offset
            .checked_add((self.sector_size as u64) * 2)
            .ok_or_else(|| {
                ErrorTrace::new("XFS allocation group inode offset value out of bounds".to_string())
            })?;
        let data = read_data_at_offset(source, agi_offset, self.sector_size as usize)?;

        if &data[0..4] != b"XAGI" {
            return Err(ErrorTrace::new("Unsupported XFS AGI signature".to_string()));
        }

        Ok((read_u32_be(&data, 20)?, read_u32_be(&data, 24)?))
    }

    fn search_inode_btree_for_aginode(
        &self,
        source: &dyn DataSource,
        allocation_group_number: u64,
        allocation_group_block_number: u64,
        allocation_group_inode_number: u64,
        recursion_depth: usize,
    ) -> Result<bool, ErrorTrace> {
        if recursion_depth > 128 {
            return Err(ErrorTrace::new(
                "XFS inode btree recursion depth value out of bounds".to_string(),
            ));
        }

        let file_system_block_number = allocation_group_number
            .checked_mul(self.allocation_group_block_size as u64)
            .and_then(|value| value.checked_add(allocation_group_block_number))
            .ok_or_else(|| {
                ErrorTrace::new("XFS inode btree block number out of bounds".to_string())
            })?;
        let block_offset = file_system_block_number
            .checked_mul(self.block_size as u64)
            .ok_or_else(|| {
                ErrorTrace::new("XFS inode btree block offset out of bounds".to_string())
            })?;
        let data = read_data_at_offset(source, block_offset, self.block_size as usize)?;
        let header_size = if &data[0..4] == XFS_INODE_BTREE_SIGNATURE_V5 {
            56
        } else if &data[0..4] == XFS_INODE_BTREE_SIGNATURE_V4 {
            16
        } else {
            return Err(ErrorTrace::new(
                "Unsupported XFS inode btree signature".to_string(),
            ));
        };
        let level = read_u16_be(&data, 4)?;
        let number_of_records = read_u16_be(&data, 6)? as usize;

        if level == 0 {
            for record_index in 0..number_of_records {
                let record_data = get_data_slice(&data, header_size + record_index * 16, 16)?;
                let first_inode_number = read_u32_be(record_data, 0)? as u64;

                if allocation_group_inode_number >= first_inode_number
                    && allocation_group_inode_number < first_inode_number + XFS_INODES_PER_CHUNK
                {
                    return Ok(true);
                }
            }

            return Ok(false);
        }

        let records_data_size = data.len() - header_size;
        let number_of_key_value_pairs = records_data_size / 8;

        if number_of_records > number_of_key_value_pairs {
            return Err(ErrorTrace::new(
                "XFS inode btree record count value out of bounds".to_string(),
            ));
        }

        let mut record_index: usize = 0;

        for key_index in 0..number_of_records {
            let key_data = get_data_slice(&data, header_size + key_index * 4, 4)?;
            let key_inode_number = read_u32_be(key_data, 0)? as u64;

            if allocation_group_inode_number < key_inode_number {
                break;
            }
            record_index += 1;
        }

        if record_index == 0 {
            return Ok(false);
        }

        let pointer_offset = header_size + (number_of_key_value_pairs + record_index - 1) * 4;
        let pointer_data = get_data_slice(&data, pointer_offset, 4)?;
        let child_block_number = read_u32_be(pointer_data, 0)? as u64;

        if child_block_number >= self.allocation_group_block_size as u64 {
            return Err(ErrorTrace::new(format!(
                "XFS inode btree child block number: {} value out of bounds",
                child_block_number,
            )));
        }

        self.search_inode_btree_for_aginode(
            source,
            allocation_group_number,
            child_block_number,
            allocation_group_inode_number,
            recursion_depth + 1,
        )
    }
}

impl XfsInode {
    fn read_data(data: &[u8]) -> Result<Self, ErrorTrace> {
        if data.len() < 100 {
            return Err(ErrorTrace::new(
                "Unsupported XFS inode data size".to_string(),
            ));
        }
        if &data[0..2] != XFS_INODE_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported XFS inode signature".to_string(),
            ));
        }

        let file_mode = read_u16_be(data, 2)?;
        let format_version = data[4];
        let data_fork_format = data[5];

        if !matches!(format_version, 1..=3) {
            return Err(ErrorTrace::new(format!(
                "Unsupported XFS inode format version: {}",
                format_version,
            )));
        }

        let data_size = read_u64_be(data, 56)?;
        let number_of_extents = read_u32_be(data, 76)? as u64;
        let attribute_fork_offset = (data[82] as usize) * 8;
        let core_size = if format_version == 3 { 176 } else { 100 };

        if data.len() < core_size {
            return Err(ErrorTrace::new(
                "Unsupported XFS inode core size".to_string(),
            ));
        }

        let mut data_fork_size = data.len() - core_size;
        if attribute_fork_offset > 0 {
            if attribute_fork_offset > data_fork_size {
                return Err(ErrorTrace::new(
                    "Invalid XFS attribute fork offset value out of bounds".to_string(),
                ));
            }
            data_fork_size = attribute_fork_offset;
        }

        let data_fork = data[core_size..core_size + data_fork_size].to_vec();
        let inline_data = if data_fork_format == XFS_INODE_FORMAT_LOCAL {
            if data_size as usize > data_fork.len() {
                return Err(ErrorTrace::new(
                    "XFS inline data size value out of bounds".to_string(),
                ));
            }
            Some(data_fork[..min(data_fork.len(), data_size as usize)].to_vec())
        } else {
            None
        };

        Ok(Self {
            file_mode,
            data_fork_format,
            data_size,
            number_of_extents,
            data_fork,
            inline_data,
        })
    }

    fn is_directory(&self) -> bool {
        self.file_mode & XFS_FILE_MODE_TYPE_MASK == XFS_FILE_MODE_TYPE_DIRECTORY
    }

    fn is_regular_file(&self) -> bool {
        self.file_mode & XFS_FILE_MODE_TYPE_MASK == XFS_FILE_MODE_TYPE_REGULAR_FILE
    }

    fn is_symbolic_link(&self) -> bool {
        self.file_mode & XFS_FILE_MODE_TYPE_MASK == XFS_FILE_MODE_TYPE_SYMBOLIC_LINK
    }
}

fn parse_block_ranges(
    data: &[u8],
    number_of_records: usize,
) -> Result<Vec<XfsBlockRange>, ErrorTrace> {
    let mut block_ranges = Vec::with_capacity(number_of_records);

    for record_index in 0..number_of_records {
        let record_data = get_data_slice(data, record_index * 16, 16)?;
        block_ranges.push(parse_block_range(record_data));
    }

    Ok(block_ranges)
}

fn parse_block_range(data: &[u8]) -> XfsBlockRange {
    let mut upper = u64::from_be_bytes(data[0..8].try_into().unwrap());
    let mut lower = u64::from_be_bytes(data[8..16].try_into().unwrap());

    let number_of_blocks = lower & 0x001f_ffff;
    lower >>= 21;

    let physical_block_number = lower | (upper & 0x01ff);
    upper >>= 9;

    let logical_block_number = upper & 0x003f_ffff_ffff_ffff;
    upper >>= 54;

    XfsBlockRange {
        logical_block_number,
        physical_block_number,
        number_of_blocks,
        range_type: if upper != 0 {
            XfsBlockRangeType::Sparse
        } else {
            XfsBlockRangeType::InFile
        },
    }
}

fn normalize_sparse_block_ranges(
    mut block_ranges: Vec<XfsBlockRange>,
    block_size: u64,
    data_size: u64,
) -> Result<Vec<XfsBlockRange>, ErrorTrace> {
    block_ranges.sort_by_key(|block_range| block_range.logical_block_number);

    let mut normalized_block_ranges = Vec::new();
    let mut logical_block_number: u64 = 0;
    let mut number_of_blocks = data_size / block_size;

    if !data_size.is_multiple_of(block_size) {
        number_of_blocks += 1;
    }

    for block_range in block_ranges {
        if block_range.number_of_blocks == 0 {
            continue;
        }
        if block_range.logical_block_number < logical_block_number {
            return Err(ErrorTrace::new(
                "Overlapping XFS block ranges are unsupported".to_string(),
            ));
        }
        if block_range.logical_block_number > logical_block_number {
            normalized_block_ranges.push(XfsBlockRange {
                logical_block_number,
                physical_block_number: 0,
                number_of_blocks: block_range.logical_block_number - logical_block_number,
                range_type: XfsBlockRangeType::Sparse,
            });
        }
        logical_block_number = block_range
            .logical_block_number
            .checked_add(block_range.number_of_blocks)
            .ok_or_else(|| {
                ErrorTrace::new("XFS block range logical offset overflow".to_string())
            })?;
        normalized_block_ranges.push(block_range);
    }

    if logical_block_number < number_of_blocks {
        normalized_block_ranges.push(XfsBlockRange {
            logical_block_number,
            physical_block_number: 0,
            number_of_blocks: number_of_blocks - logical_block_number,
            range_type: XfsBlockRangeType::Sparse,
        });
    }

    Ok(normalized_block_ranges)
}

fn read_shortform_entries(
    data: &[u8],
    has_file_types: bool,
) -> Result<BTreeMap<String, XfsDirectoryEntry>, ErrorTrace> {
    if data.len() < 2 {
        return Err(ErrorTrace::new(
            "Unsupported XFS shortform directory data size".to_string(),
        ));
    }

    let number_of_entries_32bit = data[0] as usize;
    let number_of_entries_64bit = data[1] as usize;

    if number_of_entries_32bit != 0 && number_of_entries_64bit != 0 {
        return Err(ErrorTrace::new(
            "Unsupported XFS shortform directory entry counters".to_string(),
        ));
    }

    let (number_of_entries, inode_size, mut data_offset): (usize, usize, usize) =
        if number_of_entries_64bit == 0 {
            (number_of_entries_32bit, 4, 6)
        } else {
            (number_of_entries_64bit, 8, 10)
        };
    let mut entries = BTreeMap::new();

    for _ in 0..number_of_entries {
        if data_offset >= data.len() {
            return Err(ErrorTrace::new(
                "XFS shortform directory entry data offset value out of bounds".to_string(),
            ));
        }

        let name_size = data[data_offset] as usize;
        let mut entry_size = 3usize.checked_add(name_size).ok_or_else(|| {
            ErrorTrace::new("XFS shortform directory entry size value out of bounds".to_string())
        })?;
        if has_file_types {
            entry_size = entry_size.checked_add(1).ok_or_else(|| {
                ErrorTrace::new(
                    "XFS shortform directory entry size value out of bounds".to_string(),
                )
            })?;
        }
        entry_size = entry_size.checked_add(inode_size).ok_or_else(|| {
            ErrorTrace::new("XFS shortform directory entry size value out of bounds".to_string())
        })?;

        let _ = get_data_slice(data, data_offset, entry_size)?;
        data_offset += 1;
        data_offset += 2;

        let name = decode_string(get_data_slice(data, data_offset, name_size)?);
        data_offset += name_size;

        if has_file_types {
            data_offset += 1;
        }

        let inode_number = if inode_size == 4 {
            read_u32_be(data, data_offset)? as u64
        } else {
            read_u64_be(data, data_offset)?
        } & XFS_MAX_INODE_NUMBER;
        data_offset += inode_size;

        if name != "." && name != ".." {
            entries.insert(name, XfsDirectoryEntry { inode_number });
        }
    }

    Ok(entries)
}

fn read_block_directory_entries(
    source: &dyn DataSource,
    block_size: u32,
    directory_block_size: u32,
    block_ranges: &[XfsBlockRange],
    has_file_types: bool,
) -> Result<BTreeMap<String, XfsDirectoryEntry>, ErrorTrace> {
    let mut entries = BTreeMap::new();

    for block_range in block_ranges {
        if block_range.range_type == XfsBlockRangeType::Sparse
            || block_range.number_of_blocks == 0
            || block_range.logical_block_number >= XFS_DIRECTORY_LEAF_OFFSET
        {
            continue;
        }

        let range_physical_offset = block_range
            .physical_block_number
            .checked_mul(block_size as u64)
            .ok_or_else(|| ErrorTrace::new("XFS range physical offset overflow".to_string()))?;
        let range_size = block_range
            .number_of_blocks
            .checked_mul(block_size as u64)
            .ok_or_else(|| ErrorTrace::new("XFS range size overflow".to_string()))?;
        let range_logical_offset = block_range
            .logical_block_number
            .checked_mul(block_size as u64)
            .ok_or_else(|| ErrorTrace::new("XFS range logical offset overflow".to_string()))?;
        let mut range_offset: u64 = 0;

        while range_offset + directory_block_size as u64 <= range_size {
            let logical_offset = range_logical_offset + range_offset;
            if logical_offset >= XFS_DIRECTORY_LEAF_OFFSET {
                break;
            }

            let data = read_data_at_offset(
                source,
                range_physical_offset + range_offset,
                directory_block_size as usize,
            )?;
            read_block_entries(&data, has_file_types, &mut entries)?;
            range_offset += directory_block_size as u64;
        }
    }

    Ok(entries)
}

fn read_block_entries(
    data: &[u8],
    has_file_types: bool,
    entries: &mut BTreeMap<String, XfsDirectoryEntry>,
) -> Result<(), ErrorTrace> {
    let signature = get_data_slice(data, 0, 4)?;
    let (header_size, has_footer) = match signature {
        b"XD2B" => (16usize, true),
        b"XD2D" => (16usize, false),
        b"XDB3" => (64usize, true),
        b"XDD3" => (64usize, false),
        b"XD2L" | b"XD2N" | b"XD2F" | b"XDL3" | b"XDN3" | b"XDF3" => return Ok(()),
        _ => {
            return Err(ErrorTrace::new(format!(
                "Unsupported XFS directory block signature: {:02x}{:02x}{:02x}{:02x}",
                data[0], data[1], data[2], data[3],
            )));
        }
    };
    let entries_end_offset = if has_footer {
        let footer_data = get_data_slice(data, data.len() - 8, 8)?;
        let number_of_entries = read_u32_be(footer_data, 0)? as usize;
        let hash_data_size = number_of_entries.checked_mul(8).ok_or_else(|| {
            ErrorTrace::new("XFS directory hash table size value out of bounds".to_string())
        })?;

        data.len().checked_sub(8 + hash_data_size).ok_or_else(|| {
            ErrorTrace::new("Invalid XFS directory hash table data size".to_string())
        })?
    } else {
        data.len()
    };
    let mut data_offset = header_size;

    while data_offset < entries_end_offset {
        let header_data = get_data_slice(data, data_offset, 4)?;
        if read_u16_be(header_data, 0)? == 0xffff {
            let size = read_u16_be(header_data, 2)? as usize;
            if size < 4 {
                return Err(ErrorTrace::new(
                    "Invalid XFS free directory region size".to_string(),
                ));
            }
            data_offset += size;
            continue;
        }

        let inode_number = read_u64_be(data, data_offset)? & XFS_MAX_INODE_NUMBER;
        let name_size = data[data_offset + 8] as usize;
        let mut entry_size = 9usize
            .checked_add(name_size)
            .and_then(|value| value.checked_add(2))
            .ok_or_else(|| {
                ErrorTrace::new("XFS directory entry size value out of bounds".to_string())
            })?;
        if has_file_types {
            entry_size += 1;
        }
        let remainder_size = entry_size % 8;
        if remainder_size != 0 {
            entry_size += 8 - remainder_size;
        }

        let name = decode_string(get_data_slice(data, data_offset + 9, name_size)?);

        if name != "." && name != ".." {
            entries.insert(name, XfsDirectoryEntry { inode_number });
        }
        data_offset += entry_size;
    }

    Ok(())
}

fn split_absolute_path(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|component| !component.is_empty())
        .map(|component| component.to_string())
        .collect()
}

fn rewrite_path_components(
    current_components: &[String],
    symbolic_link_target: &str,
    remaining_components: &[String],
) -> Vec<String> {
    let mut components = if symbolic_link_target.starts_with('/') {
        split_absolute_path(symbolic_link_target)
    } else {
        let mut components = current_components.to_vec();

        for component in symbolic_link_target.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            if component == ".." {
                components.pop();
            } else {
                components.push(component.to_string());
            }
        }
        components
    };

    components.extend_from_slice(remaining_components);
    components
}

fn get_data_slice(data: &[u8], data_offset: usize, data_size: usize) -> Result<&[u8], ErrorTrace> {
    let data_end_offset = data_offset.checked_add(data_size).ok_or_else(|| {
        ErrorTrace::new("Invalid XFS data offset value out of bounds".to_string())
    })?;

    data.get(data_offset..data_end_offset).ok_or_else(|| {
        ErrorTrace::new(format!(
            "Invalid XFS data offset: {} value out of bounds",
            data_offset
        ))
    })
}

fn read_data_at_offset(
    source: &dyn DataSource,
    offset: u64,
    data_size: usize,
) -> Result<Vec<u8>, ErrorTrace> {
    let mut data = vec![0; data_size];

    source.read_exact_at(offset, &mut data)?;
    Ok(data)
}

fn decode_string(data: &[u8]) -> String {
    let end_offset = data
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.len());

    String::from_utf8_lossy(&data[..end_offset]).into_owned()
}

fn read_u16_be(data: &[u8], offset: usize) -> Result<u16, ErrorTrace> {
    Ok(u16::from_be_bytes(
        get_data_slice(data, offset, 2)?.try_into().unwrap(),
    ))
}

fn read_u32_be(data: &[u8], offset: usize) -> Result<u32, ErrorTrace> {
    Ok(u32::from_be_bytes(
        get_data_slice(data, offset, 4)?.try_into().unwrap(),
    ))
}

fn read_u64_be(data: &[u8], offset: usize) -> Result<u64, ErrorTrace> {
    Ok(u64::from_be_bytes(
        get_data_slice(data, offset, 8)?.try_into().unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XFS_FILE_MODE_TYPE_DIRECTORY_TEST: u16 = 0x4000;
    const XFS_FILE_MODE_TYPE_REGULAR_FILE_TEST: u16 = 0x8000;
    const XFS_FILE_MODE_TYPE_SYMBOLIC_LINK_TEST: u16 = 0xa000;
    const XFS_INODE_FORMAT_LOCAL_TEST: u8 = 1;
    const XFS_INODE_FORMAT_EXTENTS_TEST: u8 = 2;
    const XFS_SUPERBLOCK_FEATURE2_FILE_TYPE_TEST: u32 = 0x0000_0200;

    fn build_superblock(root_inode_number: u64) -> Vec<u8> {
        build_superblock_with_geometry(root_inode_number, 8, 1, 3)
    }

    fn build_superblock_with_geometry(
        root_inode_number: u64,
        allocation_group_block_size: u32,
        number_of_allocation_groups: u32,
        relative_block_bits: u8,
    ) -> Vec<u8> {
        let mut data = vec![0; 512];

        data[0..4].copy_from_slice(XFS_SUPERBLOCK_SIGNATURE);
        data[4..8].copy_from_slice(&512u32.to_be_bytes());
        data[56..64].copy_from_slice(&root_inode_number.to_be_bytes());
        data[84..88].copy_from_slice(&allocation_group_block_size.to_be_bytes());
        data[88..92].copy_from_slice(&number_of_allocation_groups.to_be_bytes());
        data[100..102].copy_from_slice(&5u16.to_be_bytes());
        data[102..104].copy_from_slice(&512u16.to_be_bytes());
        data[104..106].copy_from_slice(&256u16.to_be_bytes());
        data[106..108].copy_from_slice(&2u16.to_be_bytes());
        data[108..116].copy_from_slice(b"xfs_test");
        data[123] = 1;
        data[124] = relative_block_bits;
        data[192] = 0;
        data[200..204].copy_from_slice(&XFS_SUPERBLOCK_FEATURE2_FILE_TYPE_TEST.to_be_bytes());

        data
    }

    fn put_agi_and_inobt_leaf(image: &mut [u8], root_block_number: u32, first_inode_number: u32) {
        let agi = &mut image[1024..1536];
        agi[0..4].copy_from_slice(b"XAGI");
        agi[4..8].copy_from_slice(&1u32.to_be_bytes());
        agi[12..16].copy_from_slice(&8u32.to_be_bytes());
        agi[20..24].copy_from_slice(&root_block_number.to_be_bytes());
        agi[24..28].copy_from_slice(&1u32.to_be_bytes());

        let root_start_offset = (root_block_number as usize) * 512;
        let root_end_offset = root_start_offset + 512;
        let root = &mut image[root_start_offset..root_end_offset];
        root[0..4].copy_from_slice(XFS_INODE_BTREE_SIGNATURE_V5);
        root[4..6].copy_from_slice(&0u16.to_be_bytes());
        root[6..8].copy_from_slice(&1u16.to_be_bytes());
        root[56..60].copy_from_slice(&first_inode_number.to_be_bytes());
    }

    fn put_inode(
        image: &mut [u8],
        inode_offset: usize,
        file_mode: u16,
        data_fork_format: u8,
        data_size: u64,
        number_of_extents: u32,
        data_fork: &[u8],
    ) {
        let inode = &mut image[inode_offset..inode_offset + 256];

        inode[0..2].copy_from_slice(XFS_INODE_SIGNATURE);
        inode[2..4].copy_from_slice(&file_mode.to_be_bytes());
        inode[4] = 3;
        inode[5] = data_fork_format;
        inode[8..12].copy_from_slice(&1000u32.to_be_bytes());
        inode[12..16].copy_from_slice(&1000u32.to_be_bytes());
        inode[16..20].copy_from_slice(&1u32.to_be_bytes());
        inode[56..64].copy_from_slice(&data_size.to_be_bytes());
        inode[76..80].copy_from_slice(&number_of_extents.to_be_bytes());
        inode[82] = 0;

        let copy_size = min(data_fork.len(), 256 - 176);
        inode[176..176 + copy_size].copy_from_slice(&data_fork[..copy_size]);
    }

    fn encode_extent(
        logical_block_number: u64,
        physical_block_number: u64,
        number_of_blocks: u64,
    ) -> [u8; 16] {
        let upper = (logical_block_number << 9) | (physical_block_number & 0x01ff);
        let lower = ((physical_block_number >> 9) << 21) | (number_of_blocks & 0x001f_ffff);
        let mut data = [0u8; 16];

        data[0..8].copy_from_slice(&upper.to_be_bytes());
        data[8..16].copy_from_slice(&lower.to_be_bytes());
        data
    }

    fn build_shortform_directory(entries: &[(&str, u32)]) -> Vec<u8> {
        let mut directory_data = Vec::new();

        directory_data.push(entries.len() as u8);
        directory_data.push(0u8);
        directory_data.extend_from_slice(&2u32.to_be_bytes());

        for (name, inode_number) in entries {
            directory_data.push(name.len() as u8);
            directory_data.extend_from_slice(&0u16.to_be_bytes());
            directory_data.extend_from_slice(name.as_bytes());
            directory_data.push(1u8);
            directory_data.extend_from_slice(&inode_number.to_be_bytes());
        }

        directory_data
    }

    fn open_file_system(image: &[u8]) -> Result<XfsFileSystem, ErrorTrace> {
        XfsFileSystem::open(Arc::new(MemoryDataSource::new(image.to_vec())))
    }

    #[test]
    fn read_xfs_inline_file_and_shortform_directory() -> Result<(), ErrorTrace> {
        let mut image = vec![0; 4096];
        let superblock = build_superblock(2);
        image[0..512].copy_from_slice(&superblock);
        put_agi_and_inobt_leaf(&mut image, 4, 0);

        let directory_data = build_shortform_directory(&[("hello.txt", 3)]);
        put_inode(
            &mut image,
            512,
            XFS_FILE_MODE_TYPE_DIRECTORY_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            directory_data.len() as u64,
            0,
            &directory_data,
        );
        put_inode(
            &mut image,
            768,
            XFS_FILE_MODE_TYPE_REGULAR_FILE_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            3,
            0,
            b"abc",
        );

        let file_system = open_file_system(&image)?;

        assert_eq!(file_system.format_version(), 5);
        assert_eq!(file_system.volume_label(), Some("xfs_test"));

        let root_directory = file_system.root_directory()?;
        assert_eq!(root_directory.inode_number(), 2);
        assert_eq!(root_directory.number_of_sub_file_entries()?, 1);

        let file_entry = file_system.file_entry_by_path("/hello.txt")?.unwrap();
        assert_eq!(file_entry.size(), 3);
        assert_eq!(file_entry.name(), Some("hello.txt"));

        let source = file_entry.open_source()?.unwrap();
        assert_eq!(source.read_all()?, b"abc");

        Ok(())
    }

    #[test]
    fn read_xfs_extent_file_and_block_directory() -> Result<(), ErrorTrace> {
        let mut image = vec![0; 4096];
        let superblock = build_superblock(2);
        image[0..512].copy_from_slice(&superblock);
        put_agi_and_inobt_leaf(&mut image, 4, 0);

        let directory_extent = encode_extent(0, 5, 1);
        put_inode(
            &mut image,
            512,
            XFS_FILE_MODE_TYPE_DIRECTORY_TEST,
            XFS_INODE_FORMAT_EXTENTS_TEST,
            0,
            1,
            &directory_extent,
        );

        let file_extent = encode_extent(0, 3, 1);
        put_inode(
            &mut image,
            768,
            XFS_FILE_MODE_TYPE_REGULAR_FILE_TEST,
            XFS_INODE_FORMAT_EXTENTS_TEST,
            3,
            1,
            &file_extent,
        );

        let directory_block = &mut image[2560..3072];
        directory_block[0..4].copy_from_slice(b"XD2D");
        let mut data_offset: usize = 16;
        directory_block[data_offset..data_offset + 8].copy_from_slice(&3u64.to_be_bytes());
        directory_block[data_offset + 8] = 4;
        directory_block[data_offset + 9..data_offset + 13].copy_from_slice(b"file");
        directory_block[data_offset + 13] = 1;
        directory_block[data_offset + 14..data_offset + 16].copy_from_slice(&0u16.to_be_bytes());
        data_offset += 16;
        directory_block[data_offset..data_offset + 2].copy_from_slice(&0xffffu16.to_be_bytes());
        directory_block[data_offset + 2..data_offset + 4]
            .copy_from_slice(&((512 - data_offset) as u16).to_be_bytes());

        image[1536..1539].copy_from_slice(b"XYZ");

        let file_system = open_file_system(&image)?;
        let file_entry = file_system.file_entry_by_path("/file")?.unwrap();
        let source = file_entry.open_source()?.unwrap();

        assert_eq!(source.read_all()?, b"XYZ");
        Ok(())
    }

    #[test]
    fn read_xfs_symbolic_link_directory_path() -> Result<(), ErrorTrace> {
        let mut image = vec![0; 8192];
        let superblock = build_superblock(2);
        image[0..512].copy_from_slice(&superblock);
        put_agi_and_inobt_leaf(&mut image, 4, 0);

        let root_directory_data = build_shortform_directory(&[("linkdir", 3), ("real", 10)]);
        put_inode(
            &mut image,
            512,
            XFS_FILE_MODE_TYPE_DIRECTORY_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            root_directory_data.len() as u64,
            0,
            &root_directory_data,
        );
        put_inode(
            &mut image,
            768,
            XFS_FILE_MODE_TYPE_SYMBOLIC_LINK_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            4,
            0,
            b"real",
        );

        let real_directory_data = build_shortform_directory(&[("x", 11)]);
        put_inode(
            &mut image,
            2560,
            XFS_FILE_MODE_TYPE_DIRECTORY_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            real_directory_data.len() as u64,
            0,
            &real_directory_data,
        );
        put_inode(
            &mut image,
            2816,
            XFS_FILE_MODE_TYPE_REGULAR_FILE_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            2,
            0,
            b"ok",
        );

        let file_system = open_file_system(&image)?;

        let symbolic_link_entry = file_system.file_entry_by_path("/linkdir")?.unwrap();
        assert!(symbolic_link_entry.is_symbolic_link());
        assert_eq!(
            symbolic_link_entry.symbolic_link_target()?,
            Some("real".to_string())
        );

        let file_entry = file_system.file_entry_by_path("/linkdir/x")?.unwrap();
        let source = file_entry.open_source()?.unwrap();

        assert_eq!(source.read_all()?, b"ok");

        Ok(())
    }

    #[test]
    fn read_xfs_extent_with_allocation_group_geometry() -> Result<(), ErrorTrace> {
        let mut image = vec![0; 16384];
        let superblock = build_superblock_with_geometry(2, 6, 2, 3);
        image[0..512].copy_from_slice(&superblock);
        put_agi_and_inobt_leaf(&mut image, 4, 0);

        let directory_data = build_shortform_directory(&[("file", 3)]);
        put_inode(
            &mut image,
            512,
            XFS_FILE_MODE_TYPE_DIRECTORY_TEST,
            XFS_INODE_FORMAT_LOCAL_TEST,
            directory_data.len() as u64,
            0,
            &directory_data,
        );

        let file_extent = encode_extent(0, 9, 1);
        put_inode(
            &mut image,
            768,
            XFS_FILE_MODE_TYPE_REGULAR_FILE_TEST,
            XFS_INODE_FORMAT_EXTENTS_TEST,
            4,
            1,
            &file_extent,
        );
        image[7 * 512..7 * 512 + 4].copy_from_slice(b"DATA");

        let file_system = open_file_system(&image)?;
        let file_entry = file_system.file_entry_by_path("/file")?.unwrap();
        let source = file_entry.open_source()?.unwrap();

        assert_eq!(source.read_all()?, b"DATA");

        Ok(())
    }
}
