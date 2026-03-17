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
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};

const QCOW_FILE_HEADER_SIGNATURE: &[u8; 4] = b"QFI\xfb";

/// QCOW compression method.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum QcowCompressionMethod {
    Zlib,
}

/// QCOW encryption method.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum QcowEncryptionMethod {
    None,
}

struct QcowFileHeader {
    format_version: u32,
    header_size: u32,
    level1_table_number_of_references: u32,
    level1_table_offset: u64,
    number_of_cluster_block_bits: u32,
    media_size: u64,
    compression_method: QcowCompressionMethod,
    encryption_method: QcowEncryptionMethod,
    backing_file_name_offset: u64,
    backing_file_name_size: u32,
    number_of_snapshots: u32,
    snapshots_offset: u64,
}

/// Immutable QCOW file metadata plus opened logical source.
pub struct QcowFile {
    format_version: u32,
    bytes_per_sector: u16,
    header_size: u32,
    cluster_block_size: u64,
    compression_method: QcowCompressionMethod,
    encryption_method: QcowEncryptionMethod,
    media_size: u64,
    logical_source: DataSourceReference,
}

impl QcowFile {
    /// Opens and parses a QCOW file.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let source_size = source.size()?;
        let header = QcowFileHeader::read_at(source.as_ref())?;

        if header.backing_file_name_offset > 0 || header.backing_file_name_size > 0 {
            return Err(ErrorTrace::new(
                "QCOW backing files are not supported yet in keramics-drivers".to_string(),
            ));
        }
        if header.number_of_snapshots != 0 || header.snapshots_offset != 0 {
            return Err(ErrorTrace::new(
                "QCOW snapshots are not supported yet in keramics-drivers".to_string(),
            ));
        }

        let number_of_cluster_block_bits = header.number_of_cluster_block_bits;
        let cluster_block_size = 1u64 << number_of_cluster_block_bits;
        let number_of_level2_table_bits = number_of_cluster_block_bits - 3;
        let level1_index_bit_shift = number_of_cluster_block_bits + number_of_level2_table_bits;
        let level2_index_bit_mask = !(u64::MAX << (number_of_level2_table_bits as u64));
        let offset_bit_mask = 0x3fff_ffff_ffff_ffff;
        let compression_flag_bit_mask = 1u64 << 62;
        let number_of_clusters = header.media_size.div_ceil(cluster_block_size);
        let mut extents = Vec::new();
        let mut current_extent: Option<ExtentMapEntry> = None;

