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

use keramics_checksums::ReversedCrc32Context;
use keramics_core::ErrorTrace;
use keramics_types::{Utf16String, Uuid};

use super::partition::GptPartition;
use crate::source::{DataSource, DataSourceReference};

const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";

/// Immutable GUID Partition Table metadata.
pub struct GptVolumeSystem {
    bytes_per_sector: u16,
    disk_identifier: Uuid,
    partitions: Vec<GptPartition>,
}

#[derive(Clone, Debug)]
struct GptPartitionTableHeader {
    header_block_number: u64,
    backup_header_block_number: u64,
    area_start_block_number: u64,
    area_end_block_number: u64,
    disk_identifier: Uuid,
    entries_start_block_number: u64,
    number_of_entries: u32,
    entry_data_size: u32,
    entries_data_checksum: u32,
}

#[derive(Clone, Debug)]
struct GptPartitionEntry {
    index: usize,
    type_identifier: Uuid,
    identifier: Uuid,
    start_block_number: u64,
    end_block_number: u64,
    attribute_flags: u64,
    name: String,
}

impl GptPartitionTableHeader {
    fn read_at(
        source: &dyn DataSource,
        offset: u64,
        bytes_per_sector: u16,
    ) -> Result<Self, ErrorTrace> {
        let sector_size: usize = bytes_per_sector as usize;
        let mut data = vec![0; sector_size];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..8] != GPT_SIGNATURE {
            return Err(ErrorTrace::new("Unsupported GPT signature".to_string()));
        }

        let format_version: u32 = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        if format_version != 0x0001_0000 {
            return Err(ErrorTrace::new(format!(
                "Unsupported GPT format version: 0x{:08x}",
                format_version,
            )));
        }

        let header_data_size: usize =
            u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;

        if header_data_size < 92 || header_data_size > sector_size {
            return Err(ErrorTrace::new(format!(
                "Unsupported GPT header size: {}",
                header_data_size,
            )));
        }

        let stored_checksum = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let mut checksum_data = data[..header_data_size].to_vec();
        checksum_data[16..20].fill(0);

        let mut checksum_context = ReversedCrc32Context::new(0xedb8_8320, 0);
        checksum_context.update(&checksum_data);

        let calculated_checksum = checksum_context.finalize();

        if stored_checksum != 0 && stored_checksum != calculated_checksum {
            return Err(ErrorTrace::new(format!(
                "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} GPT header checksums",
                stored_checksum, calculated_checksum,
            )));
        }

        Ok(Self {
            header_block_number: u64::from_le_bytes([
                data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
            ]),
            backup_header_block_number: u64::from_le_bytes([
                data[32], data[33], data[34], data[35], data[36], data[37], data[38], data[39],
            ]),
            area_start_block_number: u64::from_le_bytes([
                data[40], data[41], data[42], data[43], data[44], data[45], data[46], data[47],
            ]),
            area_end_block_number: u64::from_le_bytes([
                data[48], data[49], data[50], data[51], data[52], data[53], data[54], data[55],
            ]),
            disk_identifier: Uuid::from_le_bytes(&data[56..72]),
            entries_start_block_number: u64::from_le_bytes([
                data[72], data[73], data[74], data[75], data[76], data[77], data[78], data[79],
            ]),
            number_of_entries: u32::from_le_bytes([data[80], data[81], data[82], data[83]]),
            entry_data_size: u32::from_le_bytes([data[84], data[85], data[86], data[87]]),
            entries_data_checksum: u32::from_le_bytes([data[88], data[89], data[90], data[91]]),
        })
    }
}

impl GptPartitionEntry {
    fn read_data(index: usize, data: &[u8], entry_data_size: u32) -> Result<Self, ErrorTrace> {
        if data.len() != entry_data_size as usize || entry_data_size < 128 {
            return Err(ErrorTrace::new(
                "Unsupported GPT partition entry size".to_string(),
            ));
        }

        let name = Utf16String::from_le_bytes(&data[56..128]).to_string();

        Ok(Self {
            index,
            type_identifier: Uuid::from_le_bytes(&data[0..16]),
            identifier: Uuid::from_le_bytes(&data[16..32]),
            start_block_number: u64::from_le_bytes([
                data[32], data[33], data[34], data[35], data[36], data[37], data[38], data[39],
            ]),
            end_block_number: u64::from_le_bytes([
                data[40], data[41], data[42], data[43], data[44], data[45], data[46], data[47],
            ]),
            attribute_flags: u64::from_le_bytes([
                data[48], data[49], data[50], data[51], data[52], data[53], data[54], data[55],
            ]),
            name,
        })
    }

