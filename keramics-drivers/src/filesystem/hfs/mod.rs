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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{
    DataSource, DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget,
    MemoryDataSource,
};

const HFS_MASTER_DIRECTORY_BLOCK_SIGNATURE: [u8; 2] = *b"BD";
const HFSPLUS_VOLUME_HEADER_SIGNATURE: [u8; 2] = *b"H+";
const HFSX_VOLUME_HEADER_SIGNATURE: [u8; 2] = *b"HX";
const HFS_ROOT_DIRECTORY_IDENTIFIER: u32 = 2;

/// HFS format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HfsFormat {
    Hfs,
    HfsPlus,
    HfsX,
}

#[derive(Clone)]
struct HfsExtentDescriptor {
    block_number: u32,
    number_of_blocks: u32,
}

#[derive(Clone)]
struct HfsForkDescriptor {
    size: u64,
    number_of_blocks: u32,
    extents: Vec<HfsExtentDescriptor>,
}

#[derive(Clone)]
struct HfsCatalogEntry {
    identifier: u32,
    parent_identifier: u32,
    name: String,
    is_directory: bool,
    data_fork_descriptor: Option<HfsForkDescriptor>,
}

#[derive(Clone)]
struct HfsRuntime {
    source: DataSourceReference,
    format: HfsFormat,
    block_size: u32,
    data_area_block_number: u16,
    entries_by_id: HashMap<u32, HfsCatalogEntry>,
    children_by_parent: HashMap<u32, BTreeMap<String, u32>>,
}

/// Immutable HFS file entry.
#[derive(Clone)]
pub struct HfsFileEntry {
    runtime: Arc<HfsRuntime>,
    identifier: u32,
}

/// Immutable HFS file system.
pub struct HfsFileSystem {
    runtime: Arc<HfsRuntime>,
}

impl HfsFileSystem {
    /// Opens and parses an HFS or HFS+ file system.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let mut header_data = [0u8; 512];

        source.read_exact_at(1024, &mut header_data)?;

        let (format, block_size, data_area_block_number, catalog_fork_descriptor) =
            match [header_data[0], header_data[1]] {
                HFS_MASTER_DIRECTORY_BLOCK_SIGNATURE => parse_master_directory_block(&header_data)?,
                HFSPLUS_VOLUME_HEADER_SIGNATURE | HFSX_VOLUME_HEADER_SIGNATURE => {
                    parse_volume_header(&header_data)?
                }
                _ => {
                    return Err(ErrorTrace::new(
                        "Unsupported HFS signature at offset 1024".to_string(),
                    ));
                }
            };

        let catalog_source = build_fork_source(
            source.clone(),
            format,
            block_size,
            data_area_block_number,
            &catalog_fork_descriptor,
        )?;
        let catalog_header = read_btree_header(catalog_source.as_ref())?;
        let catalog_entries =
            read_catalog_entries(catalog_source.as_ref(), format, &catalog_header)?;
        let mut entries_by_id: HashMap<u32, HfsCatalogEntry> = HashMap::new();
        let mut children_by_parent: HashMap<u32, BTreeMap<String, u32>> = HashMap::new();

        for entry in catalog_entries {
            children_by_parent
                .entry(entry.parent_identifier)
                .or_default()
                .insert(normalize_lookup_name(&entry.name), entry.identifier);
            entries_by_id.insert(entry.identifier, entry);
        }

        let runtime = Arc::new(HfsRuntime {
            source,
            format,
            block_size,
            data_area_block_number,
            entries_by_id,
            children_by_parent,
        });

