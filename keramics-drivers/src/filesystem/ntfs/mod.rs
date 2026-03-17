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

const NTFS_FILE_SYSTEM_SIGNATURE: &[u8; 8] = b"NTFS    ";
const NTFS_MFT_ENTRY_SIGNATURE: &[u8; 4] = b"FILE";
const NTFS_BAD_MFT_ENTRY_SIGNATURE: &[u8; 4] = b"BAAD";
const NTFS_INDEX_ENTRY_SIGNATURE: &[u8; 4] = b"INDX";

const NTFS_ATTRIBUTE_TYPE_DATA: u32 = 0x0000_0080;
const NTFS_ATTRIBUTE_TYPE_FILE_NAME: u32 = 0x0000_0030;
const NTFS_ATTRIBUTE_TYPE_INDEX_ROOT: u32 = 0x0000_0090;
const NTFS_ATTRIBUTE_TYPE_INDEX_ALLOCATION: u32 = 0x0000_00a0;

const NTFS_ROOT_DIRECTORY_IDENTIFIER: u64 = 5;

const NTFS_INDEX_VALUE_FLAG_IS_BRANCH: u32 = 0x0000_0001;
const NTFS_INDEX_VALUE_FLAG_IS_LAST: u32 = 0x0000_0002;

const NTFS_NAME_SPACE_DOS: u8 = 0x02;

#[derive(Clone)]
struct NtfsRuntime {
    source: DataSourceReference,
    bytes_per_sector: u16,
    cluster_block_size: u32,
    mft_entry_size: u32,
    index_entry_size: u32,
    volume_serial_number: u64,
    mft_source: DataSourceReference,
}

#[derive(Clone)]
struct NtfsBootRecord {
    bytes_per_sector: u16,
    cluster_block_size: u32,
    mft_cluster_block_number: u64,
    mft_entry_size: u32,
    index_entry_size: u32,
    volume_serial_number: u64,
}

#[derive(Clone)]
struct NtfsDataRun {
    logical_cluster_number: u64,
    physical_cluster_number: u64,
    number_of_clusters: u64,
    is_sparse: bool,
}

#[derive(Clone)]
struct NtfsAttribute {
    attribute_type: u32,
    name: Option<String>,
    is_resident: bool,
    data_flags: u16,
    data_size: u64,
    resident_data: Vec<u8>,
    data_runs: Vec<NtfsDataRun>,
}

#[derive(Clone)]
struct NtfsMftEntry {
    base_record_file_reference: u64,
    attributes: Vec<NtfsAttribute>,
    is_directory: bool,
}

#[derive(Clone)]
struct NtfsFileName {
    name_space: u8,
    name: String,
}

#[derive(Clone)]
struct NtfsDirectoryEntry {
    file_reference: u64,
    file_name: NtfsFileName,
}

#[derive(Clone, Copy)]
struct NtfsIndexRootHeader {
    attribute_type: u32,
    collation_type: u32,
}

#[derive(Clone, Copy)]
struct NtfsIndexNodeHeader {
    index_values_offset: u32,
    size: u32,
}

#[derive(Clone, Copy)]
struct NtfsIndexValue {
    file_reference: u64,
    size: u16,
    key_data_size: u16,
    flags: u32,
}

/// Immutable NTFS file entry.
#[derive(Clone)]
pub struct NtfsFileEntry {
    runtime: Arc<NtfsRuntime>,
    entry_number: u64,
    entry: NtfsMftEntry,
    name: Option<String>,
}

/// Immutable NTFS file system.
pub struct NtfsFileSystem {
    runtime: Arc<NtfsRuntime>,
}

impl NtfsFileSystem {
    /// Opens and parses an NTFS file system.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let boot_record = NtfsBootRecord::read_at(source.as_ref())?;
        let mft_entry_offset = boot_record
            .mft_cluster_block_number
            .checked_mul(boot_record.cluster_block_size as u64)
            .ok_or_else(|| ErrorTrace::new("NTFS MFT offset overflow".to_string()))?;
        let entry0 = read_mft_entry_at(
            source.as_ref(),
            boot_record.mft_entry_size,
            mft_entry_offset,
        )?;
        let mft_attribute = find_attribute(&entry0.attributes, None, NTFS_ATTRIBUTE_TYPE_DATA)
            .ok_or_else(|| {
                ErrorTrace::new("Missing NTFS unnamed $DATA attribute for $MFT".to_string())
            })?;
        let mft_source = build_non_resident_attribute_source(
            source.clone(),
            boot_record.cluster_block_size,
            mft_attribute,
        )?;
        let runtime = Arc::new(NtfsRuntime {
            source,
            bytes_per_sector: boot_record.bytes_per_sector,
            cluster_block_size: boot_record.cluster_block_size,
            mft_entry_size: boot_record.mft_entry_size,
            index_entry_size: boot_record.index_entry_size,
            volume_serial_number: boot_record.volume_serial_number,
            mft_source,
        });

