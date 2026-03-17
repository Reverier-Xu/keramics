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

use std::cmp::{max, min};
use std::collections::BTreeMap;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{
    DataSource, DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget,
    MemoryDataSource,
};

const EXT_SUPERBLOCK_SIGNATURE: [u8; 2] = [0x53, 0xef];
const EXT_EXTENTS_HEADER_SIGNATURE: [u8; 2] = [0x0a, 0xf3];

const EXT_COMPATIBLE_FEATURE_FLAG_SPARSE_SUPERBLOCK2: u32 = 0x0000_0200;

const EXT_INCOMPATIBLE_FEATURE_FLAG_JOURNAL_DEVICE: u32 = 0x0000_0008;
const EXT_INCOMPATIBLE_FEATURE_FLAG_HAS_EXTENTS: u32 = 0x0000_0040;
const EXT_INCOMPATIBLE_FEATURE_FLAG_64BIT_SUPPORT: u32 = 0x0000_0080;
const EXT_INCOMPATIBLE_FEATURE_FLAG_HAS_FLEX_BLOCK_GROUPS: u32 = 0x0000_0200;
const EXT_INCOMPATIBLE_FEATURE_FLAG_HAS_METADATA_CHECKSUM_SEED: u32 = 0x0000_2000;

const EXT_INODE_FLAG_HAS_EXTENTS: u32 = 0x0008_0000;
const EXT_INODE_FLAG_IS_EXTENDED_ATTRIBUTE_INODE: u32 = 0x0020_0000;
const EXT_INODE_FLAG_INLINE_DATA: u32 = 0x1000_0000;

const EXT_FILE_MODE_TYPE_DIRECTORY: u16 = 0x4000;
const EXT_FILE_MODE_TYPE_REGULAR_FILE: u16 = 0x8000;
const EXT_FILE_MODE_TYPE_SYMBOLIC_LINK: u16 = 0xa000;

const EXT_ROOT_DIRECTORY_IDENTIFIER: u32 = 2;

#[derive(Clone)]
struct ExtRuntime {
    source: DataSourceReference,
    format_version: u8,
    compatible_feature_flags: u32,
    incompatible_feature_flags: u32,
    read_only_compatible_feature_flags: u32,
    volume_label: Option<String>,
    inode_table: ExtInodeTable,
}

#[derive(Clone)]
struct ExtSuperblock {
    number_of_inodes: u32,
    number_of_blocks: u64,
    block_size: u32,
    number_of_blocks_per_block_group: u32,
    number_of_inodes_per_block_group: u32,
    format_revision: u32,
    inode_size: u16,
    compatible_feature_flags: u32,
    incompatible_feature_flags: u32,
    read_only_compatible_feature_flags: u32,
    group_descriptor_size: u16,
    volume_label: Option<String>,
}

#[derive(Clone)]
struct ExtInodeTable {
    format_version: u8,
    block_size: u32,
    inode_size: u16,
    number_of_inodes_per_block_group: u32,
    group_descriptors: Vec<ExtGroupDescriptor>,
    number_of_inodes: u32,
}

#[derive(Clone)]
struct ExtGroupDescriptor {
    inode_table_block_number: u64,
}

#[derive(Clone)]
struct ExtInode {
    file_mode: u16,
    data_size: u64,
    number_of_blocks: u64,
    flags: u32,
    data_reference: [u8; 60],
}

#[derive(Clone)]
struct ExtDirectoryEntry {
    inode_number: u32,
    name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExtBlockRangeType {
    InFile,
    Sparse,
}

#[derive(Clone, Debug)]
struct ExtBlockRange {
    logical_block_number: u64,
    physical_block_number: u64,
    number_of_blocks: u64,
    range_type: ExtBlockRangeType,
}

#[derive(Clone, Copy)]
struct ExtExtentsHeader {
    number_of_entries: u16,
    depth: u16,
}

#[derive(Clone, Copy)]
struct ExtExtentDescriptor {
    logical_block_number: u32,
    number_of_blocks: u16,
    physical_block_number: u64,
}

#[derive(Clone, Copy)]
struct ExtExtentIndex {
    physical_block_number: u64,
}

struct ExtBlockNumbersContext<'source> {
    source: &'source dyn DataSource,
    block_size: u32,
    number_of_blocks_per_block: u32,
    number_of_blocks: u64,
}

/// Immutable ext file entry.
#[derive(Clone)]
pub struct ExtFileEntry {
    runtime: Arc<ExtRuntime>,
    inode_number: u32,
    inode: ExtInode,
    name: Option<String>,
}

/// Immutable ext file system.
pub struct ExtFileSystem {
    runtime: Arc<ExtRuntime>,
}

impl ExtFileSystem {
    /// Opens and parses an ext file system.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let superblock = ExtSuperblock::read_at(source.as_ref())?;
        let format_version = determine_format_version(
            superblock.compatible_feature_flags,
            superblock.incompatible_feature_flags,
            superblock.read_only_compatible_feature_flags,
        );

