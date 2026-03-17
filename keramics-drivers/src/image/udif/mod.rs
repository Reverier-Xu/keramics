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

use std::sync::{Arc, RwLock};

use keramics_compression::{AdcContext, Bzip2Context, LzfseContext, ZlibContext};
use keramics_core::ErrorTrace;
use keramics_encodings::Base64Stream;

use crate::source::{
    DataSource, DataSourceCapabilities, DataSourceReadConcurrency, DataSourceReference,
    DataSourceSeekCost, SliceDataSource,
};

const MAXIMUM_NUMBER_OF_SECTORS: u64 = u64::MAX / 512;
const UDIF_BLOCK_TABLE_HEADER_SIGNATURE: &[u8; 4] = b"mish";
const UDIF_FILE_FOOTER_SIGNATURE: &[u8; 4] = b"koly";

/// UDIF compression method.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UdifCompressionMethod {
    Adc,
    Bzip2,
    Lzfse,
    Lzma,
    None,
    Zlib,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UdifBlockRangeType {
    Compressed,
    InFile,
    Sparse,
}

#[derive(Clone)]
struct UdifBlockRange {
    media_offset: u64,
    data_offset: u64,
    size: u64,
    compressed_data_size: u32,
    range_type: UdifBlockRangeType,
}

struct UdifFileFooter {
    data_fork_offset: u64,
    data_fork_size: u64,
    plist_offset: u64,
    plist_size: u64,
}

struct UdifBlockTableHeader {
    start_sector: u64,
    number_of_entries: u32,
}

struct UdifBlockTableEntry {
    entry_type: u32,
    start_sector: u64,
    number_of_sectors: u64,
    data_offset: u64,
    data_size: u64,
}

struct UdifRuntime {
    source: DataSourceReference,
    block_ranges: Vec<UdifBlockRange>,
    compression_method: UdifCompressionMethod,
    media_size: u64,
    block_cache: RwLock<std::collections::HashMap<u64, Arc<[u8]>>>,
}

struct UdifDataSource {
    runtime: Arc<UdifRuntime>,
}

/// Immutable UDIF file metadata plus opened logical source.
pub struct UdifFile {
    bytes_per_sector: u16,
    compression_method: UdifCompressionMethod,
    media_size: u64,
    logical_source: DataSourceReference,
}

impl UdifFile {
    /// Opens and parses a UDIF file.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let source_size = source.size()?;
        if source_size < 512 {
            return Err(ErrorTrace::new(
                "Unsupported UDIF file size smaller than footer".to_string(),
            ));
        }

        let file_footer = UdifFileFooter::read_at(source.as_ref(), source_size - 512)?;
        let bytes_per_sector: u16 = 512;
        let data_fork_end_offset = file_footer
            .data_fork_offset
            .checked_add(file_footer.data_fork_size)
            .ok_or_else(|| ErrorTrace::new("UDIF data fork end offset overflow".to_string()))?;

        if data_fork_end_offset > source_size {
            return Err(ErrorTrace::new(
                "UDIF data fork exceeds source size".to_string(),
            ));
        }
        if file_footer.plist_size == 0 {
            let logical_source: DataSourceReference = Arc::new(SliceDataSource::new(
                source,
                file_footer.data_fork_offset,
                file_footer.data_fork_size,
            ));

            return Ok(Self {
                bytes_per_sector,
                compression_method: UdifCompressionMethod::None,
                media_size: file_footer.data_fork_size,
                logical_source,
            });
        }

        if file_footer.plist_size > 16_777_216 {
            return Err(ErrorTrace::new(format!(
                "Unsupported UDIF plist size: {} value out of bounds",
                file_footer.plist_size,
            )));
        }
        let plist_end_offset = file_footer
            .plist_offset
            .checked_add(file_footer.plist_size)
            .ok_or_else(|| ErrorTrace::new("UDIF plist end offset overflow".to_string()))?;

        if plist_end_offset > source_size {
            return Err(ErrorTrace::new(
                "UDIF plist exceeds source size".to_string(),
            ));
        }