        Ok(Self { runtime })
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.runtime.bytes_per_sector
    }

    /// Retrieves the cluster block size.
    pub fn cluster_block_size(&self) -> u32 {
        self.runtime.cluster_block_size
    }

    /// Retrieves the MFT entry size.
    pub fn mft_entry_size(&self) -> u32 {
        self.runtime.mft_entry_size
    }

    /// Retrieves the index entry size.
    pub fn index_entry_size(&self) -> u32 {
        self.runtime.index_entry_size
    }

    /// Retrieves the volume serial number.
    pub fn volume_serial_number(&self) -> u64 {
        self.runtime.volume_serial_number
    }

    /// Retrieves the root directory.
    pub fn root_directory(&self) -> Result<NtfsFileEntry, ErrorTrace> {
        self.file_entry_by_identifier(NTFS_ROOT_DIRECTORY_IDENTIFIER)
    }

    /// Retrieves a file entry by MFT entry number.
    pub fn file_entry_by_identifier(&self, entry_number: u64) -> Result<NtfsFileEntry, ErrorTrace> {
        let entry = read_mft_entry_at(
            self.runtime.mft_source.as_ref(),
            self.runtime.mft_entry_size,
            entry_number
                .checked_mul(self.runtime.mft_entry_size as u64)
                .ok_or_else(|| ErrorTrace::new("NTFS MFT entry offset overflow".to_string()))?,
        )?;

        Ok(NtfsFileEntry {
            runtime: self.runtime.clone(),
            entry_number,
            entry,
            name: None,
        })
    }

    /// Retrieves a file entry by absolute path.
    pub fn file_entry_by_path(&self, path: &str) -> Result<Option<NtfsFileEntry>, ErrorTrace> {
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

impl NtfsFileEntry {
    /// Retrieves the MFT entry number.
    pub fn entry_number(&self) -> u64 {
        self.entry_number
    }

    /// Retrieves the name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Retrieves the size of the nameless default data stream.
    pub fn size(&self) -> u64 {
        find_attribute(&self.entry.attributes, None, NTFS_ATTRIBUTE_TYPE_DATA)
            .map_or(0, |attribute| attribute.data_size)
    }

    /// Determines if the file entry is a directory.
    pub fn is_directory(&self) -> bool {
        self.entry.is_directory
    }

    /// Opens the nameless default data source.
    pub fn open_source(&self) -> Result<Option<DataSourceReference>, ErrorTrace> {
        if self.is_directory() {
            return Ok(None);
        }
        if self.entry.base_record_file_reference != 0 {
            return Err(ErrorTrace::new(format!(
                "Unsupported NTFS base record file reference: {}",
                self.entry.base_record_file_reference,
            )));
        }

        let attribute = match find_attribute(&self.entry.attributes, None, NTFS_ATTRIBUTE_TYPE_DATA)
        {
            Some(attribute) => attribute,
            None => return Ok(None),
        };

        if attribute.is_resident {
            return Ok(Some(Arc::new(MemoryDataSource::new(
                attribute.resident_data.clone(),
            ))));
        }

        Ok(Some(build_non_resident_attribute_source(
            self.runtime.source.clone(),
            self.runtime.cluster_block_size,
            attribute,
        )?))
    }

    fn sub_file_entry_by_name(&self, name: &str) -> Result<Option<NtfsFileEntry>, ErrorTrace> {
        let directory_entries = self.read_directory_entries()?;
        let directory_entry = match directory_entries.get(name) {
            Some(directory_entry) => directory_entry,
            None => return Ok(None),
        };
        let entry_number = directory_entry.file_reference & 0x0000_ffff_ffff_ffff;
        let mut file_entry = self
            .runtime
            .file_system()
            .file_entry_by_identifier(entry_number)?;

        file_entry.name = Some(directory_entry.file_name.name.clone());
        Ok(Some(file_entry))
    }

    fn read_directory_entries(&self) -> Result<BTreeMap<String, NtfsDirectoryEntry>, ErrorTrace> {
        let index_root_attribute = find_attribute(
            &self.entry.attributes,
            Some("$I30"),
            NTFS_ATTRIBUTE_TYPE_INDEX_ROOT,
        )
        .ok_or_else(|| ErrorTrace::new("Missing NTFS $I30 $INDEX_ROOT attribute".to_string()))?;

        if !index_root_attribute.is_resident {
            return Err(ErrorTrace::new(
                "Unsupported non-resident NTFS $INDEX_ROOT attribute".to_string(),
            ));
        }

        let index_root_header = read_index_root_header(&index_root_attribute.resident_data)?;
        if index_root_header.attribute_type != NTFS_ATTRIBUTE_TYPE_FILE_NAME {
            return Err(ErrorTrace::new(format!(
                "Unsupported NTFS $INDEX_ROOT attribute type: 0x{:08x}",
                index_root_header.attribute_type,
            )));
        }
        if index_root_header.collation_type != 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported NTFS $INDEX_ROOT collation type: {}",
                index_root_header.collation_type,
            )));
        }

        let allocation_source = match find_attribute(
            &self.entry.attributes,
            Some("$I30"),
            NTFS_ATTRIBUTE_TYPE_INDEX_ALLOCATION,
        ) {
            Some(index_allocation_attribute) => Some(build_non_resident_attribute_source(
                self.runtime.source.clone(),
                self.runtime.cluster_block_size,
                index_allocation_attribute,
            )?),
            None => None,
        };
        let mut entries = BTreeMap::new();

        read_index_entries_from_node(
            &index_root_attribute.resident_data,
            16,
            self.runtime.index_entry_size,
            allocation_source.as_ref(),
            &mut entries,
        )?;

        Ok(entries)
    }
}