        if superblock.format_revision > 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported ext format revision: {}",
                superblock.format_revision,
            )));
        }
        if has_unsupported_features(superblock.incompatible_feature_flags) {
            return Err(ErrorTrace::new(
                "Unsupported ext file system features".to_string(),
            ));
        }

        let number_of_group_descriptors = superblock
            .number_of_blocks
            .div_ceil(superblock.number_of_blocks_per_block_group as u64);
        let group_descriptor_size = determine_group_descriptor_size(
            format_version,
            superblock.incompatible_feature_flags,
            superblock.group_descriptor_size,
        )?;
        let group_descriptor_offset = if superblock.block_size == 1024 {
            2048
        } else {
            superblock.block_size as u64
        };
        let group_descriptors = read_group_descriptors(
            source.as_ref(),
            format_version,
            group_descriptor_size,
            number_of_group_descriptors,
            group_descriptor_offset,
        )?;
        let inode_table = ExtInodeTable {
            format_version,
            block_size: superblock.block_size,
            inode_size: superblock.inode_size,
            number_of_inodes_per_block_group: superblock.number_of_inodes_per_block_group,
            group_descriptors,
            number_of_inodes: superblock.number_of_inodes,
        };
        let runtime = Arc::new(ExtRuntime {
            source,
            format_version,
            compatible_feature_flags: superblock.compatible_feature_flags,
            incompatible_feature_flags: superblock.incompatible_feature_flags,
            read_only_compatible_feature_flags: superblock.read_only_compatible_feature_flags,
            volume_label: superblock.volume_label,
            inode_table,
        });

        Ok(Self { runtime })
    }

    /// Retrieves the format version.
    pub fn format_version(&self) -> u8 {
        self.runtime.format_version
    }

    /// Retrieves the compatible feature flags.
    pub fn compatible_feature_flags(&self) -> u32 {
        self.runtime.compatible_feature_flags
    }

    /// Retrieves the incompatible feature flags.
    pub fn incompatible_feature_flags(&self) -> u32 {
        self.runtime.incompatible_feature_flags
    }

    /// Retrieves the read-only compatible feature flags.
    pub fn read_only_compatible_feature_flags(&self) -> u32 {
        self.runtime.read_only_compatible_feature_flags
    }

    /// Retrieves the volume label.
    pub fn volume_label(&self) -> Option<&str> {
        self.runtime.volume_label.as_deref()
    }

    /// Retrieves the root directory.
    pub fn root_directory(&self) -> Result<ExtFileEntry, ErrorTrace> {
        self.file_entry_by_identifier(EXT_ROOT_DIRECTORY_IDENTIFIER)
    }

    /// Retrieves a file entry by inode number.
    pub fn file_entry_by_identifier(&self, inode_number: u32) -> Result<ExtFileEntry, ErrorTrace> {
        let inode = self
            .runtime
            .inode_table
            .get_inode(self.runtime.source.as_ref(), inode_number)?;

        Ok(ExtFileEntry {
            runtime: self.runtime.clone(),
            inode_number,
            inode,
            name: None,
        })
    }

    /// Retrieves a file entry by absolute path.
    pub fn file_entry_by_path(&self, path: &str) -> Result<Option<ExtFileEntry>, ErrorTrace> {
        if path.is_empty() || !path.starts_with('/') {
            return Ok(None);
        }
        if path == "/" {
            return Ok(Some(self.root_directory()?));
        }

        let mut file_entry = self.root_directory()?;

        for path_component in path.split('/').filter(|component| !component.is_empty()) {
            file_entry = match file_entry.sub_file_entry_by_name(path_component)? {
                Some(file_entry) => file_entry,
                None => return Ok(None),
            };
        }

        Ok(Some(file_entry))
    }
}

impl ExtFileEntry {
    /// Retrieves the inode number.
    pub fn inode_number(&self) -> u32 {
        self.inode_number
    }

    /// Retrieves the name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Retrieves the size.
    pub fn size(&self) -> u64 {
        match self.inode.file_mode & 0xf000 {
            EXT_FILE_MODE_TYPE_REGULAR_FILE | EXT_FILE_MODE_TYPE_SYMBOLIC_LINK => {
                self.inode.data_size
            }
            _ => 0,
        }
    }

    /// Determines if the file entry is a directory.
    pub fn is_directory(&self) -> bool {
        self.inode.file_mode & 0xf000 == EXT_FILE_MODE_TYPE_DIRECTORY
    }

    /// Determines if the file entry is a symbolic link.
    pub fn is_symbolic_link(&self) -> bool {
        self.inode.file_mode & 0xf000 == EXT_FILE_MODE_TYPE_SYMBOLIC_LINK
    }