        for cluster_index in 0..number_of_clusters {
            let logical_offset = cluster_index * cluster_block_size;
            let logical_size = min(cluster_block_size, header.media_size - logical_offset);
            let level1_index = logical_offset >> level1_index_bit_shift;

            if level1_index >= header.level1_table_number_of_references as u64 {
                return Err(ErrorTrace::new(format!(
                    "QCOW level 1 index: {} exceeds table size: {}",
                    level1_index, header.level1_table_number_of_references,
                )));
            }

            let level1_entry = read_u64_be(
                source.as_ref(),
                header
                    .level1_table_offset
                    .checked_add(level1_index * 8)
                    .ok_or_else(|| {
                        ErrorTrace::new("QCOW level 1 entry offset overflow".to_string())
                    })?,
            )?;
            let level2_table_offset = level1_entry & offset_bit_mask;

            let extent = if level2_table_offset == 0 {
                ExtentMapEntry {
                    logical_offset,
                    size: logical_size,
                    target: ExtentMapTarget::Zero,
                }
            } else {
                let level2_index =
                    (logical_offset >> number_of_cluster_block_bits) & level2_index_bit_mask;
                let level2_entry = read_u64_be(
                    source.as_ref(),
                    level2_table_offset
                        .checked_add(level2_index * 8)
                        .ok_or_else(|| {
                            ErrorTrace::new("QCOW level 2 entry offset overflow".to_string())
                        })?,
                )?;
                let block_data_offset = level2_entry & offset_bit_mask;

                if block_data_offset == 0 {
                    ExtentMapEntry {
                        logical_offset,
                        size: logical_size,
                        target: ExtentMapTarget::Zero,
                    }
                } else {
                    if (level2_entry & compression_flag_bit_mask) != 0 {
                        return Err(ErrorTrace::new(
                            "Compressed QCOW clusters are not supported yet in keramics-drivers"
                                .to_string(),
                        ));
                    }
                    let block_data_end = block_data_offset
                        .checked_add(cluster_block_size)
                        .ok_or_else(|| {
                            ErrorTrace::new("QCOW block data end offset overflow".to_string())
                        })?;

                    if block_data_end > source_size {
                        return Err(ErrorTrace::new(format!(
                            "QCOW block data exceeds file size at logical offset: {}",
                            logical_offset,
                        )));
                    }

                    ExtentMapEntry {
                        logical_offset,
                        size: logical_size,
                        target: ExtentMapTarget::Data {
                            source: source.clone(),
                            source_offset: block_data_offset,
                        },
                    }
                }
            };

            current_extent = match current_extent.take() {
                Some(mut current_extent) => {
                    let can_merge = match (&current_extent.target, &extent.target) {
                        (ExtentMapTarget::Zero, ExtentMapTarget::Zero) => true,
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
                                    == extent.logical_offset
                                && current_source_offset + current_extent.size
                                    == *next_source_offset
                        }
                        _ => false,
                    };

                    if can_merge {
                        current_extent.size += extent.size;
                        Some(current_extent)
                    } else {
                        extents.push(current_extent);
                        Some(extent)
                    }
                }
                None => Some(extent),
            };
        }

        if let Some(current_extent) = current_extent {
            extents.push(current_extent);
        }

        let logical_source: DataSourceReference = Arc::new(ExtentMapDataSource::new(extents)?);

        Ok(Self {
            format_version: header.format_version,
            bytes_per_sector: 512,
            header_size: header.header_size,
            cluster_block_size,
            compression_method: header.compression_method,
            encryption_method: header.encryption_method,
            media_size: header.media_size,
            logical_source,
        })
    }

    /// Retrieves the format version.
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the header size.
    pub fn header_size(&self) -> u32 {
        self.header_size
    }

    /// Retrieves the cluster size.
    pub fn cluster_block_size(&self) -> u64 {
        self.cluster_block_size
    }

    /// Retrieves the compression method.
    pub fn compression_method(&self) -> QcowCompressionMethod {
        self.compression_method
    }

    /// Retrieves the encryption method.
    pub fn encryption_method(&self) -> QcowEncryptionMethod {
        self.encryption_method
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

impl QcowFileHeader {
    fn read_at(source: &dyn crate::source::DataSource) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 112];

        source.read_exact_at(0, &mut data)?;

        if &data[0..4] != QCOW_FILE_HEADER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported QCOW file signature".to_string(),
            ));
        }

        let format_version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        if format_version != 3 {
            return Err(ErrorTrace::new(format!(
                "Unsupported QCOW format version: {}",
                format_version,
            )));
        }

        let supported_flags: u64 = 1;
        let incompatible_feature_flags = u64::from_be_bytes([
            data[72], data[73], data[74], data[75], data[76], data[77], data[78], data[79],
        ]);
        let compatible_feature_flags = u64::from_be_bytes([
            data[80], data[81], data[82], data[83], data[84], data[85], data[86], data[87],
        ]);

        if incompatible_feature_flags & !supported_flags != 0 {
            return Err(ErrorTrace::new(format!(
                "Unsupported QCOW incompatible feature flags: 0x{:016x}",
                incompatible_feature_flags,
            )));
        }
        if compatible_feature_flags & !supported_flags != 0 {
            return Err(ErrorTrace::new(format!(
                "Unsupported QCOW compatible feature flags: 0x{:016x}",
                compatible_feature_flags,
            )));
        }

        let number_of_cluster_block_bits =
            u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        if number_of_cluster_block_bits <= 8 || number_of_cluster_block_bits > 63 {
            return Err(ErrorTrace::new(format!(
                "Invalid QCOW number of cluster block bits: {} value out of bounds",
                number_of_cluster_block_bits,
            )));
        }

        let header_size = u32::from_be_bytes([data[100], data[101], data[102], data[103]]);
        if header_size != 104 && header_size != 112 {
            return Err(ErrorTrace::new(format!(
                "Unsupported QCOW header size: {}",
                header_size,
            )));
        }

        let compression_method = match data[104] {
            0 => QcowCompressionMethod::Zlib,
            value => {
                return Err(ErrorTrace::new(format!(
                    "Unsupported QCOW compression method: {}",
                    value,
                )));
            }
        };
        let encryption_method = match u32::from_be_bytes([data[32], data[33], data[34], data[35]]) {
            0 => QcowEncryptionMethod::None,
            value => {
                return Err(ErrorTrace::new(format!(
                    "Unsupported QCOW encryption method: {}",
                    value,
                )));
            }
        };

        Ok(Self {
            format_version,
            header_size,
            level1_table_number_of_references: u32::from_be_bytes([
                data[36], data[37], data[38], data[39],
            ]),
            level1_table_offset: u64::from_be_bytes([
                data[40], data[41], data[42], data[43], data[44], data[45], data[46], data[47],
            ]),
            number_of_cluster_block_bits,
            media_size: u64::from_be_bytes([
                data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
            ]),
            compression_method,
            encryption_method,
            backing_file_name_offset: u64::from_be_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]),
            backing_file_name_size: u32::from_be_bytes([data[16], data[17], data[18], data[19]]),
            number_of_snapshots: u32::from_be_bytes([data[60], data[61], data[62], data[63]]),
            snapshots_offset: u64::from_be_bytes([
                data[64], data[65], data[66], data[67], data[68], data[69], data[70], data[71],
            ]),
        })
    }
}

fn read_u64_be(source: &dyn crate::source::DataSource, offset: u64) -> Result<u64, ErrorTrace> {
    let mut data = [0u8; 8];

    source.read_exact_at(offset, &mut data)?;

    Ok(u64::from_be_bytes(data))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::read_data_source_md5;

    fn open_file(path: &str) -> Result<QcowFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;

        QcowFile::open(source)
    }

    #[test]
    fn test_open_ext2() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/qcow/ext2.qcow2")?;

        assert_eq!(file.format_version(), 3);
        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.header_size(), 112);
        assert_eq!(file.cluster_block_size(), 65_536);
        assert_eq!(file.compression_method(), QcowCompressionMethod::Zlib);
        assert_eq!(file.encryption_method(), QcowEncryptionMethod::None);
        assert_eq!(file.media_size(), 4_194_304);
        Ok(())
    }

    #[test]
    fn test_open_fat_variants() -> Result<(), ErrorTrace> {
        let fat16 = open_file("../test_data/qcow/fat16.qcow2")?;
        let fat32 = open_file("../test_data/qcow/fat32.qcow2")?;

        assert_eq!(fat16.media_size(), 16_777_216);
        assert_eq!(fat32.media_size(), 67_108_864);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/qcow/ext2.qcow2")?;
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
        let file = open_file("../test_data/qcow/ext2.qcow2")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_read_media() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/qcow/ext2.qcow2")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "b1760d0b35a512ef56970df4e6f8c5d6");
        Ok(())
    }
}