trait NtfsRuntimeAccess {
    fn file_system(&self) -> NtfsFileSystem;
}

impl NtfsRuntimeAccess for Arc<NtfsRuntime> {
    fn file_system(&self) -> NtfsFileSystem {
        NtfsFileSystem {
            runtime: self.clone(),
        }
    }
}

impl NtfsBootRecord {
    fn read_at(source: &dyn DataSource) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(0, &mut data)?;

        if &data[3..11] != NTFS_FILE_SYSTEM_SIGNATURE {
            return Err(ErrorTrace::new("Unsupported NTFS signature".to_string()));
        }

        let bytes_per_sector = read_u16_le(&data, 11)?;
        if ![256, 512, 1024, 2048, 4096].contains(&bytes_per_sector) {
            return Err(ErrorTrace::new(format!(
                "Unsupported NTFS bytes per sector: {}",
                bytes_per_sector,
            )));
        }

        let sectors_per_cluster = data[13] as u32;
        let cluster_block_size = if sectors_per_cluster <= 128 {
            sectors_per_cluster
                .checked_mul(bytes_per_sector as u32)
                .ok_or_else(|| ErrorTrace::new("NTFS cluster block size overflow".to_string()))?
        } else {
            let exponent = 256 - sectors_per_cluster;

            if exponent > 12 {
                return Err(ErrorTrace::new(format!(
                    "Unsupported NTFS sectors per cluster: {}",
                    sectors_per_cluster,
                )));
            }
            1u32 << exponent
        };

        let mft_entry_size_descriptor = read_u32_le(&data, 64)?;
        let mft_entry_size =
            decode_record_size(mft_entry_size_descriptor, cluster_block_size, 42, 65535)?;
        let index_entry_size_descriptor = read_u32_le(&data, 68)?;
        let index_entry_size = decode_record_size(
            index_entry_size_descriptor,
            cluster_block_size,
            32,
            16_777_216,
        )?;

        Ok(Self {
            bytes_per_sector,
            cluster_block_size,
            mft_cluster_block_number: read_u64_le(&data, 48)?,
            mft_entry_size,
            index_entry_size,
            volume_serial_number: read_u64_le(&data, 72)?,
        })
    }
}

fn decode_record_size(
    descriptor: u32,
    cluster_block_size: u32,
    minimum_size: u32,
    maximum_size: u32,
) -> Result<u32, ErrorTrace> {
    if descriptor == 0 || descriptor > 255 {
        return Err(ErrorTrace::new(format!(
            "Unsupported NTFS record size descriptor: {}",
            descriptor,
        )));
    }

    let size = if descriptor < 128 {
        descriptor
            .checked_mul(cluster_block_size)
            .ok_or_else(|| ErrorTrace::new("NTFS record size overflow".to_string()))?
    } else {
        let exponent = 256 - descriptor;
        if exponent > 32 {
            return Err(ErrorTrace::new(format!(
                "Unsupported NTFS record size descriptor: {}",
                descriptor,
            )));
        }
        1u32 << exponent
    };

    if size < minimum_size || size > maximum_size {
        return Err(ErrorTrace::new(format!(
            "Unsupported NTFS record size: {} value out of bounds",
            size,
        )));
    }

    Ok(size)
}