    /// Opens the default data source.
    pub fn open_source(&self) -> Result<Option<DataSourceReference>, ErrorTrace> {
        if self.inode.file_mode & 0xf000 != EXT_FILE_MODE_TYPE_REGULAR_FILE {
            return Ok(None);
        }

        Ok(Some(self.build_data_source()?))
    }

    fn sub_file_entry_by_name(&self, name: &str) -> Result<Option<ExtFileEntry>, ErrorTrace> {
        let directory_entries = self.read_directory_entries()?;
        let directory_entry = match directory_entries.get(name) {
            Some(directory_entry) => directory_entry,
            None => return Ok(None),
        };
        let inode = self
            .runtime
            .inode_table
            .get_inode(self.runtime.source.as_ref(), directory_entry.inode_number)?;

        Ok(Some(ExtFileEntry {
            runtime: self.runtime.clone(),
            inode_number: directory_entry.inode_number,
            inode,
            name: Some(directory_entry.name.clone()),
        }))
    }

    fn read_directory_entries(&self) -> Result<BTreeMap<String, ExtDirectoryEntry>, ErrorTrace> {
        if !self.is_directory() {
            return Ok(BTreeMap::new());
        }

        let data = if self.has_inline_data() {
            self.inode.inline_data()?
        } else {
            self.build_data_source()?.read_all()?
        };

        read_directory_entries_from_data(&data)
    }

    fn build_data_source(&self) -> Result<DataSourceReference, ErrorTrace> {
        if self.has_inline_data() {
            return Ok(Arc::new(MemoryDataSource::new(self.inode.inline_data()?)));
        }

        let number_of_blocks = max(
            self.inode
                .data_size
                .div_ceil(self.runtime.inode_table.block_size as u64),
            self.inode.number_of_blocks,
        );
        let block_ranges = if self.runtime.format_version == 4
            && self.inode.flags & EXT_INODE_FLAG_HAS_EXTENTS != 0
        {
            read_extents_block_ranges(
                &self.inode.data_reference,
                self.runtime.source.as_ref(),
                self.runtime.inode_table.block_size,
                number_of_blocks,
            )?
        } else {
            read_block_number_ranges(
                &self.inode.data_reference,
                self.runtime.source.as_ref(),
                self.runtime.inode_table.block_size,
                number_of_blocks,
            )?
        };
        let mut extents = Vec::new();
        let mut current_extent: Option<ExtentMapEntry> = None;

        for block_range in block_ranges {
            let logical_offset = block_range
                .logical_block_number
                .checked_mul(self.runtime.inode_table.block_size as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("ext logical block range offset overflow".to_string())
                })?;

            if logical_offset >= self.inode.data_size {
                break;
            }

            let size = min(
                block_range.number_of_blocks * self.runtime.inode_table.block_size as u64,
                self.inode.data_size - logical_offset,
            );
            let next_extent = ExtentMapEntry {
                logical_offset,
                size,
                target: match block_range.range_type {
                    ExtBlockRangeType::InFile => ExtentMapTarget::Data {
                        source: self.runtime.source.clone(),
                        source_offset: block_range
                            .physical_block_number
                            .checked_mul(self.runtime.inode_table.block_size as u64)
                            .ok_or_else(|| {
                                ErrorTrace::new(
                                    "ext physical block range offset overflow".to_string(),
                                )
                            })?,
                    },
                    ExtBlockRangeType::Sparse => ExtentMapTarget::Zero,
                },
            };

            current_extent = merge_extent(current_extent, next_extent, &mut extents);
        }

        if let Some(current_extent) = current_extent {
            extents.push(current_extent);
        }

        Ok(Arc::new(ExtentMapDataSource::new(extents)?))
    }

    fn has_inline_data(&self) -> bool {
        self.runtime.format_version == 4 && self.inode.flags & EXT_INODE_FLAG_INLINE_DATA != 0
    }
}