        let plist_data = read_exact_to_vec(
            source.as_ref(),
            file_footer.plist_offset,
            file_footer.plist_size,
        )?;
        let plist_string = String::from_utf8(plist_data).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to convert UDIF plist data into UTF-8 string with error: {}",
                error,
            ))
        })?;
        let block_table_data = extract_blkx_data_blocks(&plist_string)?;
        let mut block_ranges = Vec::new();
        let mut media_sector: u64 = 0;
        let mut media_offset: u64 = 0;
        let mut compressed_entry_type: u32 = 0;

        for (table_index, data) in block_table_data.iter().enumerate() {
            let block_table = read_block_table(data)?;

            if block_table.start_sector != media_sector {
                return Err(ErrorTrace::new(format!(
                    "Unsupported UDIF block table: {} start sector value out of bounds",
                    table_index,
                )));
            }

            for (entry_index, block_table_entry) in block_table.entries.iter().enumerate() {
                if block_table_entry.entry_type == 0xffff_ffff {
                    break;
                }
                if block_table_entry.entry_type == 0x7fff_fffe {
                    continue;
                }
                if block_table_entry.start_sector
                    > MAXIMUM_NUMBER_OF_SECTORS - block_table.start_sector
                    || block_table.start_sector + block_table_entry.start_sector != media_sector
                {
                    return Err(ErrorTrace::new(format!(
                        "Unsupported UDIF block table: {} entry: {} start sector value out of bounds",
                        table_index, entry_index,
                    )));
                }
                if block_table_entry.number_of_sectors == 0
                    || block_table_entry.number_of_sectors > MAXIMUM_NUMBER_OF_SECTORS
                {
                    return Err(ErrorTrace::new(format!(
                        "Unsupported UDIF block table: {} entry: {} number of sectors value out of bounds",
                        table_index, entry_index,
                    )));
                }
                if block_table_entry.entry_type != 0x0000_0000
                    && block_table_entry.entry_type != 0x0000_0002
                {
                    if block_table_entry.data_offset < file_footer.data_fork_offset
                        || block_table_entry.data_offset >= data_fork_end_offset
                    {
                        return Err(ErrorTrace::new(format!(
                            "Unsupported UDIF block table: {} entry: {} data offset value out of bounds",
                            table_index, entry_index,
                        )));
                    }
                    if block_table_entry.data_size
                        > data_fork_end_offset - block_table_entry.data_offset
                    {
                        return Err(ErrorTrace::new(format!(
                            "Unsupported UDIF block table: {} entry: {} data size value out of bounds",
                            table_index, entry_index,
                        )));
                    }
                }

                let entry_media_size = block_table_entry
                    .number_of_sectors
                    .checked_mul(bytes_per_sector as u64)
                    .ok_or_else(|| ErrorTrace::new("UDIF entry media size overflow".to_string()))?;
                let block_range = match block_table_entry.entry_type {
                    0x0000_0000 | 0x0000_0002 => UdifBlockRange {
                        media_offset,
                        data_offset: 0,
                        size: entry_media_size,
                        compressed_data_size: 0,
                        range_type: UdifBlockRangeType::Sparse,
                    },
                    0x0000_0001 => UdifBlockRange {
                        media_offset,
                        data_offset: block_table_entry.data_offset,
                        size: entry_media_size,
                        compressed_data_size: 0,
                        range_type: UdifBlockRangeType::InFile,
                    },
                    0x8000_0004..=0x8000_0008 => {
                        if block_table_entry.number_of_sectors > 2048 {
                            return Err(ErrorTrace::new(format!(
                                "Unsupported compressed UDIF block table: {} entry: {} number of sectors value out of bounds",
                                table_index, entry_index,
                            )));
                        }
                        if compressed_entry_type == 0 {
                            compressed_entry_type = block_table_entry.entry_type;
                        } else if block_table_entry.entry_type != compressed_entry_type {
                            return Err(ErrorTrace::new(
                                "Unsupported UDIF mixed compression methods".to_string(),
                            ));
                        }

                        UdifBlockRange {
                            media_offset,
                            data_offset: block_table_entry.data_offset,
                            size: entry_media_size,
                            compressed_data_size: u32::try_from(block_table_entry.data_size)
                                .map_err(|_| {
                                    ErrorTrace::new(
                                        "UDIF compressed data size exceeds u32 range".to_string(),
                                    )
                                })?,
                            range_type: UdifBlockRangeType::Compressed,
                        }
                    }
                    _ => {
                        return Err(ErrorTrace::new(format!(
                            "Unsupported UDIF block table entry type: 0x{:08x}",
                            block_table_entry.entry_type,
                        )));
                    }
                };

                block_ranges.push(block_range);
                media_offset = media_offset
                    .checked_add(entry_media_size)
                    .ok_or_else(|| ErrorTrace::new("UDIF media size overflow".to_string()))?;
                media_sector = media_sector
                    .checked_add(block_table_entry.number_of_sectors)
                    .ok_or_else(|| ErrorTrace::new("UDIF media sector overflow".to_string()))?;
            }
        }

        let compression_method = match compressed_entry_type {
            0x8000_0004 => UdifCompressionMethod::Adc,
            0x8000_0005 => UdifCompressionMethod::Zlib,
            0x8000_0006 => UdifCompressionMethod::Bzip2,
            0x8000_0007 => UdifCompressionMethod::Lzfse,
            0x8000_0008 => {
                return Err(ErrorTrace::new(
                    "UDIF LZMA compression is not supported yet in keramics-drivers".to_string(),
                ));
            }
            _ => UdifCompressionMethod::None,
        };

        let logical_source: DataSourceReference = Arc::new(UdifDataSource {
            runtime: Arc::new(UdifRuntime {
                source,
                block_ranges,
                compression_method,
                media_size: media_offset,
                block_cache: RwLock::new(std::collections::HashMap::new()),
            }),
        });

        Ok(Self {
            bytes_per_sector,
            compression_method,
            media_size: media_offset,
            logical_source,
        })
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the compression method.
    pub fn compression_method(&self) -> UdifCompressionMethod {
        self.compression_method
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

impl DataSource for UdifDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if offset >= self.runtime.media_size || buf.is_empty() {
            return Ok(0);
        }

        let mut written: usize = 0;
        let mut current_offset: u64 = offset;

        while written < buf.len() && current_offset < self.runtime.media_size {
            let range = get_block_range(&self.runtime.block_ranges, current_offset)?;
            let range_relative_offset = current_offset - range.media_offset;
            let range_available = usize::try_from(range.size - range_relative_offset)
                .unwrap_or(usize::MAX)
                .min(buf.len() - written);

            match range.range_type {
                UdifBlockRangeType::Sparse => {
                    buf[written..written + range_available].fill(0);
                }
                UdifBlockRangeType::InFile => {
                    self.runtime.source.read_exact_at(
                        range.data_offset + range_relative_offset,
                        &mut buf[written..written + range_available],
                    )?;
                }
                UdifBlockRangeType::Compressed => {
                    let data = read_decompressed_block(&self.runtime, range)?;
                    let range_data_offset = range_relative_offset as usize;

                    buf[written..written + range_available].copy_from_slice(
                        &data[range_data_offset..range_data_offset + range_available],
                    );
                }
            }

            written += range_available;
            current_offset = current_offset
                .checked_add(range_available as u64)
                .ok_or_else(|| ErrorTrace::new("UDIF read offset overflow".to_string()))?;
        }

        Ok(written)
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        Ok(self.runtime.media_size)
    }

    fn capabilities(&self) -> DataSourceCapabilities {
        let seek_cost = if self.runtime.compression_method == UdifCompressionMethod::None {
            DataSourceSeekCost::Cheap
        } else {
            DataSourceSeekCost::Expensive
        };

        DataSourceCapabilities::new(DataSourceReadConcurrency::Concurrent, seek_cost)
    }

    fn telemetry_name(&self) -> &'static str {
        "udif"
    }
}