    fn is_empty(&self) -> bool {
        self.type_identifier.is_nil()
    }
}

impl GptVolumeSystem {
    const SUPPORTED_BYTES_PER_SECTOR: [u16; 4] = [512, 4096, 2048, 1024];

    /// Opens and parses a GUID Partition Table.
    pub fn open(source: &DataSourceReference) -> Result<Self, ErrorTrace> {
        let mut last_error: Option<ErrorTrace> = None;

        for bytes_per_sector in Self::SUPPORTED_BYTES_PER_SECTOR {
            match Self::open_with_bytes_per_sector(source, bytes_per_sector) {
                Ok(volume_system) => return Ok(volume_system),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ErrorTrace::new("Unable to determine GPT bytes-per-sector value".to_string())
        }))
    }

    /// Opens and parses a GUID Partition Table with a fixed bytes-per-sector value.
    pub fn open_with_bytes_per_sector(
        source: &DataSourceReference,
        bytes_per_sector: u16,
    ) -> Result<Self, ErrorTrace> {
        if !Self::SUPPORTED_BYTES_PER_SECTOR.contains(&bytes_per_sector) {
            return Err(ErrorTrace::new(format!(
                "Unsupported GPT bytes-per-sector value: {}",
                bytes_per_sector,
            )));
        }

        let source_size = source.size()?;
        let primary_header = GptPartitionTableHeader::read_at(
            source.as_ref(),
            bytes_per_sector as u64,
            bytes_per_sector,
        )?;

        if primary_header.header_block_number != 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported GPT primary header block number: {}",
                primary_header.header_block_number,
            )));
        }
        if primary_header.area_end_block_number < primary_header.area_start_block_number {
            return Err(ErrorTrace::new(
                "Invalid GPT usable area block range".to_string(),
            ));
        }
        if primary_header.number_of_entries == 0 {
            return Err(ErrorTrace::new(
                "Unsupported GPT number of partition entries: 0".to_string(),
            ));
        }
        if primary_header.entry_data_size < 128 || primary_header.entry_data_size % 8 != 0 {
            return Err(ErrorTrace::new(format!(
                "Unsupported GPT entry data size: {}",
                primary_header.entry_data_size,
            )));
        }

        let _ = Self::read_backup_header(
            source.as_ref(),
            source_size,
            bytes_per_sector,
            &primary_header,
        )?;
        let partitions =
            Self::read_partition_entries(source.as_ref(), bytes_per_sector, &primary_header)?;

        Ok(Self {
            bytes_per_sector,
            disk_identifier: primary_header.disk_identifier,
            partitions,
        })
    }

    /// Retrieves the disk identifier.
    pub fn disk_identifier(&self) -> &Uuid {
        &self.disk_identifier
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves all parsed partitions.
    pub fn partitions(&self) -> &[GptPartition] {
        self.partitions.as_slice()
    }

    /// Retrieves a partition by index.
    pub fn partition(&self, partition_index: usize) -> Result<&GptPartition, ErrorTrace> {
        self.partitions.get(partition_index).ok_or_else(|| {
            ErrorTrace::new(format!("No GPT partition with index: {}", partition_index))
        })
    }

    fn read_backup_header(
        source: &dyn DataSource,
        source_size: u64,
        bytes_per_sector: u16,
        primary_header: &GptPartitionTableHeader,
    ) -> Result<GptPartitionTableHeader, ErrorTrace> {
        let backup_offset = primary_header
            .backup_header_block_number
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("GPT backup header offset overflow".to_string()))?;

        match GptPartitionTableHeader::read_at(source, backup_offset, bytes_per_sector) {
            Ok(backup_header) => Ok(backup_header),
            Err(_) => {
                let fallback_offset = source_size
                    .checked_sub(bytes_per_sector as u64)
                    .ok_or_else(|| {
                        ErrorTrace::new("GPT source is smaller than one sector".to_string())
                    })?;

                GptPartitionTableHeader::read_at(source, fallback_offset, bytes_per_sector)
            }
        }
    }

    fn read_partition_entries(
        source: &dyn DataSource,
        bytes_per_sector: u16,
        header: &GptPartitionTableHeader,
    ) -> Result<Vec<GptPartition>, ErrorTrace> {
        let entry_data_size = header.entry_data_size as usize;
        let total_entries_size = (header.number_of_entries as u64)
            .checked_mul(header.entry_data_size as u64)
            .ok_or_else(|| {
                ErrorTrace::new("GPT partition entry table size overflow".to_string())
            })?;
        let entries_start_offset = header
            .entries_start_block_number
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| {
                ErrorTrace::new("GPT partition entry start offset overflow".to_string())
            })?;
        let first_usable_offset = header
            .area_start_block_number
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("GPT first usable offset overflow".to_string()))?;

        if entries_start_offset > first_usable_offset {
            return Err(ErrorTrace::new(
                "GPT partition entries start inside the usable area".to_string(),
            ));
        }
        if entries_start_offset + total_entries_size > first_usable_offset {
            return Err(ErrorTrace::new(
                "GPT partition entry table overlaps the usable area".to_string(),
            ));
        }

        let mut checksum_context = ReversedCrc32Context::new(0xedb8_8320, 0);
        let mut partitions = Vec::new();
        let mut previous_end_block = None;

        for entry_index in 0..header.number_of_entries as usize {
            let entry_offset = entries_start_offset
                .checked_add((entry_index as u64) * header.entry_data_size as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("GPT partition entry offset overflow".to_string())
                })?;
            let mut entry_data = vec![0; entry_data_size];

            source.read_exact_at(entry_offset, &mut entry_data)?;
            checksum_context.update(&entry_data);

            let partition_entry =
                GptPartitionEntry::read_data(entry_index, &entry_data, header.entry_data_size)?;

            if partition_entry.is_empty() {
                continue;
            }
            if partition_entry.start_block_number < header.area_start_block_number
                || partition_entry.end_block_number > header.area_end_block_number
            {
                return Err(ErrorTrace::new(format!(
                    "GPT partition entry {} block range is out of bounds",
                    entry_index,
                )));
            }
            if partition_entry.end_block_number < partition_entry.start_block_number {
                return Err(ErrorTrace::new(format!(
                    "GPT partition entry {} end block number is before start block number",
                    entry_index,
                )));
            }
            if let Some(previous_end_block) = previous_end_block
                && partition_entry.start_block_number <= previous_end_block
            {
                return Err(ErrorTrace::new(
                    "Unsupported overlapping GPT partition entries".to_string(),
                ));
            }

            let partition_offset = partition_entry
                .start_block_number
                .checked_mul(bytes_per_sector as u64)
                .ok_or_else(|| ErrorTrace::new("GPT partition offset overflow".to_string()))?;
            let partition_size_in_blocks = partition_entry
                .end_block_number
                .checked_sub(partition_entry.start_block_number)
                .and_then(|size| size.checked_add(1))
                .ok_or_else(|| ErrorTrace::new("GPT partition size overflow".to_string()))?;
            let partition_size = partition_size_in_blocks
                .checked_mul(bytes_per_sector as u64)
                .ok_or_else(|| ErrorTrace::new("GPT partition byte size overflow".to_string()))?;

            partitions.push(GptPartition::new(
                partition_entry.index,
                partition_offset,
                partition_size,
                partition_entry.type_identifier,
                partition_entry.identifier,
                partition_entry.attribute_flags,
                partition_entry.name,
            ));
            previous_end_block = Some(
                partitions.last().unwrap().offset() / (bytes_per_sector as u64)
                    + (partitions.last().unwrap().size() / (bytes_per_sector as u64))
                    - 1,
            );
        }

        let calculated_checksum = checksum_context.finalize();

        if header.entries_data_checksum != 0 && header.entries_data_checksum != calculated_checksum
        {
            return Err(ErrorTrace::new(format!(
                "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} GPT partition entry checksums",
                header.entries_data_checksum, calculated_checksum,
            )));
        }

        Ok(partitions)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{MemoryDataSource, open_local_data_source};
    use crate::tests::get_test_data_path;

    fn get_volume_system() -> Result<GptVolumeSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("gpt/gpt.raw"));
        let source = open_local_data_source(&path)?;

        GptVolumeSystem::open(&source)
    }

    fn reversed_crc32(data: &[u8]) -> u32 {
        let mut context = ReversedCrc32Context::new(0xedb8_8320, 0);
        context.update(data);
        context.finalize()
    }

    fn build_gpt_data(bytes_per_sector: u16) -> Vec<u8> {
        let bytes_per_sector = bytes_per_sector as usize;
        let total_sectors = 64usize;
        let mut data = vec![0u8; total_sectors * bytes_per_sector];
        let first_usable_lba = 6u64;
        let last_usable_lba = 62u64;
        let backup_header_lba = 63u64;
        let entries_start_lba = 2u64;
        let entry_data_size = 128u32;
        let entry_count = 4u32;
        let entries_size = (entry_data_size * entry_count) as usize;

        let type_guid = Uuid::from_string("0fc63daf-8483-4772-8e79-3d69d8477de4").unwrap();
        let part_guid = Uuid::from_string("1e25588c-27a9-4094-868c-2f257021f87b").unwrap();
        let disk_guid = Uuid::from_string("e86e657a-d840-4c09-afe3-a1a5f665cf44").unwrap();

        fn uuid_to_le_bytes(uuid: &Uuid) -> [u8; 16] {
            let mut data = [0u8; 16];

            data[0..4].copy_from_slice(&uuid.part1.to_le_bytes());
            data[4..6].copy_from_slice(&uuid.part2.to_le_bytes());
            data[6..8].copy_from_slice(&uuid.part3.to_le_bytes());
            data[8..10].copy_from_slice(&uuid.part4.to_be_bytes());
            data[10..12].copy_from_slice(&((uuid.part5 >> 32) as u16).to_be_bytes());
            data[12..16].copy_from_slice(&(uuid.part5 as u32).to_be_bytes());
            data
        }

        let entries_offset = entries_start_lba as usize * bytes_per_sector;
        let mut entry = vec![0u8; entry_data_size as usize];
        entry[0..16].copy_from_slice(&uuid_to_le_bytes(&type_guid));
        entry[16..32].copy_from_slice(&uuid_to_le_bytes(&part_guid));
        entry[32..40].copy_from_slice(&8u64.to_le_bytes());
        entry[40..48].copy_from_slice(&15u64.to_le_bytes());
        entry[48..56].copy_from_slice(&0u64.to_le_bytes());
        let mut name_bytes = [0u8; 72];
        for (index, value) in "gpt_test".encode_utf16().enumerate() {
            name_bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        entry[56..128].copy_from_slice(&name_bytes);

        data[entries_offset..entries_offset + entry.len()].copy_from_slice(&entry);
        let entries_checksum = reversed_crc32(&data[entries_offset..entries_offset + entries_size]);

        struct HeaderSpec<'a> {
            current_lba: u64,
            backup_lba: u64,
            first_usable_lba: u64,
            last_usable_lba: u64,
            disk_guid: &'a Uuid,
            entries_start_lba: u64,
            entry_count: u32,
            entry_data_size: u32,
            entries_checksum: u32,
        }

        fn write_header(
            data: &mut [u8],
            offset: usize,
            bytes_per_sector: usize,
            spec: &HeaderSpec<'_>,
        ) {
            fn uuid_to_le_bytes(uuid: &Uuid) -> [u8; 16] {
                let mut data = [0u8; 16];

                data[0..4].copy_from_slice(&uuid.part1.to_le_bytes());
                data[4..6].copy_from_slice(&uuid.part2.to_le_bytes());
                data[6..8].copy_from_slice(&uuid.part3.to_le_bytes());
                data[8..10].copy_from_slice(&uuid.part4.to_be_bytes());
                data[10..12].copy_from_slice(&((uuid.part5 >> 32) as u16).to_be_bytes());
                data[12..16].copy_from_slice(&(uuid.part5 as u32).to_be_bytes());
                data
            }

            let mut header = vec![0u8; bytes_per_sector];
            header[0..8].copy_from_slice(GPT_SIGNATURE);
            header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            header[12..16].copy_from_slice(&92u32.to_le_bytes());
            header[24..32].copy_from_slice(&spec.current_lba.to_le_bytes());
            header[32..40].copy_from_slice(&spec.backup_lba.to_le_bytes());
            header[40..48].copy_from_slice(&spec.first_usable_lba.to_le_bytes());
            header[48..56].copy_from_slice(&spec.last_usable_lba.to_le_bytes());
            header[56..72].copy_from_slice(&uuid_to_le_bytes(spec.disk_guid));
            header[72..80].copy_from_slice(&spec.entries_start_lba.to_le_bytes());
            header[80..84].copy_from_slice(&spec.entry_count.to_le_bytes());
            header[84..88].copy_from_slice(&spec.entry_data_size.to_le_bytes());
            header[88..92].copy_from_slice(&spec.entries_checksum.to_le_bytes());

            let mut checksum_header = header[..92].to_vec();
            checksum_header[16..20].fill(0);
            let checksum = reversed_crc32(&checksum_header);
            header[16..20].copy_from_slice(&checksum.to_le_bytes());

            data[offset..offset + bytes_per_sector].copy_from_slice(&header);
        }

        let primary_header = HeaderSpec {
            current_lba: 1,
            backup_lba: backup_header_lba,
            first_usable_lba,
            last_usable_lba,
            disk_guid: &disk_guid,
            entries_start_lba,
            entry_count,
            entry_data_size,
            entries_checksum,
        };
        let backup_header = HeaderSpec {
            current_lba: backup_header_lba,
            backup_lba: 1,
            first_usable_lba,
            last_usable_lba,
            disk_guid: &disk_guid,
            entries_start_lba,
            entry_count,
            entry_data_size,
            entries_checksum,
        };

        write_header(
            &mut data,
            bytes_per_sector,
            bytes_per_sector,
            &primary_header,
        );
        write_header(
            &mut data,
            backup_header_lba as usize * bytes_per_sector,
            bytes_per_sector,
            &backup_header,
        );

        data
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let volume_system = get_volume_system()?;

        assert_eq!(volume_system.partitions().len(), 2);
        assert_eq!(volume_system.bytes_per_sector(), 512);
        Ok(())
    }

    #[test]
    fn test_partition() -> Result<(), ErrorTrace> {
        let volume_system = get_volume_system()?;
        let partition = volume_system.partition(0)?;

        assert_eq!(partition.entry_index(), 0);
        assert_eq!(partition.offset(), 1_048_576);
        assert_eq!(partition.size(), 1_048_576);
        assert_eq!(partition.name(), "Linux filesystem");
        Ok(())
    }

    #[test]
    fn test_partition_open_source() -> Result<(), ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("gpt/gpt.raw"));
        let source = open_local_data_source(&path)?;
        let volume_system = GptVolumeSystem::open(&source)?;
        let partition_source = volume_system.partition(0)?.open_source(source);
        let mut data = vec![0; 2];

        partition_source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_open_with_4096_byte_sectors() -> Result<(), ErrorTrace> {
        let source: DataSourceReference =
            std::sync::Arc::new(MemoryDataSource::new(build_gpt_data(4096)));
        let volume_system = GptVolumeSystem::open_with_bytes_per_sector(&source, 4096)?;
        let partition = volume_system.partition(0)?;

        assert_eq!(volume_system.bytes_per_sector(), 4096);
        assert_eq!(partition.offset(), 8 * 4096);
        assert_eq!(partition.size(), 8 * 4096);
        assert_eq!(partition.name(), "gpt_test");
        Ok(())
    }
}