impl ExtSuperblock {
    fn read_at(source: &dyn DataSource) -> Result<Self, ErrorTrace> {
        let mut data = vec![0u8; 1024];

        source.read_exact_at(1024, &mut data)?;

        if data[56..58] != EXT_SUPERBLOCK_SIGNATURE {
            return Err(ErrorTrace::new("Unsupported ext signature".to_string()));
        }

        let block_size_shift = read_u32_le(&data, 24)?;
        if block_size_shift > 21 {
            return Err(ErrorTrace::new(format!(
                "Invalid ext block size shift: {} value out of bounds",
                block_size_shift,
            )));
        }

        let incompatible_feature_flags = read_u32_le(&data, 96)?;
        let mut number_of_blocks = read_u32_le(&data, 4)? as u64;
        if incompatible_feature_flags & EXT_INCOMPATIBLE_FEATURE_FLAG_64BIT_SUPPORT != 0 {
            number_of_blocks |= (read_u32_le(&data, 336)? as u64) << 32;
        }

        let block_size = 1024u32
            .checked_shl(block_size_shift)
            .ok_or_else(|| ErrorTrace::new("ext block size overflow".to_string()))?;
        let number_of_blocks_per_block_group = read_u32_le(&data, 32)?;
        let number_of_inodes_per_block_group = read_u32_le(&data, 40)?;
        let inode_size = read_u16_le(&data, 88)?;

        if number_of_blocks == 0 {
            return Err(ErrorTrace::new(
                "Invalid ext number of blocks value out of bounds".to_string(),
            ));
        }
        if number_of_blocks_per_block_group == 0 {
            return Err(ErrorTrace::new(
                "Invalid ext number of blocks per block group value out of bounds".to_string(),
            ));
        }
        if number_of_inodes_per_block_group == 0 {
            return Err(ErrorTrace::new(
                "Invalid ext number of inodes per block group value out of bounds".to_string(),
            ));
        }
        if !(128..=2048).contains(&inode_size) || inode_size & (inode_size - 1) != 0 {
            return Err(ErrorTrace::new(format!(
                "Invalid ext inode size: {} value out of bounds",
                inode_size,
            )));
        }

        Ok(Self {
            number_of_inodes: read_u32_le(&data, 0)?,
            number_of_blocks,
            block_size,
            number_of_blocks_per_block_group,
            number_of_inodes_per_block_group,
            format_revision: read_u32_le(&data, 76)?,
            inode_size,
            compatible_feature_flags: read_u32_le(&data, 92)?,
            incompatible_feature_flags,
            read_only_compatible_feature_flags: read_u32_le(&data, 100)?,
            group_descriptor_size: read_u16_le(&data, 254)?,
            volume_label: {
                let label = trim_nul_terminated(&data[120..136]);

                if label.is_empty() { None } else { Some(label) }
            },
        })
    }
}

impl ExtInodeTable {
    fn get_inode(
        &self,
        source: &dyn DataSource,
        inode_number: u32,
    ) -> Result<ExtInode, ErrorTrace> {
        if inode_number == 0 || inode_number > self.number_of_inodes {
            return Err(ErrorTrace::new(format!(
                "Invalid ext inode number: {} value out of bounds",
                inode_number,
            )));
        }

        let group_index =
            ((inode_number - 1) as usize) / (self.number_of_inodes_per_block_group as usize);
        let inode_group_index = (inode_number - 1) % self.number_of_inodes_per_block_group;
        let group_descriptor = self.group_descriptors.get(group_index).ok_or_else(|| {
            ErrorTrace::new(format!(
                "Missing ext group descriptor for inode number: {}",
                inode_number,
            ))
        })?;
        let inode_offset = group_descriptor
            .inode_table_block_number
            .checked_mul(self.block_size as u64)
            .and_then(|value| {
                value.checked_add((inode_group_index as u64) * (self.inode_size as u64))
            })
            .ok_or_else(|| ErrorTrace::new("ext inode offset overflow".to_string()))?;
        let mut data = vec![0u8; self.inode_size as usize];

        source.read_exact_at(inode_offset, &mut data)?;

        ExtInode::read_data(self.format_version, &data)
    }
}

impl ExtInode {
    fn read_data(format_version: u8, data: &[u8]) -> Result<Self, ErrorTrace> {
        if data.len() < 128 {
            return Err(ErrorTrace::new("Unsupported ext inode size".to_string()));
        }

        let flags = read_u32_le(data, 32)?;
        if flags & EXT_INODE_FLAG_IS_EXTENDED_ATTRIBUTE_INODE != 0 {
            return Err(ErrorTrace::new(
                "Extended attribute inodes are not supported in keramics-drivers ext support"
                    .to_string(),
            ));
        }

        let mut data_reference = [0u8; 60];
        data_reference.copy_from_slice(&data[40..100]);

        let data_size = if format_version == 4 {
            ((read_u32_le(data, 108)? as u64) << 32) | (read_u32_le(data, 4)? as u64)
        } else {
            read_u32_le(data, 4)? as u64
        };
        let number_of_blocks = if format_version == 4 {
            ((read_u16_le(data, 116)? as u64) << 32) | (read_u32_le(data, 28)? as u64)
        } else {
            read_u32_le(data, 28)? as u64
        };

        Ok(Self {
            file_mode: read_u16_le(data, 0)?,
            data_size,
            number_of_blocks,
            flags,
            data_reference,
        })
    }

    fn inline_data(&self) -> Result<Vec<u8>, ErrorTrace> {
        if self.data_size > self.data_reference.len() as u64 {
            return Err(ErrorTrace::new(format!(
                "Unsupported ext inline data size: {} value out of bounds",
                self.data_size,
            )));
        }

        Ok(self.data_reference[..self.data_size as usize].to_vec())
    }
}