fn get_block_range(
    block_ranges: &[UdifBlockRange],
    offset: u64,
) -> Result<&UdifBlockRange, ErrorTrace> {
    let range_index = block_ranges.partition_point(|range| range.media_offset <= offset);

    if range_index == 0 {
        return Err(ErrorTrace::new(format!(
            "Missing UDIF block range for offset: {} (0x{:08x})",
            offset, offset,
        )));
    }

    Ok(&block_ranges[range_index - 1])
}

fn read_decompressed_block(
    runtime: &UdifRuntime,
    block_range: &UdifBlockRange,
) -> Result<Arc<[u8]>, ErrorTrace> {
    {
        let block_cache = runtime.block_cache.read().map_err(|_| {
            ErrorTrace::new("Unable to acquire UDIF block cache read lock".to_string())
        })?;

        if let Some(data) = block_cache.get(&block_range.data_offset) {
            return Ok(data.clone());
        }
    }

    let compressed_data = read_exact_to_vec(
        runtime.source.as_ref(),
        block_range.data_offset,
        block_range.compressed_data_size as u64,
    )?;
    let mut data = vec![0; block_range.size as usize];

    match runtime.compression_method {
        UdifCompressionMethod::Adc => {
            let mut context = AdcContext::new();
            context.decompress(&compressed_data, &mut data)?;
        }
        UdifCompressionMethod::Bzip2 => {
            let mut context = Bzip2Context::new();
            context.decompress(&compressed_data, &mut data)?;
        }
        UdifCompressionMethod::Lzfse => {
            let mut context = LzfseContext::new();
            context.decompress(&compressed_data, &mut data)?;
        }
        UdifCompressionMethod::Zlib => {
            let mut context = ZlibContext::new();
            context.decompress(&compressed_data, &mut data)?;
        }
        _ => {
            return Err(ErrorTrace::new(
                "Unsupported UDIF compression method".to_string(),
            ));
        }
    }

    let data: Arc<[u8]> = data.into();
    let mut block_cache = runtime.block_cache.write().map_err(|_| {
        ErrorTrace::new("Unable to acquire UDIF block cache write lock".to_string())
    })?;
    let entry = block_cache
        .entry(block_range.data_offset)
        .or_insert_with(|| data.clone());

    Ok(entry.clone())
}