        Ok(Self { runtime })
    }

    /// Retrieves the format.
    pub fn format(&self) -> HfsFormat {
        self.runtime.format
    }

    /// Retrieves the root directory.
    pub fn root_directory(&self) -> Result<HfsFileEntry, ErrorTrace> {
        self.file_entry_by_identifier(HFS_ROOT_DIRECTORY_IDENTIFIER)?
            .ok_or_else(|| ErrorTrace::new("Missing HFS root directory entry".to_string()))
    }

    /// Retrieves a file entry by catalog node identifier.
    pub fn file_entry_by_identifier(
        &self,
        identifier: u32,
    ) -> Result<Option<HfsFileEntry>, ErrorTrace> {
        if identifier == HFS_ROOT_DIRECTORY_IDENTIFIER
            || self.runtime.entries_by_id.contains_key(&identifier)
        {
            return Ok(Some(HfsFileEntry {
                runtime: self.runtime.clone(),
                identifier,
            }));
        }

        Ok(None)
    }

    /// Retrieves a file entry by absolute path.
    pub fn file_entry_by_path(&self, path: &str) -> Result<Option<HfsFileEntry>, ErrorTrace> {
        if path.is_empty() || !path.starts_with('/') {
            return Ok(None);
        }
        if path == "/" {
            return Ok(Some(self.root_directory()?));
        }

        let mut identifier = HFS_ROOT_DIRECTORY_IDENTIFIER;

        for path_component in path.split('/').filter(|component| !component.is_empty()) {
            let lookup_name = normalize_lookup_name(path_component);
            let child_identifier = match self.runtime.children_by_parent.get(&identifier) {
                Some(children) => children.get(&lookup_name).copied(),
                None => None,
            };

            identifier = match child_identifier {
                Some(identifier) => identifier,
                None => return Ok(None),
            };
        }

        self.file_entry_by_identifier(identifier)
    }
}

impl HfsFileEntry {
    /// Retrieves the identifier.
    pub fn identifier(&self) -> u32 {
        self.identifier
    }

    /// Retrieves the name.
    pub fn name(&self) -> Option<&str> {
        if self.identifier == HFS_ROOT_DIRECTORY_IDENTIFIER {
            self.runtime
                .entries_by_id
                .get(&self.identifier)
                .map(|entry| entry.name.as_str())
        } else {
            self.runtime
                .entries_by_id
                .get(&self.identifier)
                .map(|entry| entry.name.as_str())
        }
    }

    /// Determines if the entry is a directory.
    pub fn is_directory(&self) -> bool {
        if self.identifier == HFS_ROOT_DIRECTORY_IDENTIFIER {
            return self
                .runtime
                .entries_by_id
                .get(&self.identifier)
                .map(|entry| entry.is_directory)
                .unwrap_or(true);
        }

        self.runtime
            .entries_by_id
            .get(&self.identifier)
            .map(|entry| entry.is_directory)
            .unwrap_or(false)
    }

    /// Retrieves the size.
    pub fn size(&self) -> u64 {
        self.runtime
            .entries_by_id
            .get(&self.identifier)
            .and_then(|entry| entry.data_fork_descriptor.as_ref())
            .map_or(0, |fork_descriptor| fork_descriptor.size)
    }

    /// Opens the default data source.
    pub fn open_source(&self) -> Result<Option<DataSourceReference>, ErrorTrace> {
        let entry = match self.runtime.entries_by_id.get(&self.identifier) {
            Some(entry) => entry,
            None => return Ok(None),
        };

        if entry.is_directory {
            return Ok(None);
        }

        let fork_descriptor = match entry.data_fork_descriptor.as_ref() {
            Some(fork_descriptor) => fork_descriptor,
            None => return Ok(Some(Arc::new(MemoryDataSource::new(Vec::new())))),
        };

        if fork_descriptor.size == 0 {
            return Ok(Some(Arc::new(MemoryDataSource::new(Vec::new()))));
        }

        Ok(Some(build_fork_source(
            self.runtime.source.clone(),
            self.runtime.format,
            self.runtime.block_size,
            self.runtime.data_area_block_number,
            fork_descriptor,
        )?))
    }
}

fn parse_master_directory_block(
    data: &[u8],
) -> Result<(HfsFormat, u32, u16, HfsForkDescriptor), ErrorTrace> {
    let block_size = read_u32_be(data, 20)?;
    let data_area_block_number = read_u16_be(data, 28)?;
    let catalog_file_size = read_u32_be(data, 146)? as u64;
    let catalog_file_number_of_blocks = catalog_file_size.div_ceil(block_size as u64) as u32;
    let mut extents = Vec::new();

    for data_offset in (150..162).step_by(4) {
        if data[data_offset..data_offset + 4] == [0; 4] {
            break;
        }

        extents.push(HfsExtentDescriptor {
            block_number: read_u16_be(data, data_offset)? as u32,
            number_of_blocks: read_u16_be(data, data_offset + 2)? as u32,
        });
    }

    Ok((
        HfsFormat::Hfs,
        block_size,
        data_area_block_number,
        HfsForkDescriptor {
            size: catalog_file_size,
            number_of_blocks: catalog_file_number_of_blocks,
            extents,
        },
    ))
}