fn determine_format_version(
    compatible_feature_flags: u32,
    incompatible_feature_flags: u32,
    read_only_compatible_feature_flags: u32,
) -> u8 {
    if compatible_feature_flags & EXT_COMPATIBLE_FEATURE_FLAG_SPARSE_SUPERBLOCK2 != 0
        || incompatible_feature_flags & 0x0001_f7c0 != 0
        || read_only_compatible_feature_flags & 0x0000_0378 != 0
    {
        4
    } else if compatible_feature_flags & 0x0000_0004 != 0
        || incompatible_feature_flags & 0x0000_000c != 0
    {
        3
    } else {
        2
    }
}

fn has_unsupported_features(incompatible_feature_flags: u32) -> bool {
    let supported_flags = 0x0000_0002
        | 0x0000_0004
        | EXT_INCOMPATIBLE_FEATURE_FLAG_JOURNAL_DEVICE
        | EXT_INCOMPATIBLE_FEATURE_FLAG_HAS_EXTENTS
        | EXT_INCOMPATIBLE_FEATURE_FLAG_64BIT_SUPPORT
        | EXT_INCOMPATIBLE_FEATURE_FLAG_HAS_FLEX_BLOCK_GROUPS
        | 0x0000_0400
        | EXT_INCOMPATIBLE_FEATURE_FLAG_HAS_METADATA_CHECKSUM_SEED
        | EXT_INODE_FLAG_INLINE_DATA
        | 0x0001_0000
        | 0x0002_0000;

    incompatible_feature_flags & !supported_flags != 0
}

fn determine_group_descriptor_size(
    format_version: u8,
    incompatible_feature_flags: u32,
    group_descriptor_size: u16,
) -> Result<usize, ErrorTrace> {
    let size = if format_version == 4 {
        if incompatible_feature_flags & EXT_INCOMPATIBLE_FEATURE_FLAG_64BIT_SUPPORT != 0 {
            usize::from(group_descriptor_size.max(64))
        } else {
            32
        }
    } else {
        32
    };

    if !(32..=1024).contains(&size) {
        return Err(ErrorTrace::new(format!(
            "Invalid ext group descriptor size: {} value out of bounds",
            size,
        )));
    }

    Ok(size)
}

fn read_group_descriptors(
    source: &dyn DataSource,
    format_version: u8,
    group_descriptor_size: usize,
    number_of_group_descriptors: u64,
    offset: u64,
) -> Result<Vec<ExtGroupDescriptor>, ErrorTrace> {
    let data_size = usize::try_from(number_of_group_descriptors)
        .ok()
        .and_then(|number_of_group_descriptors| {
            number_of_group_descriptors.checked_mul(group_descriptor_size)
        })
        .ok_or_else(|| {
            ErrorTrace::new("Unsupported ext group descriptor table size".to_string())
        })?;

    if data_size == 0 || data_size > 16_777_216 {
        return Err(ErrorTrace::new(format!(
            "Unsupported ext group descriptor table size: {} value out of bounds",
            data_size,
        )));
    }

    let mut data = vec![0u8; data_size];
    source.read_exact_at(offset, &mut data)?;

    let mut group_descriptors = Vec::new();
    let mut data_offset: usize = 0;
    let empty_group_descriptor = vec![0; group_descriptor_size];

    for _ in 0..number_of_group_descriptors as usize {
        let data_end_offset = data_offset + group_descriptor_size;
        if data[data_offset..data_end_offset] == empty_group_descriptor {
            break;
        }

        group_descriptors.push(read_group_descriptor(
            format_version,
            &data[data_offset..data_end_offset],
        )?);
        data_offset = data_end_offset;
    }

    Ok(group_descriptors)
}

fn read_group_descriptor(
    format_version: u8,
    data: &[u8],
) -> Result<ExtGroupDescriptor, ErrorTrace> {
    if data.len() < 32 {
        return Err(ErrorTrace::new(
            "Unsupported ext group descriptor size".to_string(),
        ));
    }

    let inode_table_block_number = if format_version == 4 {
        let lower_32bit = read_u32_le(data, 8)? as u64;
        let upper_32bit = if data.len() >= 44 {
            read_u32_le(data, 40)? as u64
        } else {
            0
        };

        (upper_32bit << 32) | lower_32bit
    } else {
        read_u32_le(data, 8)? as u64
    };

    Ok(ExtGroupDescriptor {
        inode_table_block_number,
    })
}