fn build_non_resident_attribute_source(
    source: DataSourceReference,
    cluster_block_size: u32,
    attribute: &NtfsAttribute,
) -> Result<DataSourceReference, ErrorTrace> {
    if attribute.is_resident {
        return Err(ErrorTrace::new(
            "Resident NTFS attribute cannot be opened as a non-resident source".to_string(),
        ));
    }
    if attribute.data_flags & 0x00ff != 0 {
        return Err(ErrorTrace::new(
            "Compressed NTFS attributes are not supported yet in keramics-drivers".to_string(),
        ));
    }

    let mut extents = Vec::new();
    let mut current_extent: Option<ExtentMapEntry> = None;

    for data_run in attribute.data_runs.iter() {
        let logical_offset = data_run
            .logical_cluster_number
            .checked_mul(cluster_block_size as u64)
            .ok_or_else(|| ErrorTrace::new("NTFS logical run offset overflow".to_string()))?;
        let size = min(
            data_run.number_of_clusters * cluster_block_size as u64,
            attribute.data_size.saturating_sub(logical_offset),
        );

        if size == 0 {
            continue;
        }

        let next_extent = ExtentMapEntry {
            logical_offset,
            size,
            target: if data_run.is_sparse {
                ExtentMapTarget::Zero
            } else {
                ExtentMapTarget::Data {
                    source: source.clone(),
                    source_offset: data_run
                        .physical_cluster_number
                        .checked_mul(cluster_block_size as u64)
                        .ok_or_else(|| {
                            ErrorTrace::new("NTFS physical run offset overflow".to_string())
                        })?,
                }
            },
        };

        current_extent = merge_extent(current_extent, next_extent, &mut extents);
    }

    if let Some(current_extent) = current_extent {
        extents.push(current_extent);
    }

    Ok(Arc::new(ExtentMapDataSource::new(extents)?))
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

fn read_mft_entry_at(
    source: &dyn DataSource,
    entry_size: u32,
    offset: u64,
) -> Result<NtfsMftEntry, ErrorTrace> {
    if !(42..=65535).contains(&entry_size) {
        return Err(ErrorTrace::new(format!(
            "Unsupported NTFS MFT entry size: {} value out of bounds",
            entry_size,
        )));
    }

    let mut data = vec![0u8; entry_size as usize];
    source.read_exact_at(offset, &mut data)?;

    read_mft_entry_data(&mut data)
}

fn read_mft_entry_data(data: &mut [u8]) -> Result<NtfsMftEntry, ErrorTrace> {
    if data[0..4] == [0; 4] {
        return Err(ErrorTrace::new(
            "Unsupported empty NTFS MFT entry".to_string(),
        ));
    }
    if &data[0..4] == NTFS_BAD_MFT_ENTRY_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported bad NTFS MFT entry".to_string(),
        ));
    }
    if &data[0..4] != NTFS_MFT_ENTRY_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported NTFS MFT entry signature".to_string(),
        ));
    }

    let fixup_values_offset = read_u16_le(data, 4)? as usize;
    let number_of_fixup_values = read_u16_le(data, 6)?;
    let _sequence_number = read_u16_le(data, 16)?;
    let attributes_offset = read_u16_le(data, 20)? as usize;
    let flags = read_u16_le(data, 22)?;
    let base_record_file_reference = read_u64_le(data, 32)?;
    let data_size = data.len();

    if fixup_values_offset < 42 || fixup_values_offset > data_size {
        return Err(ErrorTrace::new(format!(
            "Invalid NTFS fix-up values offset: {} value out of bounds",
            fixup_values_offset,
        )));
    }
    if attributes_offset < 42 || attributes_offset > data_size {
        return Err(ErrorTrace::new(format!(
            "Invalid NTFS attributes offset: {} value out of bounds",
            attributes_offset,
        )));
    }
    if fixup_values_offset >= attributes_offset {
        return Err(ErrorTrace::new(format!(
            "NTFS fix-up values offset: {} exceeds attributes offset: {}",
            fixup_values_offset, attributes_offset,
        )));
    }

    apply_fixup_values(data, fixup_values_offset, number_of_fixup_values)?;

    Ok(NtfsMftEntry {
        base_record_file_reference,
        attributes: read_attributes(data, attributes_offset)?,
        is_directory: flags & 0x0002 != 0,
    })
}