fn parse_volume_header(
    data: &[u8],
) -> Result<(HfsFormat, u32, u16, HfsForkDescriptor), ErrorTrace> {
    let format = match [data[0], data[1]] {
        HFSPLUS_VOLUME_HEADER_SIGNATURE => HfsFormat::HfsPlus,
        HFSX_VOLUME_HEADER_SIGNATURE => HfsFormat::HfsX,
        _ => {
            return Err(ErrorTrace::new(
                "Unsupported HFS volume header signature".to_string(),
            ));
        }
    };
    let block_size = read_u32_be(data, 40)?;
    let catalog_file_fork_descriptor = read_fork_descriptor(&data[272..352])?;

    Ok((format, block_size, 0, catalog_file_fork_descriptor))
}

fn read_fork_descriptor(data: &[u8]) -> Result<HfsForkDescriptor, ErrorTrace> {
    if data.len() < 80 {
        return Err(ErrorTrace::new(
            "Unsupported HFS fork descriptor size".to_string(),
        ));
    }

    let size = read_u64_be(data, 0)?;
    let number_of_blocks = read_u32_be(data, 12)?;
    let mut extents = Vec::new();

    for data_offset in (16..80).step_by(8) {
        if data[data_offset..data_offset + 8] == [0; 8] {
            break;
        }

        extents.push(HfsExtentDescriptor {
            block_number: read_u32_be(data, data_offset)?,
            number_of_blocks: read_u32_be(data, data_offset + 4)?,
        });
    }

    Ok(HfsForkDescriptor {
        size,
        number_of_blocks,
        extents,
    })
}

fn build_fork_source(
    source: DataSourceReference,
    format: HfsFormat,
    block_size: u32,
    data_area_block_number: u16,
    fork_descriptor: &HfsForkDescriptor,
) -> Result<DataSourceReference, ErrorTrace> {
    if fork_descriptor.size == 0 {
        return Ok(Arc::new(MemoryDataSource::new(Vec::new())));
    }

    let total_inline_blocks: u32 = fork_descriptor
        .extents
        .iter()
        .map(|extent| extent.number_of_blocks)
        .sum();

    if total_inline_blocks < fork_descriptor.number_of_blocks {
        return Err(ErrorTrace::new(
            "HFS overflow extents are not supported yet in keramics-drivers".to_string(),
        ));
    }

    let mut extents = Vec::new();
    let mut logical_offset: u64 = 0;
    let mut remaining_size = fork_descriptor.size;

    for extent in fork_descriptor.extents.iter() {
        if remaining_size == 0 {
            break;
        }

        let extent_size = (extent.number_of_blocks as u64)
            .checked_mul(block_size as u64)
            .ok_or_else(|| ErrorTrace::new("HFS extent size overflow".to_string()))?;
        let size = min(extent_size, remaining_size);
        let physical_block_number = match format {
            HfsFormat::Hfs => extent.block_number + data_area_block_number as u32,
            HfsFormat::HfsPlus | HfsFormat::HfsX => extent.block_number,
        };
        let source_offset = (physical_block_number as u64)
            .checked_mul(block_size as u64)
            .ok_or_else(|| ErrorTrace::new("HFS source offset overflow".to_string()))?;

        extents.push(ExtentMapEntry {
            logical_offset,
            size,
            target: ExtentMapTarget::Data {
                source: source.clone(),
                source_offset,
            },
        });

        logical_offset += size;
        remaining_size -= size;
    }

    if remaining_size != 0 {
        return Err(ErrorTrace::new(
            "HFS inline extents do not cover the full fork size".to_string(),
        ));
    }

    Ok(Arc::new(ExtentMapDataSource::new(extents)?))
}

struct HfsBtreeHeader {
    node_size: u16,
    first_leaf_node_number: u32,
}

