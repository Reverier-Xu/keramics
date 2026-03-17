/* Copyright 2024-2026 Joachim Metz <joachim.metz@gmail.com>
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
use std::collections::HashMap;
use std::sync::Arc;

use keramics_checksums::ReversedCrc32Context;
use keramics_core::ErrorTrace;
use keramics_types::Uuid;

use crate::VhdDiskType;
use crate::source::{DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};

const VHDX_FILE_HEADER_SIGNATURE: &[u8; 8] = b"vhdxfile";
const VHDX_IMAGE_HEADER_SIGNATURE: &[u8; 4] = b"head";
const VHDX_REGION_TABLE_HEADER_SIGNATURE: &[u8; 4] = b"regi";
const VHDX_METADATA_TABLE_HEADER_SIGNATURE: &[u8; 8] = b"metadata";
const VHDX_PARENT_LOCATOR_TYPE_INDICATOR: [u8; 16] = [
    0xb7, 0xef, 0x4a, 0xb0, 0x9e, 0xd1, 0x81, 0x4a, 0xb7, 0x89, 0x25, 0xb8, 0xe9, 0x44, 0x59, 0x13,
];

const VHDX_BLOCK_ALLOCATION_TABLE_REGION_IDENTIFIER: Uuid = Uuid {
    part1: 0x2dc27766,
    part2: 0xf623,
    part3: 0x4200,
    part4: 0x9d64,
    part5: 0x115e9bfd4a08,
};
const VHDX_METADATA_REGION_IDENTIFIER: Uuid = Uuid {
    part1: 0x8b7ca206,
    part2: 0x4790,
    part3: 0x4b9a,
    part4: 0xb8fe,
    part5: 0x575f050f886e,
};
const VHDX_VIRTUAL_DISK_SIZE_METADATA_IDENTIFIER: Uuid = Uuid {
    part1: 0x2fa54224,
    part2: 0xcd1b,
    part3: 0x4876,
    part4: 0xb211,
    part5: 0x5dbed83bf4b8,
};
const VHDX_LOGICAL_SECTOR_SIZE_METADATA_IDENTIFIER: Uuid = Uuid {
    part1: 0x8141bf1d,
    part2: 0xa96f,
    part3: 0x4709,
    part4: 0xba47,
    part5: 0xf233a8faab5f,
};
const VHDX_PARENT_LOCATOR_METADATA_IDENTIFIER: Uuid = Uuid {
    part1: 0xa8d35f2d,
    part2: 0xb30b,
    part3: 0x454d,
    part4: 0xabf7,
    part5: 0xd3d84834ab0c,
};
const VHDX_FILE_PARAMETERS_METADATA_IDENTIFIER: Uuid = Uuid {
    part1: 0xcaa16737,
    part2: 0xfa36,
    part3: 0x4d43,
    part4: 0xb3b6,
    part5: 0x33f0aa44e76b,
};
const VHDX_PHYSICAL_SECTOR_SIZE_METADATA_IDENTIFIER: Uuid = Uuid {
    part1: 0xcda348c7,
    part2: 0x445d,
    part3: 0x4471,
    part4: 0x9cc9,
    part5: 0xe9885251c556,
};
const VHDX_VIRTUAL_DISK_IDENTIFIER_METADATA_IDENTIFIER: Uuid = Uuid {
    part1: 0xbeca12ab,
    part2: 0xb2e6,
    part3: 0x4523,
    part4: 0x93ef,
    part5: 0xc309e000c746,
};

#[derive(Clone)]
struct VhdxImageHeader {
    sequence_number: u64,
    data_write_identifier: Uuid,
    format_version: u16,
}

#[derive(Clone)]
struct VhdxRegionTableEntry {
    data_offset: u64,
    data_size: u32,
}

#[derive(Clone)]
struct VhdxMetadataTableEntry {
    item_offset: u32,
    item_size: u32,
}

#[derive(Clone)]
struct VhdxMetadataValues {
    disk_type: VhdDiskType,
    block_size: u32,
    bytes_per_sector: u16,
    media_size: u64,
    parent_identifier: Option<Uuid>,
    parent_name: Option<String>,
}

#[derive(Clone, Copy)]
struct VhdxBatEntry {
    block_state: u8,
    block_offset: u64,
}

struct VhdxSectorBitmapRange {
    size: u64,
    is_set: bool,
}

/// Immutable VHDX file metadata plus opened logical source.
pub struct VhdxFile {
    format_version: u16,
    disk_type: VhdDiskType,
    identifier: Uuid,
    parent_identifier: Option<Uuid>,
    parent_name: Option<String>,
    bytes_per_sector: u16,
    block_size: u32,
    media_size: u64,
    logical_source: DataSourceReference,
}

impl VhdxFile {
    /// Opens and parses a VHDX file.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        Self::open_internal(source, None)
    }

    /// Opens and parses a differential VHDX file with an explicit parent file.
    pub fn open_with_parent(
        source: DataSourceReference,
        parent_file: &VhdxFile,
    ) -> Result<Self, ErrorTrace> {
        Self::open_internal(source, Some(parent_file))
    }

    fn open_internal(
        source: DataSourceReference,
        parent_file: Option<&VhdxFile>,
    ) -> Result<Self, ErrorTrace> {
        validate_file_header(source.as_ref())?;
        let active_image_header = read_active_image_header(source.as_ref())?;
        let region_table = read_region_table(source.as_ref())?;
        let metadata_region = region_table
            .get(&VHDX_METADATA_REGION_IDENTIFIER)
            .ok_or_else(|| ErrorTrace::new("Missing VHDX metadata region".to_string()))?;
        let bat_region = region_table
            .get(&VHDX_BLOCK_ALLOCATION_TABLE_REGION_IDENTIFIER)
            .ok_or_else(|| {
                ErrorTrace::new("Missing VHDX block allocation table region".to_string())
            })?;
        let metadata_values = read_metadata_values(source.as_ref(), metadata_region)?;
        let parent_source = if metadata_values.disk_type == VhdDiskType::Differential {
            let parent_file = parent_file.ok_or_else(|| {
                ErrorTrace::new(
                    "Differential VHDX files require an explicit parent file".to_string(),
                )
            })?;
            let parent_identifier =
                metadata_values.parent_identifier.as_ref().ok_or_else(|| {
                    ErrorTrace::new(
                        "Differential VHDX file is missing a parent identifier".to_string(),
                    )
                })?;

            if parent_file.identifier() != parent_identifier {
                return Err(ErrorTrace::new(format!(
                    "Parent identifier: {} does not match identifier of parent file: {}",
                    parent_identifier,
                    parent_file.identifier(),
                )));
            }
            Some(parent_file.open_source())
        } else {
            None
        };

        let extents = build_extents(
            source.clone(),
            metadata_values.disk_type,
            metadata_values.block_size,
            metadata_values.bytes_per_sector,
            metadata_values.media_size,
            bat_region,
            parent_source,
        )?;
        let logical_source: DataSourceReference = Arc::new(ExtentMapDataSource::new(extents)?);

        Ok(Self {
            format_version: active_image_header.format_version,
            disk_type: metadata_values.disk_type,
            identifier: active_image_header.data_write_identifier,
            parent_identifier: metadata_values.parent_identifier,
            parent_name: metadata_values.parent_name,
            bytes_per_sector: metadata_values.bytes_per_sector,
            block_size: metadata_values.block_size,
            media_size: metadata_values.media_size,
            logical_source,
        })
    }

    /// Retrieves the format version.
    pub fn format_version(&self) -> u16 {
        self.format_version
    }

    /// Retrieves the disk type.
    pub fn disk_type(&self) -> VhdDiskType {
        self.disk_type
    }

    /// Retrieves the identifier.
    pub fn identifier(&self) -> &Uuid {
        &self.identifier
    }

    /// Retrieves the parent identifier if present.
    pub fn parent_identifier(&self) -> Option<&Uuid> {
        self.parent_identifier.as_ref()
    }

    /// Retrieves the parent file name if present.
    pub fn parent_name(&self) -> Option<&str> {
        self.parent_name.as_deref()
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the block size in bytes.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Retrieves the media size in bytes.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Opens the logical media source.
    pub fn open_source(&self) -> DataSourceReference {
        self.logical_source.clone()
    }
}

fn validate_file_header(source: &dyn crate::source::DataSource) -> Result<(), ErrorTrace> {
    let mut data = [0u8; 65_536];

    source.read_exact_at(0, &mut data)?;

    if &data[0..8] != VHDX_FILE_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported VHDX file header signature".to_string(),
        ));
    }

    Ok(())
}

fn read_active_image_header(
    source: &dyn crate::source::DataSource,
) -> Result<VhdxImageHeader, ErrorTrace> {
    let primary = read_image_header(source, 65_536);
    let secondary = read_image_header(source, 2 * 65_536);

    match (primary, secondary) {
        (Ok(primary), Ok(secondary)) => {
            if primary.sequence_number > secondary.sequence_number {
                Ok(primary)
            } else {
                Ok(secondary)
            }
        }
        (Ok(primary), Err(_)) => Ok(primary),
        (Err(_), Ok(secondary)) => Ok(secondary),
        (Err(primary_error), Err(_secondary_error)) => Err(primary_error),
    }
}

fn read_image_header(
    source: &dyn crate::source::DataSource,
    offset: u64,
) -> Result<VhdxImageHeader, ErrorTrace> {
    let mut data = [0u8; 4096];

    source.read_exact_at(offset, &mut data)?;

    if &data[0..4] != VHDX_IMAGE_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported VHDX image header signature".to_string(),
        ));
    }

    let stored_checksum = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let mut crc32 = ReversedCrc32Context::new(0x82f63b78, 0);

    crc32.update(&data[0..4]);
    crc32.update(&[0; 4]);
    crc32.update(&data[8..4096]);

    let calculated_checksum = crc32.finalize();
    if stored_checksum != 0 && stored_checksum != calculated_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} VHDX image header checksums",
            stored_checksum, calculated_checksum,
        )));
    }

    Ok(VhdxImageHeader {
        sequence_number: u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]),
        data_write_identifier: Uuid::from_le_bytes(&data[32..48]),
        format_version: u16::from_le_bytes([data[66], data[67]]),
    })
}

fn read_region_table(
    source: &dyn crate::source::DataSource,
) -> Result<HashMap<Uuid, VhdxRegionTableEntry>, ErrorTrace> {
    match read_region_table_at(source, 3 * 65_536) {
        Ok(entries) => Ok(entries),
        Err(_) => read_region_table_at(source, 4 * 65_536),
    }
}

fn read_region_table_at(
    source: &dyn crate::source::DataSource,
    offset: u64,
) -> Result<HashMap<Uuid, VhdxRegionTableEntry>, ErrorTrace> {
    let mut data = vec![0u8; 65_536];

    source.read_exact_at(offset, &mut data)?;

    if &data[0..4] != VHDX_REGION_TABLE_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported VHDX region table signature".to_string(),
        ));
    }

    let stored_checksum = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let number_of_entries = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

    if number_of_entries > 2047 {
        return Err(ErrorTrace::new(format!(
            "Invalid VHDX region table entry count: {}",
            number_of_entries,
        )));
    }

    let mut crc32 = ReversedCrc32Context::new(0x82f63b78, 0);

    crc32.update(&data[0..4]);
    crc32.update(&[0; 4]);
    crc32.update(&data[8..65_536]);

    let calculated_checksum = crc32.finalize();
    if stored_checksum != 0 && stored_checksum != calculated_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} VHDX region table checksums",
            stored_checksum, calculated_checksum,
        )));
    }

    let mut entries = HashMap::new();
    let mut data_offset: usize = 16;

    for _ in 0..number_of_entries {
        let data_end_offset = data_offset + 32;
        if data_end_offset > data.len() {
            return Err(ErrorTrace::new(format!(
                "Invalid VHDX region table size for number of entries: {}",
                number_of_entries,
            )));
        }

        let type_identifier = Uuid::from_le_bytes(&data[data_offset..data_offset + 16]);
        let data_offset_value = u64::from_le_bytes([
            data[data_offset + 16],
            data[data_offset + 17],
            data[data_offset + 18],
            data[data_offset + 19],
            data[data_offset + 20],
            data[data_offset + 21],
            data[data_offset + 22],
            data[data_offset + 23],
        ]);
        let data_size = u32::from_le_bytes([
            data[data_offset + 24],
            data[data_offset + 25],
            data[data_offset + 26],
            data[data_offset + 27],
        ]);

        entries.insert(
            type_identifier,
            VhdxRegionTableEntry {
                data_offset: data_offset_value,
                data_size,
            },
        );

        data_offset = data_end_offset;
    }

    Ok(entries)
}

fn read_metadata_values(
    source: &dyn crate::source::DataSource,
    metadata_region: &VhdxRegionTableEntry,
) -> Result<VhdxMetadataValues, ErrorTrace> {
    if metadata_region.data_size < 65_536 {
        return Err(ErrorTrace::new(format!(
            "Unsupported VHDX metadata region size: {}",
            metadata_region.data_size,
        )));
    }

    let metadata_entries = read_metadata_table(source, metadata_region)?;
    let file_parameters_entry = metadata_entries
        .get(&VHDX_FILE_PARAMETERS_METADATA_IDENTIFIER)
        .ok_or_else(|| ErrorTrace::new("Missing VHDX file parameters metadata item".to_string()))?;
    if file_parameters_entry.item_size != 8 {
        return Err(ErrorTrace::new(format!(
            "Unsupported VHDX file parameters metadata item size: {}",
            file_parameters_entry.item_size,
        )));
    }

    let file_parameters_offset = metadata_region
        .data_offset
        .checked_add(file_parameters_entry.item_offset as u64)
        .ok_or_else(|| ErrorTrace::new("VHDX file parameters item offset overflow".to_string()))?;
    let block_size = read_u32_le(source, file_parameters_offset)?;
    let file_parameters_flags = read_u32_le(source, file_parameters_offset + 4)?;

    if !(1024 * 1024..=256 * 1024 * 1024).contains(&block_size) {
        return Err(ErrorTrace::new(format!(
            "Invalid VHDX block size: {} value out of bounds",
            block_size,
        )));
    }

    let disk_type = match file_parameters_flags & 0x0000_0003 {
        0 => VhdDiskType::Fixed,
        1 => VhdDiskType::Dynamic,
        2 => VhdDiskType::Differential,
        value => {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHDX disk type flags: 0x{:08x}",
                value,
            )));
        }
    };

    let virtual_disk_size_entry = metadata_entries
        .get(&VHDX_VIRTUAL_DISK_SIZE_METADATA_IDENTIFIER)
        .ok_or_else(|| {
            ErrorTrace::new("Missing VHDX virtual disk size metadata item".to_string())
        })?;
    if virtual_disk_size_entry.item_size != 8 {
        return Err(ErrorTrace::new(format!(
            "Unsupported VHDX virtual disk size metadata item size: {}",
            virtual_disk_size_entry.item_size,
        )));
    }
    let media_size = read_u64_le(
        source,
        metadata_region
            .data_offset
            .checked_add(virtual_disk_size_entry.item_offset as u64)
            .ok_or_else(|| {
                ErrorTrace::new("VHDX virtual disk size item offset overflow".to_string())
            })?,
    )?;

    let logical_sector_size_entry = metadata_entries
        .get(&VHDX_LOGICAL_SECTOR_SIZE_METADATA_IDENTIFIER)
        .ok_or_else(|| {
            ErrorTrace::new("Missing VHDX logical sector size metadata item".to_string())
        })?;
    if logical_sector_size_entry.item_size != 4 {
        return Err(ErrorTrace::new(format!(
            "Unsupported VHDX logical sector size metadata item size: {}",
            logical_sector_size_entry.item_size,
        )));
    }
    let logical_sector_size = read_u32_le(
        source,
        metadata_region
            .data_offset
            .checked_add(logical_sector_size_entry.item_offset as u64)
            .ok_or_else(|| {
                ErrorTrace::new("VHDX logical sector size item offset overflow".to_string())
            })?,
    )?;

    if logical_sector_size != 512 && logical_sector_size != 4096 {
        return Err(ErrorTrace::new(format!(
            "Invalid VHDX logical sector size: {} value out of bounds",
            logical_sector_size,
        )));
    }

    if let Some(physical_sector_size_entry) =
        metadata_entries.get(&VHDX_PHYSICAL_SECTOR_SIZE_METADATA_IDENTIFIER)
    {
        if physical_sector_size_entry.item_size != 4 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHDX physical sector size metadata item size: {}",
                physical_sector_size_entry.item_size,
            )));
        }

        let physical_sector_size = read_u32_le(
            source,
            metadata_region
                .data_offset
                .checked_add(physical_sector_size_entry.item_offset as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("VHDX physical sector size item offset overflow".to_string())
                })?,
        )?;

        if physical_sector_size != 512 && physical_sector_size != 4096 {
            return Err(ErrorTrace::new(format!(
                "Invalid VHDX physical sector size: {} value out of bounds",
                physical_sector_size,
            )));
        }
    }

    if let Some(virtual_disk_identifier_entry) =
        metadata_entries.get(&VHDX_VIRTUAL_DISK_IDENTIFIER_METADATA_IDENTIFIER)
    {
        if virtual_disk_identifier_entry.item_size != 16 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHDX virtual disk identifier metadata item size: {}",
                virtual_disk_identifier_entry.item_size,
            )));
        }
    } else {
        return Err(ErrorTrace::new(
            "Missing VHDX virtual disk identifier metadata item".to_string(),
        ));
    }

    let (parent_identifier, parent_name) = if let Some(parent_locator_entry) =
        metadata_entries.get(&VHDX_PARENT_LOCATOR_METADATA_IDENTIFIER)
    {
        let parent_locator_offset = metadata_region
            .data_offset
            .checked_add(parent_locator_entry.item_offset as u64)
            .ok_or_else(|| {
                ErrorTrace::new("VHDX parent locator item offset overflow".to_string())
            })?;
        read_parent_locator(
            source,
            parent_locator_offset,
            parent_locator_entry.item_size,
        )?
    } else {
        (None, None)
    };

    Ok(VhdxMetadataValues {
        disk_type,
        block_size,
        bytes_per_sector: logical_sector_size as u16,
        media_size,
        parent_identifier,
        parent_name,
    })
}

fn read_metadata_table(
    source: &dyn crate::source::DataSource,
    metadata_region: &VhdxRegionTableEntry,
) -> Result<HashMap<Uuid, VhdxMetadataTableEntry>, ErrorTrace> {
    let mut data = vec![0u8; 65_536];

    source.read_exact_at(metadata_region.data_offset, &mut data)?;

    if &data[0..8] != VHDX_METADATA_TABLE_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported VHDX metadata table signature".to_string(),
        ));
    }

    let number_of_entries = u16::from_le_bytes([data[10], data[11]]) as usize;
    let mut entries = HashMap::new();
    let mut data_offset: usize = 32;

    for _ in 0..number_of_entries {
        let data_end_offset = data_offset + 32;
        if data_end_offset > data.len() {
            return Err(ErrorTrace::new(format!(
                "Invalid VHDX metadata table size for number of entries: {}",
                number_of_entries,
            )));
        }

        let item_identifier = Uuid::from_le_bytes(&data[data_offset..data_offset + 16]);
        let item_offset = u32::from_le_bytes([
            data[data_offset + 16],
            data[data_offset + 17],
            data[data_offset + 18],
            data[data_offset + 19],
        ]);
        let item_size = u32::from_le_bytes([
            data[data_offset + 20],
            data[data_offset + 21],
            data[data_offset + 22],
            data[data_offset + 23],
        ]);

        if item_offset < 65_536 {
            return Err(ErrorTrace::new(format!(
                "Invalid VHDX metadata item offset: 0x{:04x} value out of bounds",
                item_offset,
            )));
        }

        entries.insert(
            item_identifier,
            VhdxMetadataTableEntry {
                item_offset,
                item_size,
            },
        );

        data_offset = data_end_offset;
    }

    Ok(entries)
}

fn read_parent_locator(
    source: &dyn crate::source::DataSource,
    offset: u64,
    data_size: u32,
) -> Result<(Option<Uuid>, Option<String>), ErrorTrace> {
    if !(20..=65_536).contains(&data_size) {
        return Err(ErrorTrace::new(format!(
            "Unsupported VHDX parent locator data size: {} value out of bounds",
            data_size,
        )));
    }

    let mut data = vec![0u8; data_size as usize];
    source.read_exact_at(offset, &mut data)?;

    if data[0..16] != VHDX_PARENT_LOCATOR_TYPE_INDICATOR {
        return Err(ErrorTrace::new(
            "Unsupported VHDX parent locator type indicator".to_string(),
        ));
    }

    let number_of_entries = u16::from_le_bytes([data[18], data[19]]) as usize;
    let mut entries: HashMap<String, String> = HashMap::new();
    let mut data_offset: usize = 20;

    for entry_index in 0..number_of_entries {
        let data_end_offset = data_offset + 12;
        if data_end_offset > data.len() {
            return Err(ErrorTrace::new(format!(
                "Invalid VHDX parent locator size for number of entries: {}",
                number_of_entries,
            )));
        }

        let key_data_offset = u32::from_le_bytes([
            data[data_offset],
            data[data_offset + 1],
            data[data_offset + 2],
            data[data_offset + 3],
        ]);
        let value_data_offset = u32::from_le_bytes([
            data[data_offset + 4],
            data[data_offset + 5],
            data[data_offset + 6],
            data[data_offset + 7],
        ]);
        let key_data_size = u16::from_le_bytes([data[data_offset + 8], data[data_offset + 9]]);
        let value_data_size = u16::from_le_bytes([data[data_offset + 10], data[data_offset + 11]]);

        let key = decode_utf16_le_slice(
            &data,
            key_data_offset,
            key_data_size,
            "VHDX parent locator key",
            entry_index,
        )?;
        let value = decode_utf16_le_slice(
            &data,
            value_data_offset,
            value_data_size,
            "VHDX parent locator value",
            entry_index,
        )?;

        entries.insert(key, value);
        data_offset = data_end_offset;
    }

    let parent_identifier = if let Some(parent_linkage) = entries.get("parent_linkage") {
        Some(Uuid::from_string(parent_linkage.as_str()).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to parse VHDX parent identifier with error: {}",
                error
            ))
        })?)
    } else {
        None
    };
    let parent_name = entries
        .get("absolute_win32_path")
        .cloned()
        .or_else(|| entries.get("volume_path").cloned())
        .or_else(|| entries.get("relative_path").cloned());

    Ok((parent_identifier, parent_name))
}

fn decode_utf16_le_slice(
    data: &[u8],
    offset: u32,
    size: u16,
    value_type: &str,
    entry_index: usize,
) -> Result<String, ErrorTrace> {
    let offset = offset as usize;
    let size = size as usize;
    let end_offset = offset.checked_add(size).ok_or_else(|| {
        ErrorTrace::new(format!(
            "{} offset overflow for entry: {}",
            value_type, entry_index
        ))
    })?;

    if offset < 20 || offset >= data.len() || end_offset > data.len() {
        return Err(ErrorTrace::new(format!(
            "Invalid {} bounds for entry: {}",
            value_type, entry_index,
        )));
    }
    if !size.is_multiple_of(2) {
        return Err(ErrorTrace::new(format!(
            "Invalid {} size: {} for entry: {}",
            value_type, size, entry_index,
        )));
    }

    let units = data[offset..end_offset]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<u16>>();

    String::from_utf16(&units).map_err(|error| {
        ErrorTrace::new(format!(
            "Unable to decode {} for entry: {} with error: {}",
            value_type, entry_index, error,
        ))
    })
}

fn build_extents(
    source: DataSourceReference,
    disk_type: VhdDiskType,
    block_size: u32,
    bytes_per_sector: u16,
    media_size: u64,
    bat_region: &VhdxRegionTableEntry,
    parent_source: Option<DataSourceReference>,
) -> Result<Vec<ExtentMapEntry>, ErrorTrace> {
    let entries_per_chunk = ((1u64 << 23) * (bytes_per_sector as u64)) / (block_size as u64);
    let sector_bitmap_size = 1_048_576 / (entries_per_chunk as u32);
    let number_of_entries = bat_region.data_size / 8;
    let number_of_blocks = media_size.div_ceil(block_size as u64);
    let mut extents = Vec::new();

    if media_size > (number_of_entries as u64) * (block_size as u64) {
        let calculated_number_of_blocks = media_size.div_ceil(block_size as u64);
        return Err(ErrorTrace::new(format!(
            "Number of VHDX blocks: {} in block allocation table too small for virtual disk size: {} ({} blocks)",
            number_of_entries, media_size, calculated_number_of_blocks,
        )));
    }

    for block_number in 0..number_of_blocks {
        let block_media_offset = block_number
            .checked_mul(block_size as u64)
            .ok_or_else(|| ErrorTrace::new("VHDX block media offset overflow".to_string()))?;
        let block_logical_size = min(block_size as u64, media_size - block_media_offset);
        let table_entry_index = if disk_type == VhdDiskType::Fixed {
            block_number
        } else {
            ((block_number / entries_per_chunk) * (entries_per_chunk + 1))
                + (block_number % entries_per_chunk)
        };
        let entry = read_bat_entry(
            source.as_ref(),
            bat_region.data_offset,
            number_of_entries,
            table_entry_index,
        )?;

        if disk_type == VhdDiskType::Differential && entry.block_state != 6 {
            let parent_source = parent_source.as_ref().ok_or_else(|| {
                ErrorTrace::new("Differential VHDX file requires a parent data source".to_string())
            })?;
            build_differential_extents(
                &source,
                bat_region.data_offset,
                number_of_entries,
                parent_source,
                block_number,
                block_media_offset,
                block_logical_size,
                block_size,
                bytes_per_sector,
                entries_per_chunk,
                sector_bitmap_size,
                entry.block_offset,
                &mut extents,
            )?;
        } else {
            extents.push(ExtentMapEntry {
                logical_offset: block_media_offset,
                size: block_logical_size,
                target: if entry.block_state < 6 {
                    ExtentMapTarget::Zero
                } else {
                    ExtentMapTarget::Data {
                        source: source.clone(),
                        source_offset: entry.block_offset,
                    }
                },
            });
        }
    }

    Ok(extents)
}

#[allow(clippy::too_many_arguments)]
fn build_differential_extents(
    source: &DataSourceReference,
    bat_offset: u64,
    number_of_entries: u32,
    parent_source: &DataSourceReference,
    block_number: u64,
    block_media_offset: u64,
    block_logical_size: u64,
    _block_size: u32,
    bytes_per_sector: u16,
    entries_per_chunk: u64,
    sector_bitmap_size: u32,
    block_offset: u64,
    extents: &mut Vec<ExtentMapEntry>,
) -> Result<(), ErrorTrace> {
    let sector_bitmap_bat_index =
        (1 + (block_number / entries_per_chunk)) * (entries_per_chunk + 1) - 1;
    let sector_bitmap_entry = read_bat_entry(
        source.as_ref(),
        bat_offset,
        number_of_entries,
        sector_bitmap_bat_index,
    )?;

    if sector_bitmap_entry.block_state < 6 {
        return Err(ErrorTrace::new(format!(
            "Invalid VHDX sector bitmap block state: {} for block number: {}",
            sector_bitmap_entry.block_state, block_number,
        )));
    }

    let sector_bitmap_offset = sector_bitmap_entry
        .block_offset
        .checked_add((block_number % entries_per_chunk) * sector_bitmap_size as u64)
        .ok_or_else(|| ErrorTrace::new("VHDX sector bitmap offset overflow".to_string()))?;
    let mut sector_bitmap_data = vec![0u8; sector_bitmap_size as usize];

    source
        .as_ref()
        .read_exact_at(sector_bitmap_offset, &mut sector_bitmap_data)?;

    let ranges = read_sector_bitmap_ranges(&sector_bitmap_data, bytes_per_sector);
    let mut range_media_offset = block_media_offset;
    let mut range_data_offset = block_offset;
    let block_end_offset = block_media_offset + block_logical_size;

    for range in ranges {
        if range_media_offset >= block_end_offset {
            break;
        }

        let range_size = min(range.size, block_end_offset - range_media_offset);

        extents.push(ExtentMapEntry {
            logical_offset: range_media_offset,
            size: range_size,
            target: if range.is_set {
                ExtentMapTarget::Data {
                    source: source.clone(),
                    source_offset: range_data_offset,
                }
            } else {
                ExtentMapTarget::Data {
                    source: parent_source.clone(),
                    source_offset: range_media_offset,
                }
            },
        });

        range_media_offset += range_size;
        range_data_offset += range.size;
    }

    Ok(())
}

fn read_sector_bitmap_ranges(data: &[u8], bytes_per_bit: u16) -> Vec<VhdxSectorBitmapRange> {
    let mut ranges = Vec::new();
    let mut offset: u64 = 0;
    let mut range_offset: u64 = 0;
    let mut range_bit_value: u8 = data[0] & 0x01;

    for byte_value in data.iter().copied() {
        let mut value = byte_value;

        for _ in 0..8 {
            let bit_value = value & 0x01;
            value >>= 1;

            if bit_value != range_bit_value {
                ranges.push(VhdxSectorBitmapRange {
                    size: offset - range_offset,
                    is_set: range_bit_value != 0,
                });
                range_offset = offset;
                range_bit_value = bit_value;
            }

            offset += bytes_per_bit as u64;
        }
    }

    ranges.push(VhdxSectorBitmapRange {
        size: offset - range_offset,
        is_set: range_bit_value != 0,
    });

    ranges
}

fn read_bat_entry(
    source: &dyn crate::source::DataSource,
    bat_offset: u64,
    number_of_entries: u32,
    entry_index: u64,
) -> Result<VhdxBatEntry, ErrorTrace> {
    if entry_index >= number_of_entries as u64 {
        return Err(ErrorTrace::new(format!(
            "Unsupported VHDX BAT entry index: {} value out of bounds",
            entry_index,
        )));
    }

    let entry_offset = bat_offset
        .checked_add(entry_index * 8)
        .ok_or_else(|| ErrorTrace::new("VHDX BAT entry offset overflow".to_string()))?;
    let entry = read_u64_le(source, entry_offset)?;

    Ok(VhdxBatEntry {
        block_state: (entry & 0x0000_0000_0000_0007) as u8,
        block_offset: entry & 0xffff_ffff_fff0_0000,
    })
}

fn read_u32_le(source: &dyn crate::source::DataSource, offset: u64) -> Result<u32, ErrorTrace> {
    let mut data = [0u8; 4];

    source.read_exact_at(offset, &mut data)?;
    Ok(u32::from_le_bytes(data))
}

fn read_u64_le(source: &dyn crate::source::DataSource, offset: u64) -> Result<u64, ErrorTrace> {
    let mut data = [0u8; 8];

    source.read_exact_at(offset, &mut data)?;
    Ok(u64::from_le_bytes(data))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::read_data_source_md5;

    fn open_file(path: &str) -> Result<VhdxFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;

        VhdxFile::open(source)
    }

    fn open_file_with_parent(path: &str, parent_path: &str) -> Result<VhdxFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;
        let parent_file = open_file(parent_path)?;

        VhdxFile::open_with_parent(source, &parent_file)
    }

    #[test]
    fn test_open_fixed_like() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ext2.vhdx")?;

        assert_eq!(file.format_version(), 1);
        assert_eq!(file.disk_type(), VhdDiskType::Fixed);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.block_size(), 8_388_608);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_dynamic_flag_zero_fixture() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ntfs-dynamic.vhdx")?;

        assert_eq!(file.disk_type(), VhdDiskType::Fixed);
        assert_eq!(file.block_size(), 33_554_432);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ntfs-parent.vhdx")?;

        assert_eq!(file.disk_type(), VhdDiskType::Dynamic);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.block_size(), 2_097_152);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_differential_requires_parent() -> Result<(), ErrorTrace> {
        let path_buf = PathBuf::from("../test_data/vhdx/ntfs-differential.vhdx");
        let source = open_local_data_source(&path_buf)?;

        let result = VhdxFile::open(source);

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_open_differential_with_parent() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhdx/ntfs-differential.vhdx",
            "../test_data/vhdx/ntfs-parent.vhdx",
        )?;

        assert_eq!(file.disk_type(), VhdDiskType::Differential);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.block_size(), 2_097_152);
        assert_eq!(file.media_size(), 4_194_304);
        assert_eq!(
            file.parent_identifier().map(ToString::to_string),
            Some("7584f8fb-36d3-4091-afb5-b1afe587bfa8".to_string())
        );
        assert_eq!(
            file.parent_name(),
            Some("C:\\Projects\\dfvfs\\test_data\\ntfs-parent.vhdx")
        );
        Ok(())
    }

    #[test]
    fn test_open_differential_with_wrong_parent_fails() -> Result<(), ErrorTrace> {
        let path_buf = PathBuf::from("../test_data/vhdx/ntfs-differential.vhdx");
        let source = open_local_data_source(&path_buf)?;
        let wrong_parent = open_file("../test_data/vhdx/ntfs-dynamic.vhdx")?;

        let result = VhdxFile::open_with_parent(source, &wrong_parent);

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ext2.vhdx")?;
        let capabilities = file.open_source().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_open_source_sparse_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ext2.vhdx")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_open_source_differential() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhdx/ntfs-differential.vhdx",
            "../test_data/vhdx/ntfs-parent.vhdx",
        )?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(0, &mut data)?;

        assert_eq!(data, [0x33, 0xc0]);
        Ok(())
    }

    #[test]
    fn test_read_media_fixed() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ntfs-parent.vhdx")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "75537374a81c40e51e6a4b812b36ce89");
        Ok(())
    }

    #[test]
    fn test_read_media_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ntfs-dynamic.vhdx")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "20158534070142d63ee02c9ad1a9d87e");
        Ok(())
    }

    #[test]
    fn test_read_media_sparse_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhdx/ext2.vhdx")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "b1760d0b35a512ef56970df4e6f8c5d6");
        Ok(())
    }

    #[test]
    fn test_read_media_differential() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhdx/ntfs-differential.vhdx",
            "../test_data/vhdx/ntfs-parent.vhdx",
        )?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "a25df0058eecd8aa1975a68eeaa0e178");
        Ok(())
    }
}