fn read_directory_entries_from_data(
    data: &[u8],
) -> Result<BTreeMap<String, ExtDirectoryEntry>, ErrorTrace> {
    let mut data_offset: usize = 0;
    let mut entries = BTreeMap::new();

    while data_offset < data.len() {
        if data.len() - data_offset < 8 {
            return Err(ErrorTrace::new(
                "Unsupported ext directory entry data size".to_string(),
            ));
        }

        let inode_number = read_u32_le(data, data_offset)?;
        let entry_size = read_u16_le(data, data_offset + 4)? as usize;
        let name_size = data[data_offset + 6] as usize;

        if entry_size == 0 {
            break;
        }
        if entry_size < 8 || entry_size > data.len() - data_offset {
            return Err(ErrorTrace::new(format!(
                "Invalid ext directory entry size: {} value out of bounds",
                entry_size,
            )));
        }

        let name_end_offset = data_offset + 8 + name_size;
        if name_end_offset > data_offset + entry_size {
            return Err(ErrorTrace::new(format!(
                "Invalid ext directory entry name size: {} value out of bounds",
                name_size,
            )));
        }

        let name = String::from_utf8_lossy(&data[data_offset + 8..name_end_offset]).into_owned();

        if inode_number != 0 && name != "." && name != ".." {
            entries.insert(name.clone(), ExtDirectoryEntry { inode_number, name });
        }

        data_offset += entry_size;
    }

    Ok(entries)
}

fn read_block_number_ranges(
    data_reference: &[u8; 60],
    source: &dyn DataSource,
    block_size: u32,
    number_of_blocks: u64,
) -> Result<Vec<ExtBlockRange>, ErrorTrace> {
    let context = ExtBlockNumbersContext {
        source,
        block_size,
        number_of_blocks_per_block: block_size / 4,
        number_of_blocks,
    };
    let mut logical_block_number: u64 = 0;
    let mut block_ranges = Vec::new();

    read_block_number_node(
        &context,
        &data_reference[0..48],
        &mut logical_block_number,
        &mut block_ranges,
        0,
    )?;
    read_block_number_node(
        &context,
        &data_reference[48..52],
        &mut logical_block_number,
        &mut block_ranges,
        1,
    )?;
    read_block_number_node(
        &context,
        &data_reference[52..56],
        &mut logical_block_number,
        &mut block_ranges,
        2,
    )?;
    read_block_number_node(
        &context,
        &data_reference[56..60],
        &mut logical_block_number,
        &mut block_ranges,
        3,
    )?;

    if logical_block_number < context.number_of_blocks {
        merge_block_range(
            &mut block_ranges,
            ExtBlockRange {
                logical_block_number,
                physical_block_number: 0,
                number_of_blocks: context.number_of_blocks - logical_block_number,
                range_type: ExtBlockRangeType::Sparse,
            },
        );
    }

    Ok(block_ranges)
}

fn read_block_number_node(
    context: &ExtBlockNumbersContext<'_>,
    data: &[u8],
    logical_block_number: &mut u64,
    block_ranges: &mut Vec<ExtBlockRange>,
    depth: u16,
) -> Result<(), ErrorTrace> {
    for data_offset in (0..data.len()).step_by(4) {
        if *logical_block_number >= context.number_of_blocks {
            break;
        }

        let block_number = read_u32_le(data, data_offset)?;
        if block_number != 0 && depth > 0 {
            let sub_node_offset = (block_number as u64)
                .checked_mul(context.block_size as u64)
                .ok_or_else(|| ErrorTrace::new("ext indirect block offset overflow".to_string()))?;
            let mut node_data = vec![0u8; context.block_size as usize];

            context
                .source
                .read_exact_at(sub_node_offset, &mut node_data)?;
            read_block_number_node(
                context,
                &node_data,
                logical_block_number,
                block_ranges,
                depth - 1,
            )?;
            continue;
        }

        let mut range_number_of_blocks: u64 = 1;
        if block_number == 0 {
            for _ in 0..depth {
                range_number_of_blocks *= context.number_of_blocks_per_block as u64;
            }
            range_number_of_blocks = min(
                range_number_of_blocks,
                context.number_of_blocks - *logical_block_number,
            );
        }

        merge_block_range(
            block_ranges,
            ExtBlockRange {
                logical_block_number: *logical_block_number,
                physical_block_number: block_number as u64,
                number_of_blocks: range_number_of_blocks,
                range_type: if block_number == 0 {
                    ExtBlockRangeType::Sparse
                } else {
                    ExtBlockRangeType::InFile
                },
            },
        );
        *logical_block_number += range_number_of_blocks;
    }

    Ok(())
}

fn read_extents_block_ranges(
    data_reference: &[u8; 60],
    source: &dyn DataSource,
    block_size: u32,
    number_of_blocks: u64,
) -> Result<Vec<ExtBlockRange>, ErrorTrace> {
    let mut logical_block_number: u64 = 0;
    let mut block_ranges = Vec::new();

    read_extents_node(
        data_reference,
        source,
        block_size,
        &mut logical_block_number,
        &mut block_ranges,
        6,
    )?;

    if logical_block_number < number_of_blocks {
        merge_block_range(
            &mut block_ranges,
            ExtBlockRange {
                logical_block_number,
                physical_block_number: 0,
                number_of_blocks: number_of_blocks - logical_block_number,
                range_type: ExtBlockRangeType::Sparse,
            },
        );
    }

    Ok(block_ranges)
}