fn read_btree_header(source: &dyn DataSource) -> Result<HfsBtreeHeader, ErrorTrace> {
    let mut data = [0u8; 512];

    source.read_exact_at(0, &mut data)?;

    let node_descriptor = read_btree_node_descriptor(&data)?;
    if node_descriptor.node_type != 1 {
        return Err(ErrorTrace::new(
            "Unsupported node type in first HFS B-tree node descriptor".to_string(),
        ));
    }
    let node_size = read_u16_be(&data, 14 + 18)?;
    let first_leaf_node_number = read_u32_be(&data, 14 + 10)?;

    if ![512, 1024, 2048, 4096, 8192, 16384, 32768].contains(&node_size) {
        return Err(ErrorTrace::new(format!(
            "Unsupported HFS B-tree node size: {}",
            node_size,
        )));
    }

    Ok(HfsBtreeHeader {
        node_size,
        first_leaf_node_number,
    })
}

struct HfsBtreeNode {
    next_node_number: u32,
    node_type: u8,
    records: Vec<(usize, usize)>,
    data: Vec<u8>,
}

fn read_btree_node(
    source: &dyn DataSource,
    node_number: u32,
    node_size: u16,
) -> Result<HfsBtreeNode, ErrorTrace> {
    let node_offset = (node_number as u64)
        .checked_mul(node_size as u64)
        .ok_or_else(|| ErrorTrace::new("HFS B-tree node offset overflow".to_string()))?;
    let mut data = vec![0u8; node_size as usize];

    source.read_exact_at(node_offset, &mut data)?;

    let node_descriptor = read_btree_node_descriptor(&data)?;
    let record_offsets_data_size = (node_descriptor.number_of_records as usize + 1) * 2;
    if record_offsets_data_size > data.len() {
        return Err(ErrorTrace::new(
            "Invalid HFS B-tree node number of records value out of bounds".to_string(),
        ));
    }

    let record_offsets_data_offset = data.len() - record_offsets_data_size;
    let mut records = Vec::new();
    let mut data_offset = data.len() - 2;

    for _ in 0..node_descriptor.number_of_records {
        let record_offset = read_u16_be(&data, data_offset)? as usize;
        if record_offset < 14 || record_offset >= record_offsets_data_offset {
            return Err(ErrorTrace::new(
                "Invalid HFS B-tree record offset value out of bounds".to_string(),
            ));
        }
        records.push((record_offset, 0));
        data_offset -= 2;
    }

    records.sort_by_key(|(offset, _)| *offset);
    for record_index in 0..records.len() {
        let next_record_offset = if record_index + 1 < records.len() {
            records[record_index + 1].0
        } else {
            record_offsets_data_offset
        };
        records[record_index].1 = next_record_offset - records[record_index].0;
    }

    Ok(HfsBtreeNode {
        next_node_number: node_descriptor.next_node_number,
        node_type: node_descriptor.node_type,
        records,
        data,
    })
}

struct HfsBtreeNodeDescriptorData {
    next_node_number: u32,
    node_type: u8,
    number_of_records: u16,
}

fn read_btree_node_descriptor(data: &[u8]) -> Result<HfsBtreeNodeDescriptorData, ErrorTrace> {
    if data.len() < 14 {
        return Err(ErrorTrace::new(
            "Unsupported HFS B-tree node descriptor size".to_string(),
        ));
    }

    let node_type = data[8];
    if node_type != 0x00 && node_type != 0x01 && node_type != 0x02 && node_type != 0xff {
        return Err(ErrorTrace::new(format!(
            "Unsupported HFS B-tree node type value: {}",
            node_type,
        )));
    }

    Ok(HfsBtreeNodeDescriptorData {
        next_node_number: read_u32_be(data, 0)?,
        node_type,
        number_of_records: read_u16_be(data, 10)?,
    })
}

fn read_catalog_entries(
    source: &dyn DataSource,
    format: HfsFormat,
    btree_header: &HfsBtreeHeader,
) -> Result<Vec<HfsCatalogEntry>, ErrorTrace> {
    let mut entries = Vec::new();
    let mut read_node_numbers = HashSet::new();
    let mut node_number = btree_header.first_leaf_node_number;

    while node_number != 0 {
        if !read_node_numbers.insert(node_number) {
            return Err(ErrorTrace::new(format!(
                "HFS B-tree node: {} already read",
                node_number,
            )));
        }

        let node = read_btree_node(source, node_number, btree_header.node_size)?;
        if node.node_type != 0xff {
            return Err(ErrorTrace::new(
                "Unsupported non-leaf node encountered while scanning HFS catalog leaves"
                    .to_string(),
            ));
        }

        for (record_offset, record_size) in node.records.iter().copied() {
            let record_data = &node.data[record_offset..record_offset + record_size];
            if let Some(entry) = parse_catalog_record(format, record_data)? {
                entries.push(entry);
            }
        }

        node_number = node.next_node_number;
    }

    Ok(entries)
}