fn extract_blkx_data_blocks(plist: &str) -> Result<Vec<Vec<u8>>, ErrorTrace> {
    let key_offset = plist.find("<key>blkx</key>").ok_or_else(|| {
        ErrorTrace::new("Unable to retrieve blkx value from UDIF plist".to_string())
    })?;
    let array_offset_relative = plist[key_offset..].find("<array>").ok_or_else(|| {
        ErrorTrace::new("Unable to retrieve blkx array from UDIF plist".to_string())
    })?;
    let array_start_offset = key_offset + array_offset_relative + "<array>".len();
    let array_end_offset_relative =
        plist[array_start_offset..]
            .find("</array>")
            .ok_or_else(|| {
                ErrorTrace::new("Unable to retrieve blkx array end from UDIF plist".to_string())
            })?;
    let array_data = &plist[array_start_offset..array_start_offset + array_end_offset_relative];
    let mut data_blocks = Vec::new();
    let mut data_offset: usize = 0;

    while let Some(start_offset_relative) = array_data[data_offset..].find("<data>") {
        let start_offset = data_offset + start_offset_relative + "<data>".len();
        let end_offset_relative = array_data[start_offset..].find("</data>").ok_or_else(|| {
            ErrorTrace::new("Missing closing data element in UDIF plist".to_string())
        })?;
        let end_offset = start_offset + end_offset_relative;
        let encoded_data = &array_data.as_bytes()[start_offset..end_offset];
        let mut base64_stream = Base64Stream::new(encoded_data, 0, true);
        let mut decoded_data = Vec::new();

        while let Some(byte_value) = base64_stream.get_value()? {
            decoded_data.push(byte_value);
        }
        data_blocks.push(decoded_data);
        data_offset = end_offset + "</data>".len();
    }

    if data_blocks.is_empty() {
        return Err(ErrorTrace::new(
            "Unable to retrieve blkx data blocks from UDIF plist".to_string(),
        ));
    }

    Ok(data_blocks)
}

struct UdifBlockTable {
    start_sector: u64,
    entries: Vec<UdifBlockTableEntry>,
}

fn read_block_table(data: &[u8]) -> Result<UdifBlockTable, ErrorTrace> {
    let header = read_block_table_header(data)?;
    let mut data_offset: usize = 204;
    let mut entries = Vec::new();

    for _ in 0..header.number_of_entries {
        let data_end_offset = data_offset + 40;
        if data_end_offset > data.len() {
            return Err(ErrorTrace::new(format!(
                "Invalid UDIF block table number of entries: {} value out of bounds",
                header.number_of_entries,
            )));
        }

        entries.push(read_block_table_entry(&data[data_offset..data_end_offset])?);
        data_offset = data_end_offset;
    }

    Ok(UdifBlockTable {
        start_sector: header.start_sector,
        entries,
    })
}

fn read_block_table_header(data: &[u8]) -> Result<UdifBlockTableHeader, ErrorTrace> {
    if data.len() < 204 {
        return Err(ErrorTrace::new(
            "Unsupported UDIF block table data size".to_string(),
        ));
    }
    if &data[0..4] != UDIF_BLOCK_TABLE_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported UDIF block table signature".to_string(),
        ));
    }

    let format_version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if format_version != 1 {
        return Err(ErrorTrace::new(format!(
            "Unsupported UDIF block table format version: {}",
            format_version,
        )));
    }

    Ok(UdifBlockTableHeader {
        start_sector: u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]),
        number_of_entries: u32::from_be_bytes([data[200], data[201], data[202], data[203]]),
    })
}