fn read_attributes(data: &[u8], mut data_offset: usize) -> Result<Vec<NtfsAttribute>, ErrorTrace> {
    let mut attributes = Vec::new();

    loop {
        if data_offset > data.len().saturating_sub(4) {
            return Err(ErrorTrace::new(format!(
                "Invalid NTFS attribute offset: {} value out of bounds",
                data_offset,
            )));
        }
        if data[data_offset..data_offset + 4] == [0xff; 4] {
            break;
        }

        let attribute = read_attribute(&data[data_offset..])?;

        data_offset += attribute.0;
        attributes.push(attribute.1);
    }

    Ok(attributes)
}

fn read_attribute(data: &[u8]) -> Result<(usize, NtfsAttribute), ErrorTrace> {
    if data.len() < 16 {
        return Err(ErrorTrace::new(
            "Unsupported NTFS attribute size".to_string(),
        ));
    }

    let attribute_type = read_u32_le(data, 0)?;
    let attribute_size = read_u32_le(data, 4)? as usize;
    let non_resident_flag = data[8];
    let name_size = data[9] as usize;
    let name_offset = read_u16_le(data, 10)? as usize;
    let data_flags = read_u16_le(data, 12)?;

    if attribute_size < 16 || attribute_size > data.len() {
        return Err(ErrorTrace::new(format!(
            "Unsupported NTFS attribute size: {}",
            attribute_size,
        )));
    }

    let name = if name_size > 0 {
        let name_data_size = name_size
            .checked_mul(2)
            .ok_or_else(|| ErrorTrace::new("NTFS attribute name size overflow".to_string()))?;
        let name_end_offset = name_offset.checked_add(name_data_size).ok_or_else(|| {
            ErrorTrace::new("NTFS attribute name end offset overflow".to_string())
        })?;

        if name_offset < 16 || name_end_offset > attribute_size {
            return Err(ErrorTrace::new(format!(
                "Invalid NTFS attribute name offset: {} value out of bounds",
                name_offset,
            )));
        }

        Some(decode_utf16_le(&data[name_offset..name_end_offset])?)
    } else {
        None
    };

    if non_resident_flag == 0 {
        let resident_data_size = read_u32_le(data, 16)? as usize;
        let resident_data_offset = read_u16_le(data, 20)? as usize;
        let resident_data_end_offset = resident_data_offset
            .checked_add(resident_data_size)
            .ok_or_else(|| ErrorTrace::new("NTFS resident data end offset overflow".to_string()))?;

        if resident_data_offset < 24 || resident_data_end_offset > attribute_size {
            return Err(ErrorTrace::new(format!(
                "Invalid NTFS resident data offset: {} value out of bounds",
                resident_data_offset,
            )));
        }

        Ok((
            attribute_size,
            NtfsAttribute {
                attribute_type,
                name,
                is_resident: true,
                data_flags,
                data_size: resident_data_size as u64,
                resident_data: data[resident_data_offset..resident_data_end_offset].to_vec(),
                data_runs: Vec::new(),
            },
        ))
    } else {
        if attribute_size < 64 {
            return Err(ErrorTrace::new(
                "Unsupported NTFS non-resident attribute size".to_string(),
            ));
        }
        let data_first_vcn = read_u64_le(data, 16)?;
        let data_runs_offset = read_u16_le(data, 32)? as usize;
        let compression_unit_size = read_u16_le(data, 34)?;
        let data_size = read_u64_le(data, 48)?;

        if compression_unit_size > 0 {
            return Err(ErrorTrace::new(
                "Compressed NTFS attributes are not supported yet in keramics-drivers".to_string(),
            ));
        }
        if data_runs_offset < 64 || data_runs_offset > attribute_size {
            return Err(ErrorTrace::new(format!(
                "Invalid NTFS data runs offset: {} value out of bounds",
                data_runs_offset,
            )));
        }

        Ok((
            attribute_size,
            NtfsAttribute {
                attribute_type,
                name,
                is_resident: false,
                data_flags,
                data_size,
                resident_data: Vec::new(),
                data_runs: read_data_runs(data, data_first_vcn, data_runs_offset)?,
            },
        ))
    }
}

