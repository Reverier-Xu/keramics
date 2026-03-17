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
use keramics_types::Uuid;

use crate::source::{
    DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget, SliceDataSource,
};

const VHD_FILE_FOOTER_SIGNATURE: &[u8; 8] = b"conectix";
const VHD_DYNAMIC_DISK_HEADER_SIGNATURE: &[u8; 8] = b"cxsparse";
const VHD_DISK_TYPE_FIXED: u32 = 2;
const VHD_DISK_TYPE_DYNAMIC: u32 = 3;
const VHD_DISK_TYPE_DIFFERENTIAL: u32 = 4;

/// VHD disk type.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VhdDiskType {
    Differential,
    Dynamic,
    Fixed,
}

struct VhdFileFooter {
    next_offset: u64,
    data_size: u64,
    disk_type: u32,
    identifier: Uuid,
}

struct VhdDynamicDiskHeader {
    block_table_offset: u64,
    number_of_blocks: u32,
    block_size: u32,
    parent_identifier: Uuid,
    parent_name: String,
}

struct VhdSectorBitmapRange {
    size: u64,
    is_set: bool,
}

/// Immutable VHD file metadata plus opened logical source.
pub struct VhdFile {
    disk_type: VhdDiskType,
    identifier: Uuid,
    parent_identifier: Option<Uuid>,
    parent_name: Option<String>,
    bytes_per_sector: u16,
    block_size: u32,
    media_size: u64,
    logical_source: DataSourceReference,
}

impl VhdFile {
    /// Opens and parses a VHD file.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        Self::open_internal(source, None)
    }

    /// Opens and parses a differential VHD file with its parent file.
    pub fn open_with_parent(
        source: DataSourceReference,
        parent_file: &VhdFile,
    ) -> Result<Self, ErrorTrace> {
        Self::open_internal(source, Some(parent_file))
    }

    fn open_internal(
        source: DataSourceReference,
        parent_file: Option<&VhdFile>,
    ) -> Result<Self, ErrorTrace> {
        let source_size = source.size()?;
        if source_size < 512 {
            return Err(ErrorTrace::new(
                "Unsupported VHD file size smaller than footer".to_string(),
            ));
        }

        let footer = VhdFileFooter::read_at(source.as_ref(), source_size - 512)?;
        let bytes_per_sector: u16 = 512;
        let identifier = footer.identifier.clone();
        let media_size = footer.data_size;

        match footer.disk_type {
            VHD_DISK_TYPE_FIXED => {
                let logical_source: DataSourceReference =
                    Arc::new(SliceDataSource::new(source, 0, media_size));

                Ok(Self {
                    disk_type: VhdDiskType::Fixed,
                    identifier,
                    parent_identifier: None,
                    parent_name: None,
                    bytes_per_sector,
                    block_size: 0,
                    media_size,
                    logical_source,
                })
            }
            VHD_DISK_TYPE_DYNAMIC | VHD_DISK_TYPE_DIFFERENTIAL => {
                let dynamic_header =
                    VhdDynamicDiskHeader::read_at(source.as_ref(), footer.next_offset)?;

                let parent_source: Option<DataSourceReference> = if footer.disk_type
                    == VHD_DISK_TYPE_DIFFERENTIAL
                {
                    let parent_file = parent_file.ok_or_else(|| {
                        ErrorTrace::new(
                            "Differential VHD files require an explicit parent file".to_string(),
                        )
                    })?;

                    if dynamic_header.parent_identifier.is_nil() {
                        return Err(ErrorTrace::new(
                            "Differential VHD file is missing a parent identifier".to_string(),
                        ));
                    }
                    if parent_file.identifier() != &dynamic_header.parent_identifier {
                        return Err(ErrorTrace::new(format!(
                            "Parent identifier: {} does not match identifier of parent file: {}",
                            dynamic_header.parent_identifier,
                            parent_file.identifier(),
                        )));
                    }

                    Some(parent_file.open_source())
                } else {
                    None
                };

                if footer.disk_type == VHD_DISK_TYPE_DYNAMIC
                    && !dynamic_header.parent_identifier.is_nil()
                {
                    return Err(ErrorTrace::new(
                        "Dynamic VHD unexpectedly contains a parent identifier".to_string(),
                    ));
                }

                let extents = build_dynamic_extents(
                    source.clone(),
                    source_size,
                    media_size,
                    bytes_per_sector,
                    &dynamic_header,
                    parent_source,
                )?;
                let logical_source: DataSourceReference =
                    Arc::new(ExtentMapDataSource::new(extents)?);

                Ok(Self {
                    disk_type: if footer.disk_type == VHD_DISK_TYPE_DIFFERENTIAL {
                        VhdDiskType::Differential
                    } else {
                        VhdDiskType::Dynamic
                    },
                    identifier,
                    parent_identifier: if dynamic_header.parent_identifier.is_nil() {
                        None
                    } else {
                        Some(dynamic_header.parent_identifier.clone())
                    },
                    parent_name: if dynamic_header.parent_name.is_empty() {
                        None
                    } else {
                        Some(dynamic_header.parent_name)
                    },
                    bytes_per_sector,
                    block_size: dynamic_header.block_size,
                    media_size,
                    logical_source,
                })
            }
            _ => Err(ErrorTrace::new(format!(
                "Unsupported VHD disk type: {}",
                footer.disk_type,
            ))),
        }
    }

    /// Retrieves the disk type.
    pub fn disk_type(&self) -> VhdDiskType {
        self.disk_type
    }

    /// Retrieves the file identifier.
    pub fn identifier(&self) -> &Uuid {
        &self.identifier
    }

    /// Retrieves the parent identifier if present.
    pub fn parent_identifier(&self) -> Option<&Uuid> {
        self.parent_identifier.as_ref()
    }

    /// Retrieves the parent name if present.
    pub fn parent_name(&self) -> Option<&str> {
        self.parent_name.as_deref()
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the dynamic block size in bytes.
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

impl VhdFileFooter {
    fn read_at(source: &dyn crate::source::DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..8] != VHD_FILE_FOOTER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported VHD file footer signature".to_string(),
            ));
        }

        let format_version = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
        if format_version != 0x0001_0000 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHD format version: 0x{:08x}",
                format_version,
            )));
        }

        let next_offset = u64::from_be_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]);
        let disk_type = u32::from_be_bytes([data[60], data[61], data[62], data[63]]);

        if disk_type == VHD_DISK_TYPE_FIXED {
            if next_offset != 0xffff_ffff_ffff_ffff {
                return Err(ErrorTrace::new(format!(
                    "Unsupported VHD fixed next offset: 0x{:08x}",
                    next_offset,
                )));
            }
        } else if next_offset < 512 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHD dynamic metadata offset: 0x{:08x}",
                next_offset,
            )));
        }

        Ok(Self {
            next_offset,
            data_size: u64::from_be_bytes([
                data[40], data[41], data[42], data[43], data[44], data[45], data[46], data[47],
            ]),
            disk_type,
            identifier: Uuid::from_be_bytes(&data[68..84]),
        })
    }
}