fn read_block_table_entry(data: &[u8]) -> Result<UdifBlockTableEntry, ErrorTrace> {
    if data.len() != 40 {
        return Err(ErrorTrace::new(
            "Unsupported UDIF block table entry size".to_string(),
        ));
    }

    Ok(UdifBlockTableEntry {
        entry_type: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        start_sector: u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]),
        number_of_sectors: u64::from_be_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]),
        data_offset: u64::from_be_bytes([
            data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
        ]),
        data_size: u64::from_be_bytes([
            data[32], data[33], data[34], data[35], data[36], data[37], data[38], data[39],
        ]),
    })
}

impl UdifFileFooter {
    fn read_at(source: &dyn DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..4] != UDIF_FILE_FOOTER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported UDIF file footer signature".to_string(),
            ));
        }

        let format_version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        if format_version != 4 {
            return Err(ErrorTrace::new(format!(
                "Unsupported UDIF format version: {}",
                format_version,
            )));
        }

        Ok(Self {
            data_fork_offset: u64::from_be_bytes([
                data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
            ]),
            data_fork_size: u64::from_be_bytes([
                data[32], data[33], data[34], data[35], data[36], data[37], data[38], data[39],
            ]),
            plist_offset: u64::from_be_bytes([
                data[216], data[217], data[218], data[219], data[220], data[221], data[222],
                data[223],
            ]),
            plist_size: u64::from_be_bytes([
                data[224], data[225], data[226], data[227], data[228], data[229], data[230],
                data[231],
            ]),
        })
    }
}

fn read_exact_to_vec(
    source: &dyn DataSource,
    offset: u64,
    size: u64,
) -> Result<Vec<u8>, ErrorTrace> {
    let size = usize::try_from(size)
        .map_err(|_| ErrorTrace::new("Requested UDIF read size exceeds usize range".to_string()))?;
    let mut data = vec![0; size];

    source.read_exact_at(offset, &mut data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::read_data_source_md5;

    fn open_file(path: &str) -> Result<UdifFile, ErrorTrace> {
        let path_buf = PathBuf::from(path);
        let source = open_local_data_source(&path_buf)?;

        UdifFile::open(source)
    }

    #[test]
    fn test_open_zlib() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_zlib.dmg")?;

        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.compression_method(), UdifCompressionMethod::Zlib);
        assert_eq!(file.media_size(), 1_964_032);
        Ok(())
    }

    #[test]
    fn test_open_adc() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_adc.dmg")?;

        assert_eq!(file.compression_method(), UdifCompressionMethod::Adc);
        Ok(())
    }

    #[test]
    fn test_open_bzip2() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_bzip2.dmg")?;

        assert_eq!(file.compression_method(), UdifCompressionMethod::Bzip2);
        Ok(())
    }

    #[test]
    fn test_open_lzfse() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_lzfse.dmg")?;

        assert_eq!(file.compression_method(), UdifCompressionMethod::Lzfse);
        Ok(())
    }

    #[test]
    fn test_open_lzma_unsupported() -> Result<(), ErrorTrace> {
        let path_buf = PathBuf::from("../test_data/udif/hfsplus_lzma.dmg");
        let source = open_local_data_source(&path_buf)?;

        let result = UdifFile::open(source);

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_zlib.dmg")?;
        let capabilities = file.open_source().capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Expensive);
        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_zlib.dmg")?;
        let source = file.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024, &mut data)?;

        assert_eq!(data, [0x00, 0x53]);
        Ok(())
    }

    #[test]
    fn test_read_media_adc() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_adc.dmg")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "08c32fd5d0fc1c2274d1c2d34185312a");
        Ok(())
    }

    #[test]
    fn test_read_media_bzip2() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_bzip2.dmg")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "7ec785450bbc17de417be373fd5d2159");
        Ok(())
    }

    #[test]
    fn test_read_media_lzfse() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_lzfse.dmg")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "c2c160c788676641725fd1a4b8da733b");
        Ok(())
    }

    #[test]
    fn test_read_media_zlib() -> Result<(), ErrorTrace> {
        let file = open_file("../test_data/udif/hfsplus_zlib.dmg")?;
        let (media_offset, md5_hash) = read_data_source_md5(file.open_source())?;

        assert_eq!(media_offset, file.media_size());
        assert_eq!(md5_hash.as_str(), "399bfcc39637bde7e43eb86fcc8565ae");
        Ok(())
    }
}