fn parse_catalog_record(
    format: HfsFormat,
    data: &[u8],
) -> Result<Option<HfsCatalogEntry>, ErrorTrace> {
    let key = read_catalog_key(format, data)?;
    let mut data_offset = key.size;

    if format == HfsFormat::Hfs && !data_offset.is_multiple_of(2) {
        data_offset += 1;
    }
    if data_offset + 2 > data.len() {
        return Err(ErrorTrace::new(
            "Invalid HFS catalog record data size value out of bounds".to_string(),
        ));
    }

    let record_type = read_u16_be(data, data_offset)?;

    match (format, record_type) {
        (HfsFormat::Hfs, 0x0100) | (HfsFormat::HfsPlus | HfsFormat::HfsX, 0x0001) => {
            let identifier = if format == HfsFormat::Hfs {
                read_u32_be(data, data_offset + 6)?
            } else {
                read_u32_be(data, data_offset + 8)?
            };

            Ok(Some(HfsCatalogEntry {
                identifier,
                parent_identifier: key.parent_identifier,
                name: key.name,
                is_directory: true,
                data_fork_descriptor: None,
            }))
        }
        (HfsFormat::Hfs, 0x0200) => {
            let identifier = read_u32_be(data, data_offset + 20)?;
            let data_fork_size = read_u32_be(data, data_offset + 26)? as u64;
            let mut extents = Vec::new();

            for extent_offset in (data_offset + 74..data_offset + 86).step_by(4) {
                if data[extent_offset..extent_offset + 4] == [0; 4] {
                    break;
                }
                extents.push(HfsExtentDescriptor {
                    block_number: read_u16_be(data, extent_offset)? as u32,
                    number_of_blocks: read_u16_be(data, extent_offset + 2)? as u32,
                });
            }

            Ok(Some(HfsCatalogEntry {
                identifier,
                parent_identifier: key.parent_identifier,
                name: key.name,
                is_directory: false,
                data_fork_descriptor: Some(HfsForkDescriptor {
                    size: data_fork_size,
                    number_of_blocks: data_fork_size.div_ceil(512) as u32,
                    extents,
                }),
            }))
        }
        (HfsFormat::HfsPlus | HfsFormat::HfsX, 0x0002) => {
            let identifier = read_u32_be(data, data_offset + 8)?;
            let data_fork_descriptor =
                read_fork_descriptor(&data[data_offset + 88..data_offset + 168])?;

            Ok(Some(HfsCatalogEntry {
                identifier,
                parent_identifier: key.parent_identifier,
                name: key.name,
                is_directory: false,
                data_fork_descriptor: Some(data_fork_descriptor),
            }))
        }
        (HfsFormat::Hfs, 0x0300 | 0x0400)
        | (HfsFormat::HfsPlus | HfsFormat::HfsX, 0x0003 | 0x0004) => Ok(None),
        _ => Ok(None),
    }
}

struct HfsCatalogKeyData {
    size: usize,
    parent_identifier: u32,
    name: String,
}

