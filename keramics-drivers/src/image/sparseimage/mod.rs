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
use std::collections::HashSet;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};

const SPARSEIMAGE_FILE_HEADER_SIGNATURE: &[u8; 4] = b"sprs";
const SPARSEIMAGE_HEADER_BLOCK_SIZE: usize = 4096;
const SPARSEIMAGE_HEADER_DATA_SIZE: usize = 64;

#[derive(Clone, Debug)]
struct SparseImageBandMapping {
    logical_offset: u64,
    logical_size: u64,
    data_offset: u64,
}

/// Immutable sparseimage metadata plus the underlying source.
pub struct SparseImageFile {
    source: DataSourceReference,
    bytes_per_sector: u16,
    band_size: u32,
    band_numbers: Vec<u32>,
    mapped_bands: Vec<SparseImageBandMapping>,
    media_size: u64,
}

impl SparseImageFile {
    /// Opens and parses a sparseimage file.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let source_size = source.size()?;
        let mut header_data = [0u8; SPARSEIMAGE_HEADER_BLOCK_SIZE];

        source.read_exact_at(0, &mut header_data)?;

        if &header_data[0..4] != SPARSEIMAGE_FILE_HEADER_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported sparseimage file header signature".to_string(),
            ));
        }

        let sectors_per_band = u32::from_be_bytes([
            header_data[8],
            header_data[9],
            header_data[10],
            header_data[11],
        ]);
        let number_of_sectors = u32::from_be_bytes([
            header_data[16],
            header_data[17],
            header_data[18],
            header_data[19],
        ]);

        if sectors_per_band == 0 {
            return Err(ErrorTrace::new(
                "Invalid sparseimage sectors per band value: 0".to_string(),
            ));
        }

        let bytes_per_sector: u16 = 512;
        let band_size = sectors_per_band
            .checked_mul(bytes_per_sector as u32)
            .ok_or_else(|| ErrorTrace::new("Sparseimage band size overflow".to_string()))?;
        let media_size = (number_of_sectors as u64)
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("Sparseimage media size overflow".to_string()))?;
        let number_of_bands = number_of_sectors.div_ceil(sectors_per_band);

        if number_of_bands as usize
            > (SPARSEIMAGE_HEADER_BLOCK_SIZE - SPARSEIMAGE_HEADER_DATA_SIZE) / 4
        {
            return Err(ErrorTrace::new(format!(
                "Invalid sparseimage number of bands: {} value out of bounds",
                number_of_bands,
            )));
        }

        let mut band_numbers = Vec::with_capacity(number_of_bands as usize);
        let mut mapped_bands = Vec::new();
        let mut seen_band_numbers = HashSet::new();

        for array_index in 0..number_of_bands as usize {
            let data_offset = SPARSEIMAGE_HEADER_DATA_SIZE + (array_index * 4);
            let band_number = u32::from_be_bytes([
                header_data[data_offset],
                header_data[data_offset + 1],
                header_data[data_offset + 2],
                header_data[data_offset + 3],
            ]);

            band_numbers.push(band_number);

            if band_number == 0 {
                continue;
            }
            if band_number > number_of_bands {
                return Err(ErrorTrace::new(format!(
                    "Invalid sparseimage band number: {} value out of bounds",
                    band_number,
                )));
            }
            if !seen_band_numbers.insert(band_number) {
                return Err(ErrorTrace::new(format!(
                    "Duplicate sparseimage band number encountered: {}",
                    band_number,
                )));
            }

            let logical_offset = ((band_number - 1) as u64)
                .checked_mul(band_size as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("Sparseimage logical band offset overflow".to_string())
                })?;
            let logical_size = min(band_size as u64, media_size.saturating_sub(logical_offset));
            let physical_offset = (SPARSEIMAGE_HEADER_BLOCK_SIZE as u64)
                .checked_add((array_index as u64) * (band_size as u64))
                .ok_or_else(|| {
                    ErrorTrace::new("Sparseimage physical band offset overflow".to_string())
                })?;
            let physical_end_offset =
                physical_offset.checked_add(logical_size).ok_or_else(|| {
                    ErrorTrace::new("Sparseimage physical band end overflow".to_string())
                })?;

            if physical_end_offset > source_size {
                return Err(ErrorTrace::new(format!(
                    "Sparseimage band data offset exceeds source size for band number: {}",
                    band_number,
                )));
            }

            mapped_bands.push(SparseImageBandMapping {
                logical_offset,
                logical_size,
                data_offset: physical_offset,
            });
        }

        mapped_bands.sort_by_key(|mapping| mapping.logical_offset);

        Ok(Self {
            source,
            bytes_per_sector,
            band_size,
            band_numbers,
            mapped_bands,
            media_size,
        })
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the band size in bytes.
    pub fn band_size(&self) -> u32 {
        self.band_size
    }

    /// Retrieves the media size in bytes.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Retrieves the number of logical bands described by the header.
    pub fn number_of_bands(&self) -> usize {
        self.band_numbers.len()
    }

    /// Retrieves the logical band numbers listed in the header, in physical storage order.
    pub fn band_numbers(&self) -> &[u32] {
        self.band_numbers.as_slice()
    }

    /// Opens the logical media source backed by sparseimage band mappings.
    pub fn open_source(&self) -> Result<DataSourceReference, ErrorTrace> {
        let mut extents = Vec::new();
        let mut current_offset: u64 = 0;

        for band_mapping in self.mapped_bands.iter() {
            if band_mapping.logical_offset > current_offset {
                extents.push(ExtentMapEntry {
                    logical_offset: current_offset,
                    size: band_mapping.logical_offset - current_offset,
                    target: ExtentMapTarget::Zero,
                });
            }
            extents.push(ExtentMapEntry {
                logical_offset: band_mapping.logical_offset,
                size: band_mapping.logical_size,
                target: ExtentMapTarget::Data {
                    source: self.source.clone(),
                    source_offset: band_mapping.data_offset,
                },
            });
            current_offset = band_mapping
                .logical_offset
                .checked_add(band_mapping.logical_size)
                .ok_or_else(|| ErrorTrace::new("Sparseimage extent end overflow".to_string()))?;
        }

        if current_offset < self.media_size {
            extents.push(ExtentMapEntry {
                logical_offset: current_offset,
                size: self.media_size - current_offset,
                target: ExtentMapTarget::Zero,
            });
        }

        Ok(Arc::new(ExtentMapDataSource::new(extents)?))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::source::{MemoryDataSource, open_local_data_source};
    use crate::tests::get_test_data_path;

    fn get_file() -> Result<SparseImageFile, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("sparseimage/hfsplus.sparseimage"));
        let source = open_local_data_source(&path)?;

        SparseImageFile::open(source)
    }

    fn build_sparseimage_data() -> Vec<u8> {
        let sectors_per_band = 2u32;
        let bytes_per_sector = 512u32;
        let band_size = sectors_per_band * bytes_per_sector;
        let number_of_sectors = 6u32;
        let band_numbers = [2u32, 0, 1];
        let mut data =
            vec![0u8; SPARSEIMAGE_HEADER_BLOCK_SIZE + (band_numbers.len() * band_size as usize)];

        data[0..4].copy_from_slice(SPARSEIMAGE_FILE_HEADER_SIGNATURE);
        data[4..8].copy_from_slice(&3u32.to_be_bytes());
        data[8..12].copy_from_slice(&sectors_per_band.to_be_bytes());
        data[16..20].copy_from_slice(&number_of_sectors.to_be_bytes());

        for (array_index, band_number) in band_numbers.iter().enumerate() {
            let offset = SPARSEIMAGE_HEADER_DATA_SIZE + (array_index * 4);
            data[offset..offset + 4].copy_from_slice(&band_number.to_be_bytes());
        }

        let first_slot_offset = SPARSEIMAGE_HEADER_BLOCK_SIZE;
        let third_slot_offset = SPARSEIMAGE_HEADER_BLOCK_SIZE + (2 * band_size as usize);

        data[first_slot_offset..first_slot_offset + band_size as usize].fill(b'B');
        data[third_slot_offset..third_slot_offset + band_size as usize].fill(b'A');

        data
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let file = get_file()?;

        assert_eq!(file.bytes_per_sector(), 512);
        assert_eq!(file.band_size(), 1_048_576);
        assert_eq!(file.media_size(), 4_194_304);
        assert_eq!(file.number_of_bands(), 4);
        assert_eq!(file.band_numbers(), &[1, 4, 2, 3]);
        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let file = get_file()?;
        let source = file.open_source()?;
        let mut data = vec![0; 2];

        source.read_exact_at(1024, &mut data)?;

        assert_eq!(data, [0x00, 0x53]);
        Ok(())
    }

    #[test]
    fn test_open_source_uses_band_mapping_and_zero_fill() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(build_sparseimage_data()));
        let file = SparseImageFile::open(source)?;
        let logical_source = file.open_source()?;
        let data = logical_source.read_all()?;

        assert_eq!(file.band_numbers(), &[2, 0, 1]);
        assert_eq!(&data[0..4], b"AAAA");
        assert_eq!(&data[1024..1028], b"BBBB");
        assert_eq!(&data[2048..2052], &[0, 0, 0, 0]);
        Ok(())
    }
}