impl VhdDynamicDiskHeader {
    fn read_at(source: &dyn crate::source::DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 1024];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..8] != VHD_DYNAMIC_DISK_HEADER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported VHD dynamic disk header signature".to_string(),
            ));
        }

        let next_offset = u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        if next_offset != 0xffff_ffff_ffff_ffff {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHD dynamic header next offset: 0x{:08x}",
                next_offset,
            )));
        }

        let format_version = u32::from_be_bytes([data[24], data[25], data[26], data[27]]);
        if format_version != 0x0001_0000 {
            return Err(ErrorTrace::new(format!(
                "Unsupported VHD dynamic header format version: 0x{:08x}",
                format_version,
            )));
        }

        let block_size = u32::from_be_bytes([data[32], data[33], data[34], data[35]]);
        if block_size == 0 {
            return Err(ErrorTrace::new(
                "Invalid VHD dynamic block size: 0".to_string(),
            ));
        }

        Ok(Self {
            block_table_offset: u64::from_be_bytes([
                data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
            ]),
            number_of_blocks: u32::from_be_bytes([data[28], data[29], data[30], data[31]]),
            block_size,
            parent_identifier: Uuid::from_be_bytes(&data[40..56]),
            parent_name: decode_ucs2_be_string(&data[64..576]),
        })
    }
}