fn read_catalog_key(format: HfsFormat, data: &[u8]) -> Result<HfsCatalogKeyData, ErrorTrace> {
    match format {
        HfsFormat::Hfs => {
            if data.is_empty() {
                return Err(ErrorTrace::new(
                    "Unsupported HFS standard catalog key size".to_string(),
                ));
            }
            let key_data_size = data[0] as usize;
            if key_data_size > data.len().saturating_sub(1) {
                return Err(ErrorTrace::new(
                    "Invalid HFS standard catalog key data size value out of bounds".to_string(),
                ));
            }
            let size = 1 + key_data_size;
            let parent_identifier = if key_data_size >= 6 {
                read_u32_be(data, 2)?
            } else {
                0
            };
            let name_size = if key_data_size >= 6 {
                data[6] as usize
            } else {
                0
            };
            let name_end_offset = 7 + name_size;

            if name_end_offset > data.len() {
                return Err(ErrorTrace::new(
                    "Invalid HFS standard catalog key name size value out of bounds".to_string(),
                ));
            }

            Ok(HfsCatalogKeyData {
                size,
                parent_identifier,
                name: String::from_utf8_lossy(&data[7..name_end_offset]).into_owned(),
            })
        }
        HfsFormat::HfsPlus | HfsFormat::HfsX => {
            if data.len() < 2 {
                return Err(ErrorTrace::new(
                    "Unsupported HFS extended catalog key size".to_string(),
                ));
            }
            let key_data_size = read_u16_be(data, 0)? as usize;
            if key_data_size > data.len().saturating_sub(2) {
                return Err(ErrorTrace::new(
                    "Invalid HFS extended catalog key data size value out of bounds".to_string(),
                ));
            }
            let size = 2 + key_data_size;
            let parent_identifier = if key_data_size >= 4 {
                read_u32_be(data, 2)?
            } else {
                0
            };
            let name_size = if key_data_size >= 6 {
                read_u16_be(data, 6)? as usize
            } else {
                0
            };
            let name_offset = 8;
            let name_end_offset = name_offset + (name_size * 2);

            if name_end_offset > data.len() {
                return Err(ErrorTrace::new(
                    "Invalid HFS extended catalog key name size value out of bounds".to_string(),
                ));
            }

            let mut code_units = Vec::with_capacity(name_size);
            for name_data_offset in (name_offset..name_end_offset).step_by(2) {
                let mut value = read_u16_be(data, name_data_offset)?;

                value = match value {
                    0x0000 => 0x2400,
                    0x002f => 0x003a,
                    _ => value,
                };
                code_units.push(value);
            }

            Ok(HfsCatalogKeyData {
                size,
                parent_identifier,
                name: String::from_utf16(code_units.as_slice()).map_err(|error| {
                    ErrorTrace::new(format!(
                        "Unable to decode HFS extended catalog key name with error: {}",
                        error,
                    ))
                })?,
            })
        }
    }
}

fn normalize_lookup_name(name: &str) -> String {
    name.to_lowercase()
}

fn read_u16_be(data: &[u8], offset: usize) -> Result<u16, ErrorTrace> {
    Ok(u16::from_be_bytes(
        data[offset..offset + 2]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read HFS u16 value".to_string()))?,
    ))
}

fn read_u32_be(data: &[u8], offset: usize) -> Result<u32, ErrorTrace> {
    Ok(u32::from_be_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read HFS u32 value".to_string()))?,
    ))
}

fn read_u64_be(data: &[u8], offset: usize) -> Result<u64, ErrorTrace> {
    Ok(u64::from_be_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read HFS u64 value".to_string()))?,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn open_file_system(path: &str) -> Result<HfsFileSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path(path));
        let source = open_local_data_source(&path)?;

        HfsFileSystem::open(source)
    }

    #[test]
    fn read_hfs_empty_file() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("hfs/hfs.raw")?;

        assert_eq!(file_system.format(), HfsFormat::Hfs);

        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(file_entry.identifier(), 16);
        assert_eq!(file_entry.name(), Some("emptyfile"));
        assert_eq!(file_entry.size(), 0);
        assert_eq!(offset, 0);
        assert_eq!(md5_hash.as_str(), "d41d8cd98f00b204e9800998ecf8427e");

        Ok(())
    }

    #[test]
    fn read_hfsplus_empty_file() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("hfs/hfsplus.raw")?;

        assert_eq!(file_system.format(), HfsFormat::HfsPlus);

        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(file_entry.identifier(), 20);
        assert_eq!(file_entry.name(), Some("emptyfile"));
        assert_eq!(file_entry.size(), 0);
        assert_eq!(offset, 0);
        assert_eq!(md5_hash.as_str(), "d41d8cd98f00b204e9800998ecf8427e");

        Ok(())
    }

    #[test]
    fn test_root_directory_and_identifier_lookup() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("hfs/hfsplus.raw")?;

        let root_directory = file_system.root_directory()?;
        let file_entry = file_system.file_entry_by_identifier(20)?.unwrap();

        assert_eq!(root_directory.identifier(), HFS_ROOT_DIRECTORY_IDENTIFIER);
        assert!(root_directory.is_directory());
        assert_eq!(file_entry.name(), Some("emptyfile"));
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file_system = open_file_system("hfs/hfsplus.raw")?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let capabilities = file_entry.open_source()?.unwrap().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }
}