fn read_extents_node(
    data: &[u8],
    source: &dyn DataSource,
    block_size: u32,
    logical_block_number: &mut u64,
    block_ranges: &mut Vec<ExtBlockRange>,
    parent_depth: u16,
) -> Result<(), ErrorTrace> {
    let extents_header = read_extents_header(data)?;

    if extents_header.depth >= parent_depth {
        return Err(ErrorTrace::new(format!(
            "Invalid ext extents depth: {} value out of bounds",
            extents_header.depth,
        )));
    }

    let mut data_offset: usize = 12;

    if extents_header.depth > 0 {
        for _ in 0..extents_header.number_of_entries {
            let extent_index = read_extent_index(&data[data_offset..data_offset + 12])?;
            let sub_node_offset = extent_index
                .physical_block_number
                .checked_mul(block_size as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("ext extent index block offset overflow".to_string())
                })?;
            let mut node_data = vec![0u8; block_size as usize];

            source.read_exact_at(sub_node_offset, &mut node_data)?;
            read_extents_node(
                &node_data,
                source,
                block_size,
                logical_block_number,
                block_ranges,
                extents_header.depth,
            )?;
            data_offset += 12;
        }
    } else {
        for _ in 0..extents_header.number_of_entries {
            let extent_descriptor = read_extent_descriptor(&data[data_offset..data_offset + 12])?;

            if extent_descriptor.logical_block_number as u64 > *logical_block_number {
                merge_block_range(
                    block_ranges,
                    ExtBlockRange {
                        logical_block_number: *logical_block_number,
                        physical_block_number: 0,
                        number_of_blocks: extent_descriptor.logical_block_number as u64
                            - *logical_block_number,
                        range_type: ExtBlockRangeType::Sparse,
                    },
                );
                *logical_block_number = extent_descriptor.logical_block_number as u64;
            }

            let mut range_number_of_blocks = extent_descriptor.number_of_blocks as u64;
            let mut range_type = if extent_descriptor.physical_block_number == 0 {
                ExtBlockRangeType::Sparse
            } else {
                ExtBlockRangeType::InFile
            };

            if range_number_of_blocks > 32768 {
                range_number_of_blocks -= 32768;
                range_type = ExtBlockRangeType::Sparse;
            }

            merge_block_range(
                block_ranges,
                ExtBlockRange {
                    logical_block_number: extent_descriptor.logical_block_number as u64,
                    physical_block_number: extent_descriptor.physical_block_number,
                    number_of_blocks: range_number_of_blocks,
                    range_type,
                },
            );
            *logical_block_number =
                extent_descriptor.logical_block_number as u64 + range_number_of_blocks;
            data_offset += 12;
        }
    }

    Ok(())
}

fn read_extents_header(data: &[u8]) -> Result<ExtExtentsHeader, ErrorTrace> {
    if data.len() < 12 {
        return Err(ErrorTrace::new(
            "Unsupported ext extents header size".to_string(),
        ));
    }
    if data[0..2] != EXT_EXTENTS_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported ext extents signature".to_string(),
        ));
    }

    let depth = read_u16_le(data, 6)?;
    if depth > 5 {
        return Err(ErrorTrace::new(format!(
            "Invalid ext extents depth: {} value out of bounds",
            depth,
        )));
    }

    Ok(ExtExtentsHeader {
        number_of_entries: read_u16_le(data, 2)?,
        depth,
    })
}

fn read_extent_descriptor(data: &[u8]) -> Result<ExtExtentDescriptor, ErrorTrace> {
    if data.len() < 12 {
        return Err(ErrorTrace::new(
            "Unsupported ext extent descriptor size".to_string(),
        ));
    }

    Ok(ExtExtentDescriptor {
        logical_block_number: read_u32_le(data, 0)?,
        number_of_blocks: read_u16_le(data, 4)?,
        physical_block_number: ((read_u16_le(data, 6)? as u64) << 32)
            | (read_u32_le(data, 8)? as u64),
    })
}

fn read_extent_index(data: &[u8]) -> Result<ExtExtentIndex, ErrorTrace> {
    if data.len() < 12 {
        return Err(ErrorTrace::new(
            "Unsupported ext extent index size".to_string(),
        ));
    }

    Ok(ExtExtentIndex {
        physical_block_number: ((read_u16_le(data, 8)? as u64) << 32)
            | (read_u32_le(data, 4)? as u64),
    })
}