fn build_dynamic_extents(
    source: DataSourceReference,
    source_size: u64,
    media_size: u64,
    bytes_per_sector: u16,
    dynamic_header: &VhdDynamicDiskHeader,
    parent_source: Option<DataSourceReference>,
) -> Result<Vec<ExtentMapEntry>, ErrorTrace> {
    let sectors_per_block = dynamic_header.block_size / (bytes_per_sector as u32);
    let sector_bitmap_size = (sectors_per_block / 8).div_ceil(512) * 512;
    let number_of_blocks = dynamic_header.number_of_blocks as usize;
    let mut extents = Vec::new();

    if media_size > (dynamic_header.number_of_blocks as u64) * (dynamic_header.block_size as u64) {
        let calculated_number_of_blocks = media_size.div_ceil(dynamic_header.block_size as u64);
        return Err(ErrorTrace::new(format!(
            "Number of VHD blocks: {} too small for media size: {} ({} blocks)",
            dynamic_header.number_of_blocks, media_size, calculated_number_of_blocks,
        )));
    }

    for block_index in 0..number_of_blocks {
        let block_media_offset = (block_index as u64)
            .checked_mul(dynamic_header.block_size as u64)
            .ok_or_else(|| ErrorTrace::new("VHD block media offset overflow".to_string()))?;

        if block_media_offset >= media_size {
            break;
        }

        let block_logical_size = min(
            dynamic_header.block_size as u64,
            media_size - block_media_offset,
        );
        let block_table_entry_offset = dynamic_header
            .block_table_offset
            .checked_add((block_index as u64) * 4)
            .ok_or_else(|| ErrorTrace::new("VHD BAT offset overflow".to_string()))?;
        let mut entry_data = [0u8; 4];

        source.read_exact_at(block_table_entry_offset, &mut entry_data)?;

        let sector_number = u32::from_be_bytes(entry_data);
        if sector_number == 0xffff_ffff {
            extents.push(ExtentMapEntry {
                logical_offset: block_media_offset,
                size: block_logical_size,
                target: match &parent_source {
                    Some(parent_source) => ExtentMapTarget::Data {
                        source: parent_source.clone(),
                        source_offset: block_media_offset,
                    },
                    None => ExtentMapTarget::Zero,
                },
            });
            continue;
        }

        let sector_bitmap_offset = (sector_number as u64)
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("VHD sector bitmap offset overflow".to_string()))?;
        let data_offset = sector_bitmap_offset
            .checked_add(sector_bitmap_size as u64)
            .ok_or_else(|| ErrorTrace::new("VHD block data offset overflow".to_string()))?;
        let block_data_end = data_offset
            .checked_add(dynamic_header.block_size as u64)
            .ok_or_else(|| ErrorTrace::new("VHD block data end offset overflow".to_string()))?;

        if block_data_end > source_size {
            return Err(ErrorTrace::new(format!(
                "VHD block data exceeds file size for block index: {}",
                block_index,
            )));
        }

        let mut sector_bitmap_data = vec![0; sector_bitmap_size as usize];
        source.read_exact_at(sector_bitmap_offset, &mut sector_bitmap_data)?;

        let ranges = read_sector_bitmap_ranges(&sector_bitmap_data, bytes_per_sector);
        let mut range_media_offset = block_media_offset;
        let mut range_data_offset = data_offset;

        for range in ranges {
            if range_media_offset >= media_size {
                break;
            }

            let range_size = min(range.size, media_size - range_media_offset);

            extents.push(ExtentMapEntry {
                logical_offset: range_media_offset,
                size: range_size,
                target: if range.is_set {
                    ExtentMapTarget::Data {
                        source: source.clone(),
                        source_offset: range_data_offset,
                    }
                } else {
                    match &parent_source {
                        Some(parent_source) => ExtentMapTarget::Data {
                            source: parent_source.clone(),
                            source_offset: range_media_offset,
                        },
                        None => ExtentMapTarget::Zero,
                    }
                },
            });

            range_media_offset = range_media_offset
                .checked_add(range_size)
                .ok_or_else(|| ErrorTrace::new("VHD range media offset overflow".to_string()))?;
            range_data_offset = range_data_offset
                .checked_add(range.size)
                .ok_or_else(|| ErrorTrace::new("VHD range data offset overflow".to_string()))?;
        }
    }

    Ok(extents)
}

fn read_sector_bitmap_ranges(data: &[u8], bytes_per_bit: u16) -> Vec<VhdSectorBitmapRange> {
    let mut ranges = Vec::new();
    let mut offset: u64 = 0;
    let mut range_offset: u64 = 0;
    let mut range_bit_value: u8 = data[0] >> 7;

    for byte_value in data.iter().copied() {
        let mut value = byte_value;

        for _ in (0..8).rev() {
            let bit_value = value & 0x01;
            value >>= 1;

            if bit_value != range_bit_value {
                ranges.push(VhdSectorBitmapRange {
                    size: offset - range_offset,
                    is_set: range_bit_value != 0,
                });
                range_offset = offset;
                range_bit_value = bit_value;
            }

            offset += bytes_per_bit as u64;
        }
    }

    ranges.push(VhdSectorBitmapRange {
        size: offset - range_offset,
        is_set: range_bit_value != 0,
    });

    ranges
}

