/* Copyright 2024-2026 Joachim Metz <joachim.metz@gmail.com>
 * Copyright 2026 Reverier-Xu <reverier.xu@woooo.tech>
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
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};

const VMDK_SPARSE_COWD_FILE_HEADER_SIGNATURE: &[u8; 4] = b"COWD";
const VMDK_SPARSE_FILE_HEADER_SIGNATURE: &[u8; 4] = b"KDMV";
const VMDK_COWD_GRAIN_TABLE_SIZE_BYTES: u64 = 4096 * 512;
const VMDK_SPARSE_FILE_FLAG_USE_SECONDARY_GRAIN_DIRECTORY: u32 = 0x0000_0002;
const VMDK_SPARSE_FILE_FLAG_HAS_GRAIN_COMPRESSION: u32 = 0x0001_0000;
const VMDK_SPARSE_FILE_FLAG_HAS_DATA_MARKERS: u32 = 0x0002_0000;

/// VMDK compression method.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VmdkCompressionMethod {
    None,
    Zlib,
}

/// VMDK disk type.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VmdkDiskType {
    MonolithicSparse,
    VmfsSparse,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VmdkDescriptorExtentType {
    Sparse,
    Unknown,
}

struct VmdkSparseFileHeader {
    flags: u32,
    maximum_data_number_of_sectors: u64,
    sectors_per_grain: u64,
    descriptor_start_sector: u64,
    descriptor_size: u64,
    number_of_grain_table_entries: u32,
    secondary_grain_directory_start_sector: u64,
    primary_grain_directory_start_sector: u64,
}

struct VmdkSparseCowdFileHeader {
    maximum_data_number_of_sectors: u32,
    sectors_per_grain: u32,
    grain_directory_start_sector: u32,
    number_of_grain_directory_entries: u32,
}

struct VmdkDescriptorExtent {
    number_of_sectors: u64,
    extent_type: VmdkDescriptorExtentType,
    file_name: Option<String>,
}

struct VmdkDescriptor {
    content_identifier: u32,
    parent_content_identifier: Option<u32>,
    disk_type: VmdkDiskType,
    extents: Vec<VmdkDescriptorExtent>,
}

/// Immutable VMDK file metadata plus opened logical source.
pub struct VmdkFile {
    disk_type: VmdkDiskType,
    bytes_per_sector: u16,
    sectors_per_grain: u64,
    compression_method: VmdkCompressionMethod,
    content_identifier: u32,
    parent_content_identifier: Option<u32>,
    media_size: u64,
    logical_source: DataSourceReference,
}

impl VmdkFile {
    /// Opens and parses a VMDK sparse or COWD file.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let mut signature = [0u8; 4];

        source.read_exact_at(0, &mut signature)?;

        if &signature == VMDK_SPARSE_COWD_FILE_HEADER_SIGNATURE {
            return Self::open_cowd(source);
        }

        let source_size = source.size()?;
        let file_header = VmdkSparseFileHeader::read_at(source.as_ref(), 0)?;

        if file_header.descriptor_start_sector == 0 {
            return Err(ErrorTrace::new(
                "Invalid VMDK descriptor start sector value out of bounds".to_string(),
            ));
        }
        if file_header.descriptor_size == 0 {
            return Err(ErrorTrace::new(
                "Invalid VMDK descriptor size value out of bounds".to_string(),
            ));
        }

        let descriptor_offset = file_header
            .descriptor_start_sector
            .checked_mul(512)
            .ok_or_else(|| ErrorTrace::new("VMDK descriptor offset overflow".to_string()))?;
        let descriptor_size = file_header
            .descriptor_size
            .checked_mul(512)
            .ok_or_else(|| ErrorTrace::new("VMDK descriptor size overflow".to_string()))?;
        let descriptor = read_descriptor(source.as_ref(), descriptor_offset, descriptor_size)?;

        if descriptor.disk_type != VmdkDiskType::MonolithicSparse {
            return Err(ErrorTrace::new(format!(
                "Unsupported VMDK disk type: {:?}",
                descriptor.disk_type,
            )));
        }
        if descriptor.extents.len() != 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VMDK number of extents: {}",
                descriptor.extents.len(),
            )));
        }

        let extent = &descriptor.extents[0];
        if extent.extent_type != VmdkDescriptorExtentType::Sparse {
            return Err(ErrorTrace::new(
                "Unsupported VMDK extent type for monolithic sparse file".to_string(),
            ));
        }
        if matches!(extent.file_name.as_deref(), Some("")) {
            return Err(ErrorTrace::new("Invalid VMDK extent file name".to_string()));
        }

        let bytes_per_sector: u16 = 512;
        let media_size = extent
            .number_of_sectors
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("VMDK media size overflow".to_string()))?;

        if media_size
            > file_header
                .maximum_data_number_of_sectors
                .checked_mul(bytes_per_sector as u64)
                .ok_or_else(|| ErrorTrace::new("VMDK maximum media size overflow".to_string()))?
        {
            return Err(ErrorTrace::new(
                "VMDK descriptor media size exceeds sparse header capacity".to_string(),
            ));
        }

        let extents = build_sparse_extents(source.clone(), source_size, media_size, &file_header)?;
        let logical_source: DataSourceReference = Arc::new(ExtentMapDataSource::new(extents)?);

        Ok(Self {
            disk_type: descriptor.disk_type,
            bytes_per_sector,
            sectors_per_grain: file_header.sectors_per_grain,
            compression_method: if file_header.flags & VMDK_SPARSE_FILE_FLAG_HAS_GRAIN_COMPRESSION
                != 0
            {
                VmdkCompressionMethod::Zlib
            } else {
                VmdkCompressionMethod::None
            },
            content_identifier: descriptor.content_identifier,
            parent_content_identifier: descriptor.parent_content_identifier,
            media_size,
            logical_source,
        })
    }

    fn open_cowd(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let source_size = source.size()?;
        let file_header = VmdkSparseCowdFileHeader::read_at(source.as_ref(), 0)?;
        let bytes_per_sector: u16 = 512;
        let media_size = (file_header.maximum_data_number_of_sectors as u64)
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("VMDK COWD media size overflow".to_string()))?;
        let extents =
            build_sparse_cowd_extents(source.clone(), source_size, media_size, &file_header)?;
        let logical_source: DataSourceReference = Arc::new(ExtentMapDataSource::new(extents)?);

        Ok(Self {
            disk_type: VmdkDiskType::VmfsSparse,
            bytes_per_sector,
            sectors_per_grain: file_header.sectors_per_grain as u64,
            compression_method: VmdkCompressionMethod::None,
            content_identifier: 0,
            parent_content_identifier: None,
            media_size,
            logical_source,
        })
    }

    /// Retrieves the disk type.
    pub fn disk_type(&self) -> VmdkDiskType {
        self.disk_type
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the sectors per grain.
    pub fn sectors_per_grain(&self) -> u64 {
        self.sectors_per_grain
    }

    /// Retrieves the compression method.
    pub fn compression_method(&self) -> VmdkCompressionMethod {
        self.compression_method
    }

    /// Retrieves the content identifier.
    pub fn content_identifier(&self) -> u32 {
        self.content_identifier
    }

    /// Retrieves the parent content identifier if present.
    pub fn parent_content_identifier(&self) -> Option<u32> {
        self.parent_content_identifier
    }

    /// Retrieves the media size.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Opens the logical media source.
    pub fn open_source(&self) -> DataSourceReference {
        self.logical_source.clone()
    }
}

impl VmdkSparseCowdFileHeader {
    fn read_at(source: &dyn crate::source::DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 2048];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..4] != VMDK_SPARSE_COWD_FILE_HEADER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported VMDK sparse COWD file signature".to_string(),
            ));
        }

        let format_version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if format_version != 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VMDK sparse COWD file format version: {}",
                format_version,
            )));
        }

        let sectors_per_grain = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let grain_directory_start_sector =
            u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        let number_of_grain_directory_entries =
            u32::from_le_bytes([data[24], data[25], data[26], data[27]]);

        if sectors_per_grain < 8 || sectors_per_grain & (sectors_per_grain - 1) != 0 {
            return Err(ErrorTrace::new(
                "Invalid VMDK sparse COWD sectors per grain value out of bounds".to_string(),
            ));
        }
        if grain_directory_start_sector == 0 {
            return Err(ErrorTrace::new(
                "Invalid VMDK sparse COWD grain directory start sector value out of bounds"
                    .to_string(),
            ));
        }
        if number_of_grain_directory_entries == 0 {
            return Err(ErrorTrace::new(
                "Invalid VMDK sparse COWD number of grain directory entries value out of bounds"
                    .to_string(),
            ));
        }

        Ok(Self {
            maximum_data_number_of_sectors: u32::from_le_bytes([
                data[12], data[13], data[14], data[15],
            ]),
            sectors_per_grain,
            grain_directory_start_sector,
            number_of_grain_directory_entries,
        })
    }
}

impl VmdkSparseFileHeader {
    fn read_at(source: &dyn crate::source::DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..4] != VMDK_SPARSE_FILE_HEADER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported VMDK sparse file signature".to_string(),
            ));
        }
        if data[73..77] != [0x0a, 0x20, 0x0d, 0x0a] {
            return Err(ErrorTrace::new(
                "Unsupported VMDK sparse file character values".to_string(),
            ));
        }

        let format_version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let flags = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let sectors_per_grain = u64::from_le_bytes([
            data[20], data[21], data[22], data[23], data[24], data[25], data[26], data[27],
        ]);

        if format_version != 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VMDK sparse file format version: {}",
                format_version,
            )));
        }

        if sectors_per_grain < 8 || sectors_per_grain & (sectors_per_grain - 1) != 0 {
            return Err(ErrorTrace::new(
                "Invalid VMDK sectors per grain value out of bounds".to_string(),
            ));
        }

        let supported_flags = 0x0000_0001
            | VMDK_SPARSE_FILE_FLAG_USE_SECONDARY_GRAIN_DIRECTORY
            | 0x0000_0004
            | VMDK_SPARSE_FILE_FLAG_HAS_GRAIN_COMPRESSION
            | VMDK_SPARSE_FILE_FLAG_HAS_DATA_MARKERS;

        if flags & !supported_flags != 0 {
            return Err(ErrorTrace::new(
                "Unsupported VMDK sparse file flags".to_string(),
            ));
        }

        Ok(Self {
            flags,
            maximum_data_number_of_sectors: u64::from_le_bytes([
                data[12], data[13], data[14], data[15], data[16], data[17], data[18], data[19],
            ]),
            sectors_per_grain,
            descriptor_start_sector: u64::from_le_bytes([
                data[28], data[29], data[30], data[31], data[32], data[33], data[34], data[35],
            ]),
            descriptor_size: u64::from_le_bytes([
                data[36], data[37], data[38], data[39], data[40], data[41], data[42], data[43],
            ]),
            number_of_grain_table_entries: u32::from_le_bytes([
                data[44], data[45], data[46], data[47],
            ]),
            secondary_grain_directory_start_sector: u64::from_le_bytes([
                data[48], data[49], data[50], data[51], data[52], data[53], data[54], data[55],
            ]),
            primary_grain_directory_start_sector: u64::from_le_bytes([
                data[56], data[57], data[58], data[59], data[60], data[61], data[62], data[63],
            ]),
        })
    }
}

fn read_descriptor(
    source: &dyn crate::source::DataSource,
    offset: u64,
    size: u64,
) -> Result<VmdkDescriptor, ErrorTrace> {
    if !(21..=16_777_216).contains(&size) {
        return Err(ErrorTrace::new(format!(
            "Unsupported VMDK descriptor size: {} value out of bounds",
            size,
        )));
    }

    let mut data = vec![0u8; size as usize];
    source.read_exact_at(offset, &mut data)?;

    let end_offset = data
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.len());
    let text = std::str::from_utf8(&data[..end_offset]).map_err(|error| {
        ErrorTrace::new(format!(
            "Unable to convert VMDK descriptor into UTF-8 string with error: {}",
            error,
        ))
    })?;
    let mut lines = text.lines();

    let signature = lines
        .next()
        .map(str::trim)
        .map(|line| line.to_ascii_lowercase())
        .ok_or_else(|| {
            ErrorTrace::new("Invalid VMDK descriptor data - missing signature".to_string())
        })?;

    if signature != "# disk descriptorfile" {
        return Err(ErrorTrace::new(
            "Invalid VMDK descriptor data - unsupported signature".to_string(),
        ));
    }

    let mut content_identifier: u32 = 0;
    let mut parent_content_identifier: Option<u32> = None;
    let mut disk_type = VmdkDiskType::Unknown;
    let mut extents = Vec::new();
    let mut in_extent_section = false;

    for line in lines {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        let lowercase_line = trimmed_line.to_ascii_lowercase();
        if lowercase_line == "# extent description" {
            in_extent_section = true;
            continue;
        }
        if in_extent_section
            && (lowercase_line == "# change tracking file"
                || lowercase_line == "# the disk data base")
        {
            break;
        }
        if trimmed_line.starts_with('#') {
            continue;
        }

        if !in_extent_section {
            let (key, value) = trimmed_line.split_once('=').ok_or_else(|| {
                ErrorTrace::new(
                    "Invalid VMDK descriptor data - unsupported key-value pair".to_string(),
                )
            })?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            match key.as_str() {
                "cid" => {
                    content_identifier = u32::from_str_radix(value, 16).map_err(|error| {
                        ErrorTrace::new(format!("Unsupported VMDK CID value with error: {}", error))
                    })?;
                }
                "parentcid" => {
                    let value = u32::from_str_radix(value, 16).map_err(|error| {
                        ErrorTrace::new(format!(
                            "Unsupported VMDK parentCID value with error: {}",
                            error
                        ))
                    })?;
                    if value != 0xffff_ffff {
                        parent_content_identifier = Some(value);
                    }
                }
                "createtype" => {
                    let value = value.trim_matches('"').to_ascii_lowercase();
                    disk_type = match value.as_str() {
                        "monolithicsparse" => VmdkDiskType::MonolithicSparse,
                        _ => VmdkDiskType::Unknown,
                    };
                }
                _ => {}
            }
        } else {
            extents.push(parse_descriptor_extent(trimmed_line)?);
        }
    }

    if disk_type == VmdkDiskType::Unknown {
        return Err(ErrorTrace::new(
            "Unsupported VMDK createType value".to_string(),
        ));
    }

    Ok(VmdkDescriptor {
        content_identifier,
        parent_content_identifier,
        disk_type,
        extents,
    })
}

fn parse_descriptor_extent(line: &str) -> Result<VmdkDescriptorExtent, ErrorTrace> {
    let mut parts = line.split_whitespace();
    let access_mode = parts
        .next()
        .ok_or_else(|| ErrorTrace::new("Unsupported VMDK extent value".to_string()))?;
    if !access_mode.eq_ignore_ascii_case("rw") {
        return Err(ErrorTrace::new(
            "Unsupported VMDK extent access mode".to_string(),
        ));
    }

    let number_of_sectors = parts
        .next()
        .ok_or_else(|| ErrorTrace::new("Missing VMDK extent number of sectors".to_string()))?
        .parse::<u64>()
        .map_err(|error| {
            ErrorTrace::new(format!(
                "Unsupported VMDK extent number of sectors value with error: {}",
                error,
            ))
        })?;
    let extent_type = match parts
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "sparse" => VmdkDescriptorExtentType::Sparse,
        _ => VmdkDescriptorExtentType::Unknown,
    };

    let file_name = line.find('"').and_then(|start_offset| {
        line[start_offset + 1..]
            .find('"')
            .map(|end_offset| line[start_offset + 1..start_offset + 1 + end_offset].to_string())
    });

    Ok(VmdkDescriptorExtent {
        number_of_sectors,
        extent_type,
        file_name,
    })
}

fn build_sparse_extents(
    source: DataSourceReference,
    source_size: u64,
    media_size: u64,
    file_header: &VmdkSparseFileHeader,
) -> Result<Vec<ExtentMapEntry>, ErrorTrace> {
    if file_header.flags & VMDK_SPARSE_FILE_FLAG_HAS_GRAIN_COMPRESSION != 0 {
        return Err(ErrorTrace::new(
            "Compressed VMDK sparse grains are not supported yet in keramics-drivers".to_string(),
        ));
    }
    if file_header.number_of_grain_table_entries == 0 {
        return Err(ErrorTrace::new(
            "Invalid VMDK number of grain table entries value out of bounds".to_string(),
        ));
    }

    let grain_size = file_header
        .sectors_per_grain
        .checked_mul(512)
        .ok_or_else(|| ErrorTrace::new("VMDK grain size overflow".to_string()))?;
    let grain_table_number_of_sectors = (file_header.number_of_grain_table_entries as u64)
        .checked_mul(file_header.sectors_per_grain)
        .ok_or_else(|| ErrorTrace::new("VMDK grain table size overflow".to_string()))?;
    let number_of_grain_directory_entries = file_header
        .maximum_data_number_of_sectors
        .div_ceil(grain_table_number_of_sectors);
    let grain_directory_offset =
        if file_header.flags & VMDK_SPARSE_FILE_FLAG_USE_SECONDARY_GRAIN_DIRECTORY == 0 {
            if file_header.primary_grain_directory_start_sector == 0 {
                return Err(ErrorTrace::new(
                    "Invalid VMDK primary grain directory start sector value out of bounds"
                        .to_string(),
                ));
            }
            file_header
                .primary_grain_directory_start_sector
                .checked_mul(512)
                .ok_or_else(|| {
                    ErrorTrace::new("VMDK primary grain directory offset overflow".to_string())
                })?
        } else {
            if file_header.secondary_grain_directory_start_sector == 0 {
                return Err(ErrorTrace::new(
                    "Invalid VMDK secondary grain directory start sector value out of bounds"
                        .to_string(),
                ));
            }
            file_header
                .secondary_grain_directory_start_sector
                .checked_mul(512)
                .ok_or_else(|| {
                    ErrorTrace::new("VMDK secondary grain directory offset overflow".to_string())
                })?
        };

    let mut extents = Vec::new();
    let mut current_extent: Option<ExtentMapEntry> = None;

    for grain_directory_index in 0..number_of_grain_directory_entries {
        let grain_directory_entry = read_u32_le(
            source.as_ref(),
            grain_directory_offset
                .checked_add(grain_directory_index * 4)
                .ok_or_else(|| {
                    ErrorTrace::new("VMDK grain directory entry offset overflow".to_string())
                })?,
        )?;

        if grain_directory_entry == 0 {
            let grain_extent_offset = grain_directory_index
                .checked_mul(file_header.number_of_grain_table_entries as u64)
                .and_then(|value| value.checked_mul(grain_size))
                .ok_or_else(|| ErrorTrace::new("VMDK sparse extent offset overflow".to_string()))?;
            let grain_extent_size = (file_header.number_of_grain_table_entries as u64)
                .checked_mul(grain_size)
                .ok_or_else(|| ErrorTrace::new("VMDK sparse extent size overflow".to_string()))?;

            if grain_extent_offset < media_size {
                current_extent = merge_extent(
                    current_extent,
                    ExtentMapEntry {
                        logical_offset: grain_extent_offset,
                        size: min(grain_extent_size, media_size - grain_extent_offset),
                        target: ExtentMapTarget::Zero,
                    },
                    &mut extents,
                );
            }
            continue;
        }

        let grain_table_offset = (grain_directory_entry as u64)
            .checked_mul(512)
            .ok_or_else(|| ErrorTrace::new("VMDK grain table offset overflow".to_string()))?;

        for grain_table_index in 0..file_header.number_of_grain_table_entries as u64 {
            let grain_index = grain_directory_index
                .checked_mul(file_header.number_of_grain_table_entries as u64)
                .and_then(|value| value.checked_add(grain_table_index))
                .ok_or_else(|| ErrorTrace::new("VMDK grain index overflow".to_string()))?;
            let grain_extent_offset = grain_index
                .checked_mul(grain_size)
                .ok_or_else(|| ErrorTrace::new("VMDK grain extent offset overflow".to_string()))?;

            if grain_extent_offset >= media_size {
                break;
            }

            let sector_number = read_u32_le(
                source.as_ref(),
                grain_table_offset
                    .checked_add(grain_table_index * 4)
                    .ok_or_else(|| {
                        ErrorTrace::new("VMDK grain table entry offset overflow".to_string())
                    })?,
            )?;
            let grain_data_offset = (sector_number as u64)
                .checked_mul(512)
                .ok_or_else(|| ErrorTrace::new("VMDK grain data offset overflow".to_string()))?;
            let grain_logical_size = min(grain_size, media_size - grain_extent_offset);

            let extent = if sector_number == 0 {
                ExtentMapEntry {
                    logical_offset: grain_extent_offset,
                    size: grain_logical_size,
                    target: ExtentMapTarget::Zero,
                }
            } else {
                let grain_data_end = grain_data_offset
                    .checked_add(grain_logical_size)
                    .ok_or_else(|| ErrorTrace::new("VMDK grain data end overflow".to_string()))?;

                if grain_data_end > source_size {
                    return Err(ErrorTrace::new(format!(
                        "VMDK grain data exceeds file size at logical offset: {}",
                        grain_extent_offset,
                    )));
                }

                ExtentMapEntry {
                    logical_offset: grain_extent_offset,
                    size: grain_logical_size,
                    target: ExtentMapTarget::Data {
                        source: source.clone(),
                        source_offset: grain_data_offset,
                    },
                }
            };

            current_extent = merge_extent(current_extent, extent, &mut extents);
        }
    }

    if let Some(current_extent) = current_extent {
        extents.push(current_extent);
    }

    Ok(extents)
}

fn build_sparse_cowd_extents(
    source: DataSourceReference,
    source_size: u64,
    media_size: u64,
    file_header: &VmdkSparseCowdFileHeader,
) -> Result<Vec<ExtentMapEntry>, ErrorTrace> {
    let grain_size = (file_header.sectors_per_grain as u64)
        .checked_mul(512)
        .ok_or_else(|| ErrorTrace::new("VMDK COWD grain size overflow".to_string()))?;
    let number_of_grains = media_size.div_ceil(grain_size);
    let entries_per_grain_table = VMDK_COWD_GRAIN_TABLE_SIZE_BYTES / 4;
    let grain_directory_offset = (file_header.grain_directory_start_sector as u64)
        .checked_mul(512)
        .ok_or_else(|| ErrorTrace::new("VMDK COWD grain directory offset overflow".to_string()))?;
    let mut extents = Vec::new();
    let mut current_extent: Option<ExtentMapEntry> = None;

    for grain_index in 0..number_of_grains {
        let grain_directory_index = grain_index / entries_per_grain_table;

        if grain_directory_index >= file_header.number_of_grain_directory_entries as u64 {
            return Err(ErrorTrace::new(format!(
                "Invalid VMDK COWD grain directory index: {} value out of bounds",
                grain_directory_index,
            )));
        }

        let grain_directory_entry = read_u32_le(
            source.as_ref(),
            grain_directory_offset
                .checked_add(grain_directory_index * 4)
                .ok_or_else(|| {
                    ErrorTrace::new("VMDK COWD grain directory entry offset overflow".to_string())
                })?,
        )?;
        let grain_logical_offset = grain_index.checked_mul(grain_size).ok_or_else(|| {
            ErrorTrace::new("VMDK COWD grain logical offset overflow".to_string())
        })?;
        let grain_logical_size = min(grain_size, media_size - grain_logical_offset);

        let extent = if grain_directory_entry == 0 {
            ExtentMapEntry {
                logical_offset: grain_logical_offset,
                size: grain_logical_size,
                target: ExtentMapTarget::Zero,
            }
        } else {
            let grain_table_offset =
                (grain_directory_entry as u64)
                    .checked_mul(512)
                    .ok_or_else(|| {
                        ErrorTrace::new("VMDK COWD grain table offset overflow".to_string())
                    })?;
            let grain_table_index = grain_index % entries_per_grain_table;
            let sector_number = read_u32_le(
                source.as_ref(),
                grain_table_offset
                    .checked_add(grain_table_index * 4)
                    .ok_or_else(|| {
                        ErrorTrace::new("VMDK COWD grain table entry offset overflow".to_string())
                    })?,
            )?;

            if sector_number == 0 {
                ExtentMapEntry {
                    logical_offset: grain_logical_offset,
                    size: grain_logical_size,
                    target: ExtentMapTarget::Zero,
                }
            } else {
                let grain_data_offset =
                    (sector_number as u64).checked_mul(512).ok_or_else(|| {
                        ErrorTrace::new("VMDK COWD grain data offset overflow".to_string())
                    })?;
                let grain_data_end = grain_data_offset
                    .checked_add(grain_logical_size)
                    .ok_or_else(|| {
                        ErrorTrace::new("VMDK COWD grain data end overflow".to_string())
                    })?;

                if grain_data_end > source_size {
                    return Err(ErrorTrace::new(format!(
                        "VMDK COWD grain data exceeds file size at logical offset: {}",
                        grain_logical_offset,
                    )));
                }

                ExtentMapEntry {
                    logical_offset: grain_logical_offset,
                    size: grain_logical_size,
                    target: ExtentMapTarget::Data {
                        source: source.clone(),
                        source_offset: grain_data_offset,
                    },
                }
            }
        };

        current_extent = merge_extent(current_extent, extent, &mut extents);
    }

    if let Some(current_extent) = current_extent {
        extents.push(current_extent);
    }

    Ok(extents)
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

fn read_u32_le(source: &dyn crate::source::DataSource, offset: u64) -> Result<u32, ErrorTrace> {
    let mut data = [0u8; 4];

    source.read_exact_at(offset, &mut data)?;
    Ok(u32::from_le_bytes(data))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::read_data_source_md5;

    fn open_file(path: &str) -> Result<VmdkFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;

        VmdkFile::open(source)
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.vmdk")?;

        assert_eq!(file.disk_type(), VmdkDiskType::MonolithicSparse);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.sectors_per_grain(), 128);
        assert_eq!(file.compression_method(), VmdkCompressionMethod::None);
        assert_eq!(file.content_identifier(), 0x4c06_9322);
        assert_eq!(file.parent_content_identifier(), None);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_cowd() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.cowd")?;

        assert_eq!(file.disk_type(), VmdkDiskType::VmfsSparse);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.sectors_per_grain(), 128);
        assert_eq!(file.compression_method(), VmdkCompressionMethod::None);
        assert_eq!(file.content_identifier(), 0);
        assert_eq!(file.parent_content_identifier(), None);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.vmdk")?;
        let capabilities = file.open_source().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities_cowd() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.cowd")?;
        let capabilities = file.open_source().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.vmdk")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_open_source_cowd() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.cowd")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_read_media() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.vmdk")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "b1760d0b35a512ef56970df4e6f8c5d6");
        Ok(())
    }

    #[test]
    fn test_read_media_cowd() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vmdk/ext2.cowd")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "b1760d0b35a512ef56970df4e6f8c5d6");
        Ok(())
    }
}