fn merge_block_range(block_ranges: &mut Vec<ExtBlockRange>, next_block_range: ExtBlockRange) {
    if let Some(last_block_range) = block_ranges.last_mut() {
        match last_block_range.range_type {
            ExtBlockRangeType::InFile => {
                if next_block_range.range_type == ExtBlockRangeType::InFile
                    && next_block_range.physical_block_number
                        == last_block_range.physical_block_number
                            + last_block_range.number_of_blocks
                    && next_block_range.logical_block_number
                        == last_block_range.logical_block_number + last_block_range.number_of_blocks
                {
                    last_block_range.number_of_blocks += next_block_range.number_of_blocks;
                    return;
                }
            }
            ExtBlockRangeType::Sparse => {
                if next_block_range.range_type == ExtBlockRangeType::Sparse
                    && next_block_range.logical_block_number
                        == last_block_range.logical_block_number + last_block_range.number_of_blocks
                {
                    last_block_range.number_of_blocks += next_block_range.number_of_blocks;
                    return;
                }
            }
        }
    }

    block_ranges.push(next_block_range);
}

fn merge_extent(
    current_extent: Option<ExtentMapEntry>,
    next_extent: ExtentMapEntry,
    extents: &mut Vec<ExtentMapEntry>,
) -> Option<ExtentMapEntry> {
    match current_extent {
        Some(mut current_extent) => {
            let can_merge = match (&current_extent.target, &next_extent.target) {
                (ExtentMapTarget::Zero, ExtentMapTarget::Zero) => {
                    current_extent.logical_offset + current_extent.size
                        == next_extent.logical_offset
                }
                (
                    ExtentMapTarget::Data {
                        source: current_source,
                        source_offset: current_source_offset,
                    },
                    ExtentMapTarget::Data {
                        source: next_source,
                        source_offset: next_source_offset,
                    },
                ) => {
                    Arc::ptr_eq(current_source, next_source)
                        && current_extent.logical_offset + current_extent.size
                            == next_extent.logical_offset
                        && *current_source_offset + current_extent.size == *next_source_offset
                }
                _ => false,
            };

            if can_merge {
                current_extent.size += next_extent.size;
                Some(current_extent)
            } else {
                extents.push(current_extent);
                Some(next_extent)
            }
        }
        None => Some(next_extent),
    }
}

fn trim_nul_terminated(data: &[u8]) -> String {
    let end_offset = data
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.len());

    String::from_utf8_lossy(&data[..end_offset]).into_owned()
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, ErrorTrace> {
    Ok(u16::from_le_bytes(
        data[offset..offset + 2]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read ext u16 value".to_string()))?,
    ))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, ErrorTrace> {
    Ok(u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read ext u32 value".to_string()))?,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn open_file_system(path: &str) -> Result<ExtFileSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path(path));
        let source = open_local_data_source(&path)?;

        ExtFileSystem::open(source)
    }

    #[test]
    fn read_ext2_file_empty() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext2.raw")?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 0);
        assert_eq!(md5_hash.as_str(), "d41d8cd98f00b204e9800998ecf8427e");
        Ok(())
    }

    #[test]
    fn read_ext2_file_regular() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext2.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/TestFile2")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 11358);
        assert_eq!(md5_hash.as_str(), "3b83ef96387f14655fc854ddc3c6bd57");
        Ok(())
    }

    #[test]
    fn read_ext2_file_with_initial_sparse_extent() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext2.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/initial_sparse1")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 1048611);
        assert_eq!(md5_hash.as_str(), "c53dd591cf199ec5d692de2cbdb8559b");
        Ok(())
    }

    #[test]
    fn read_ext2_file_with_trailing_sparse_extent() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext2.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/trailing_sparse1")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 1048576);
        assert_eq!(md5_hash.as_str(), "e0b16e3a6c58c67928b5895797fccaa0");
        Ok(())
    }

    #[test]
    fn read_ext4_file_empty() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 0);
        assert_eq!(md5_hash.as_str(), "d41d8cd98f00b204e9800998ecf8427e");
        Ok(())
    }

    #[test]
    fn read_ext4_file_regular() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/TestFile2")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 11358);
        assert_eq!(md5_hash.as_str(), "3b83ef96387f14655fc854ddc3c6bd57");
        Ok(())
    }

    #[test]
    fn read_ext4_file_with_inline_data() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/testfile1")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 9);
        assert_eq!(md5_hash.as_str(), "7fd0fc35a8c963bf34ba9d57427b3907");
        Ok(())
    }

    #[test]
    fn read_ext4_file_with_initial_sparse_extent() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/initial_sparse1")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 1048611);
        assert_eq!(md5_hash.as_str(), "c53dd591cf199ec5d692de2cbdb8559b");
        Ok(())
    }

    #[test]
    fn read_ext4_file_with_trailing_sparse_extent() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/trailing_sparse1")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 1048576);
        assert_eq!(md5_hash.as_str(), "e0b16e3a6c58c67928b5895797fccaa0");
        Ok(())
    }

    #[test]
    fn read_ext4_file_with_uninitialized_extent() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/uninitialized1")?
            .unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 4130);
        assert_eq!(md5_hash.as_str(), "5f43bd7169cfd72a1e0b5270970911f1");
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("ext/ext4.raw")?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/TestFile2")?
            .unwrap();
        let capabilities = file_entry.open_source()?.unwrap().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }
}