fn read_data_runs(
    data: &[u8],
    first_vcn: u64,
    data_runs_offset: usize,
) -> Result<Vec<NtfsDataRun>, ErrorTrace> {
    let mut data_offset = data_runs_offset;
    let mut last_physical_cluster_number: i64 = 0;
    let mut logical_cluster_number = first_vcn;
    let mut data_runs = Vec::new();

    while data_offset < data.len() {
        let size_tuple = data[data_offset] as usize;
        let number_of_clusters_size = size_tuple & 0x0f;
        let physical_cluster_number_size = size_tuple >> 4;

        if number_of_clusters_size == 0 {
            break;
        }

        let data_run_size = 1usize
            .checked_add(number_of_clusters_size)
            .and_then(|value| value.checked_add(physical_cluster_number_size))
            .ok_or_else(|| ErrorTrace::new("NTFS data run size overflow".to_string()))?;

        if data_offset + data_run_size > data.len() {
            return Err(ErrorTrace::new(format!(
                "Unsupported NTFS data run size: {}",
                data_run_size,
            )));
        }
        if number_of_clusters_size > 8 || physical_cluster_number_size > 8 {
            return Err(ErrorTrace::new(
                "Unsupported NTFS data run component size".to_string(),
            ));
        }

        let mut number_of_clusters: u64 = 0;
        for byte_value in data[data_offset + 1..data_offset + 1 + number_of_clusters_size]
            .iter()
            .rev()
        {
            number_of_clusters <<= 8;
            number_of_clusters |= *byte_value as u64;
        }

        let (physical_cluster_number, is_sparse) = if physical_cluster_number_size == 0 {
            (0u64, true)
        } else {
            let mut relative_cluster_number: i64 =
                if data[data_offset + data_run_size - 1] & 0x80 != 0 {
                    -1
                } else {
                    0
                };

            for byte_value in data
                [data_offset + 1 + number_of_clusters_size..data_offset + data_run_size]
                .iter()
                .rev()
            {
                relative_cluster_number <<= 8;
                relative_cluster_number |= *byte_value as i64;
            }
            last_physical_cluster_number += relative_cluster_number;

            (last_physical_cluster_number as u64, false)
        };

        data_runs.push(NtfsDataRun {
            logical_cluster_number,
            physical_cluster_number,
            number_of_clusters,
            is_sparse,
        });

        logical_cluster_number += number_of_clusters;
        data_offset += data_run_size;
    }

    Ok(data_runs)
}

fn find_attribute<'attribute>(
    attributes: &'attribute [NtfsAttribute],
    name: Option<&str>,
    attribute_type: u32,
) -> Option<&'attribute NtfsAttribute> {
    attributes.iter().find(|attribute| {
        attribute.attribute_type == attribute_type
            && match (&attribute.name, name) {
                (Some(attribute_name), Some(name)) => attribute_name == name,
                (None, None) => true,
                _ => false,
            }
    })
}

fn decode_utf16_le(data: &[u8]) -> Result<String, ErrorTrace> {
    if !data.len().is_multiple_of(2) {
        return Err(ErrorTrace::new(
            "NTFS UTF-16LE data size is not aligned to 2 bytes".to_string(),
        ));
    }

    let mut code_units = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        code_units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }

    String::from_utf16(&code_units).map_err(|error| {
        ErrorTrace::new(format!(
            "Unable to decode NTFS UTF-16LE string with error: {}",
            error,
        ))
    })
}

fn read_index_root_header(data: &[u8]) -> Result<NtfsIndexRootHeader, ErrorTrace> {
    if data.len() < 32 {
        return Err(ErrorTrace::new(
            "Unsupported NTFS index root size".to_string(),
        ));
    }

    Ok(NtfsIndexRootHeader {
        attribute_type: read_u32_le(data, 0)?,
        collation_type: read_u32_le(data, 4)?,
    })
}

fn read_index_node_header(data: &[u8], offset: usize) -> Result<NtfsIndexNodeHeader, ErrorTrace> {
    if data.len() < offset + 16 {
        return Err(ErrorTrace::new(
            "Unsupported NTFS index node header size".to_string(),
        ));
    }

    let index_values_offset = read_u32_le(data, offset)?;
    let size = read_u32_le(data, offset + 4)?;

    if index_values_offset < 16 || index_values_offset >= size {
        return Err(ErrorTrace::new(format!(
            "Invalid NTFS index values offset: {} value out of bounds",
            index_values_offset,
        )));
    }

    Ok(NtfsIndexNodeHeader {
        index_values_offset,
        size,
    })
}

fn read_index_value(
    data: &[u8],
    offset: usize,
    end_offset: usize,
) -> Result<NtfsIndexValue, ErrorTrace> {
    if offset + 16 > end_offset {
        return Err(ErrorTrace::new(
            "Invalid NTFS index value offset value out of bounds".to_string(),
        ));
    }

    let size = read_u16_le(data, offset + 8)?;
    if size < 16 || offset + size as usize > end_offset {
        return Err(ErrorTrace::new(format!(
            "Invalid NTFS index value size: {} value out of bounds",
            size,
        )));
    }

    Ok(NtfsIndexValue {
        file_reference: read_u64_le(data, offset)?,
        size,
        key_data_size: read_u16_le(data, offset + 10)?,
        flags: read_u32_le(data, offset + 12)?,
    })
}