fn decode_ucs2_be_string(data: &[u8]) -> String {
    let mut units = Vec::new();

    for chunk in data.chunks_exact(2) {
        let unit = u16::from_be_bytes([chunk[0], chunk[1]]);

        if unit == 0 {
            break;
        }
        units.push(unit);
    }

    String::from_utf16(&units).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use keramics_core::formatters::format_as_string;
    use keramics_hashes::{DigestHashContext, Md5Context};

    use super::*;
    use crate::source::{
        DataSourceCursor, DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source,
    };

    fn read_media_from_file(file: &VhdFile) -> Result<(u64, String), ErrorTrace> {
        let source = file.open_source();
        let mut cursor = DataSourceCursor::new(source);
        let mut data = vec![0; 35_891];
        let mut md5_context = Md5Context::new();
        let mut media_offset: u64 = 0;

        loop {
            let read_count = cursor.read(&mut data)?;
            if read_count == 0 {
                break;
            }
            md5_context.update(&data[..read_count]);
            media_offset += read_count as u64;
        }

        Ok((media_offset, format_as_string(&md5_context.finalize())))
    }

    fn open_file(path: &str) -> Result<VhdFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;

        VhdFile::open(source)
    }

    fn open_file_with_parent(path: &str, parent_path: &str) -> Result<VhdFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;
        let parent_file = open_file(parent_path)?;

        VhdFile::open_with_parent(source, &parent_file)
    }

    #[test]
    fn test_open_fixed() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-parent.vhd")?;

        assert_eq!(file.disk_type(), VhdDiskType::Fixed);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-dynamic.vhd")?;

        assert_eq!(file.disk_type(), VhdDiskType::Dynamic);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.block_size(), 2_097_152);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_differential_requires_parent() -> Result<(), ErrorTrace> {
        let path_buf = PathBuf::from("../test_data/vhd/ntfs-differential.vhd");
        let source = open_local_data_source(&path_buf)?;

        let result = VhdFile::open(source);

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_open_differential_with_parent() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhd/ntfs-differential.vhd",
            "../test_data/vhd/ntfs-parent.vhd",
        )?;

        assert_eq!(file.disk_type(), VhdDiskType::Differential);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.block_size(), 2_097_152);
        assert_eq!(file.media_size(), 4_194_304);
        assert_eq!(
            file.parent_identifier().map(ToString::to_string),
            Some("e7ea9200-8493-954e-a816-9572339be931".to_string())
        );
        assert_eq!(
            file.parent_name(),
            Some("C:\\Projects\\dfvfs\\test_data\\ntfs-parent.vhd")
        );
        Ok(())
    }

    #[test]
    fn test_open_differential_with_wrong_parent_fails() -> Result<(), ErrorTrace> {
        let path_buf = PathBuf::from("../test_data/vhd/ntfs-differential.vhd");
        let source = open_local_data_source(&path_buf)?;
        let wrong_parent = open_file("../test_data/vhd/ntfs-dynamic.vhd")?;

        let result = VhdFile::open_with_parent(source, &wrong_parent);

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_open_source_fixed() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-parent.vhd")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(0, &mut data)?;

        assert_eq!(data, [0x33, 0xc0]);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities_fixed() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-parent.vhd")?;
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
        let file = open_file("../test_data/vhd/ext2.vhd")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-dynamic.vhd")?;
        let capabilities = file.open_source().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_open_source_differential() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhd/ntfs-differential.vhd",
            "../test_data/vhd/ntfs-parent.vhd",
        )?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(0, &mut data)?;

        assert_eq!(data, [0x33, 0xc0]);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities_differential() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhd/ntfs-differential.vhd",
            "../test_data/vhd/ntfs-parent.vhd",
        )?;
        let capabilities = file.open_source().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_read_media_fixed() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-parent.vhd")?;
        let (media_offset, md5_hash) = read_media_from_file(&file)?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "acb42a740c63c1f72e299463375751c8");
        Ok(())
    }

    #[test]
    fn test_read_media_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ntfs-dynamic.vhd")?;
        let (media_offset, md5_hash) = read_media_from_file(&file)?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "4ce30a0c21dd037023a5692d85ade033");
        Ok(())
    }

    #[test]
    fn test_read_media_sparse_dynamic() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/vhd/ext2.vhd")?;
        let (media_offset, md5_hash) = read_media_from_file(&file)?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "a30f111f411d3f3d567b13f0c909e58c");
        Ok(())
    }

    #[test]
    fn test_read_media_differential() -> Result<(), ErrorTrace> {
        let file = open_file_with_parent(
            "../test_data/vhd/ntfs-differential.vhd",
            "../test_data/vhd/ntfs-parent.vhd",
        )?;
        let (media_offset, md5_hash) = read_media_from_file(&file)?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "4241cbc76e0e17517fb564238edbe415");
        Ok(())
    }
}