fn read_file_name(data: &[u8]) -> Result<NtfsFileName, ErrorTrace> {
    if data.len() < 66 {
        return Err(ErrorTrace::new(
            "Unsupported NTFS file name data size".to_string(),
        ));
    }

    let name_size = data[64] as usize;
    let name_end_offset = 66usize
        .checked_add(
            name_size
                .checked_mul(2)
                .ok_or_else(|| ErrorTrace::new("NTFS file name size overflow".to_string()))?,
        )
        .ok_or_else(|| ErrorTrace::new("NTFS file name end offset overflow".to_string()))?;

    if name_end_offset > data.len() {
        return Err(ErrorTrace::new(
            "Unsupported NTFS file name data size".to_string(),
        ));
    }

    Ok(NtfsFileName {
        name_space: data[65],
        name: decode_utf16_le(&data[66..name_end_offset])?,
    })
}

fn read_index_entries_from_node(
    data: &[u8],
    index_node_offset: usize,
    index_entry_size: u32,
    allocation_source: Option<&DataSourceReference>,
    entries: &mut BTreeMap<String, NtfsDirectoryEntry>,
) -> Result<(), ErrorTrace> {
    let index_node_header = read_index_node_header(data, index_node_offset)?;
    let mut index_value_offset = index_node_offset + index_node_header.index_values_offset as usize;
    let index_values_end_offset = index_node_offset + index_node_header.size as usize;

    while index_value_offset < index_values_end_offset {
        let index_value = read_index_value(data, index_value_offset, index_values_end_offset)?;
        let key_data_size = index_value.key_data_size as usize;
        let key_data_offset = index_value_offset + 16;
        let key_data_end_offset = key_data_offset
            .checked_add(key_data_size)
            .ok_or_else(|| ErrorTrace::new("NTFS index key end offset overflow".to_string()))?;

        if key_data_end_offset > index_values_end_offset {
            return Err(ErrorTrace::new(format!(
                "Invalid NTFS index key data size: {} value out of bounds",
                key_data_size,
            )));
        }

        let value_data_size = index_value.size as usize - 16 - key_data_size;
        let value_data_offset = key_data_end_offset;
        let value_data_end_offset =
            value_data_offset
                .checked_add(value_data_size)
                .ok_or_else(|| {
                    ErrorTrace::new("NTFS index value data end offset overflow".to_string())
                })?;

        if value_data_end_offset > index_values_end_offset {
            return Err(ErrorTrace::new(
                "Invalid NTFS index value data size value out of bounds".to_string(),
            ));
        }

        if index_value.key_data_size > 0 && index_value.flags & NTFS_INDEX_VALUE_FLAG_IS_LAST == 0 {
            let file_name = read_file_name(&data[key_data_offset..key_data_end_offset])?;

            if file_name.name_space != NTFS_NAME_SPACE_DOS && file_name.name != "." {
                entries.insert(
                    file_name.name.clone(),
                    NtfsDirectoryEntry {
                        file_reference: index_value.file_reference,
                        file_name,
                    },
                );
            }
        }

        if index_value.flags & NTFS_INDEX_VALUE_FLAG_IS_BRANCH != 0 {
            if value_data_size < 8 {
                return Err(ErrorTrace::new(format!(
                    "Invalid NTFS index branch data size: {} value out of bounds",
                    value_data_size,
                )));
            }

            let allocation_source = allocation_source.ok_or_else(|| {
                ErrorTrace::new("Missing NTFS index allocation source for branch entry".to_string())
            })?;
            let sub_node_vcn = read_u64_le(data, value_data_end_offset - 8)?;
            let mut index_entry_data = vec![0u8; index_entry_size as usize];

            allocation_source.read_exact_at(
                sub_node_vcn
                    .checked_mul(index_entry_size as u64)
                    .ok_or_else(|| {
                        ErrorTrace::new("NTFS index entry offset overflow".to_string())
                    })?,
                &mut index_entry_data,
            )?;

            if &index_entry_data[0..4] != NTFS_INDEX_ENTRY_SIGNATURE {
                return Err(ErrorTrace::new(
                    "Unsupported NTFS index entry signature".to_string(),
                ));
            }

            let fixup_values_offset = read_u16_le(&index_entry_data, 4)? as usize;
            let number_of_fixup_values = read_u16_le(&index_entry_data, 6)?;
            apply_fixup_values(
                &mut index_entry_data,
                fixup_values_offset,
                number_of_fixup_values,
            )?;
            read_index_entries_from_node(
                &index_entry_data,
                24,
                index_entry_size,
                allocation_source.into(),
                entries,
            )?;
        }

        index_value_offset += index_value.size as usize;
        let alignment_padding = (8 - (index_value_offset % 8)) % 8;
        index_value_offset += alignment_padding;

        if index_value.flags & NTFS_INDEX_VALUE_FLAG_IS_LAST != 0 {
            break;
        }
    }

    Ok(())
}

fn apply_fixup_values(
    buffer: &mut [u8],
    fixup_values_offset: usize,
    number_of_fixup_values: u16,
) -> Result<(), ErrorTrace> {
    if fixup_values_offset >= buffer.len() {
        return Err(ErrorTrace::new(format!(
            "Invalid NTFS fix-up values offset: {} value out of bounds",
            fixup_values_offset,
        )));
    }

    let fixup_values_size = 2usize
        .checked_add((number_of_fixup_values as usize) * 2)
        .ok_or_else(|| ErrorTrace::new("NTFS fix-up values size overflow".to_string()))?;
    let fixup_values_end_offset = fixup_values_offset
        .checked_add(fixup_values_size)
        .ok_or_else(|| ErrorTrace::new("NTFS fix-up values end offset overflow".to_string()))?;

    if fixup_values_end_offset > buffer.len() {
        return Err(ErrorTrace::new(format!(
            "Invalid NTFS number of fix-up values: {} value out of bounds",
            number_of_fixup_values,
        )));
    }

    let placeholder_value = [buffer[fixup_values_offset], buffer[fixup_values_offset + 1]];
    let mut fixup_value_offset = fixup_values_offset + 2;
    let mut buffer_offset: usize = 510;

    for _ in 1..number_of_fixup_values {
        let fixup_value_end_offset = fixup_value_offset + 2;
        let buffer_end_offset = buffer_offset + 2;

        if buffer_end_offset <= buffer.len() {
            if buffer[buffer_offset..buffer_end_offset] != placeholder_value {
                return Err(ErrorTrace::new(format!(
                    "NTFS corruption detected at fix-up value offset: {}",
                    fixup_value_offset,
                )));
            }
            buffer.copy_within(fixup_value_offset..fixup_value_end_offset, buffer_offset);
        }

        fixup_value_offset = fixup_value_end_offset;
        buffer_offset += 512;
    }

    Ok(())
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, ErrorTrace> {
    Ok(u16::from_le_bytes(
        data[offset..offset + 2]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read NTFS u16 value".to_string()))?,
    ))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, ErrorTrace> {
    Ok(u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read NTFS u32 value".to_string()))?,
    ))
}

fn read_u64_le(data: &[u8], offset: usize) -> Result<u64, ErrorTrace> {
    Ok(u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read NTFS u64 value".to_string()))?,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn open_file_system() -> Result<NtfsFileSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("ntfs/ntfs.raw"));
        let source = open_local_data_source(&path)?;

        NtfsFileSystem::open(source)
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;

        assert_eq!(file_system.bytes_per_sector(), 512);
        assert_eq!(file_system.cluster_block_size(), 4096);
        assert_eq!(file_system.mft_entry_size(), 1024);
        assert_eq!(file_system.index_entry_size(), 4096);
        assert_eq!(file_system.volume_serial_number(), 0x39fc_0da2_5d08_5bcb);
        Ok(())
    }

    #[test]
    fn test_root_directory() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let root_directory = file_system.root_directory()?;

        assert_eq!(root_directory.entry_number(), 5);
        assert!(root_directory.is_directory());
        Ok(())
    }

    #[test]
    fn test_file_entry_by_identifier() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system.file_entry_by_identifier(64)?;

        assert_eq!(file_entry.entry_number(), 64);
        assert_eq!(file_entry.size(), 0);
        Ok(())
    }

    #[test]
    fn test_file_entry_by_path() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();

        assert_eq!(file_entry.entry_number(), 64);
        assert_eq!(file_entry.name(), Some("emptyfile"));
        assert!(!file_entry.is_directory());
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let capabilities = file_entry.open_source()?.unwrap().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn read_ntfs_empty_file() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 0);
        assert_eq!(md5_hash.as_str(), "d41d8cd98f00b204e9800998ecf8427e");
        Ok(())
    }
}
