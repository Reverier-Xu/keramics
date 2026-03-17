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

use std::collections::HashSet;

use keramics_core::ErrorTrace;

use super::partition::MbrPartition;
use crate::source::{DataSource, DataSourceReference};

const MBR_BOOT_SIGNATURE: [u8; 2] = [0x55, 0xaa];
const MBR_PARTITION_ENTRY_OFFSET: usize = 446;
const MBR_PARTITION_ENTRY_SIZE: usize = 16;
const MBR_PARTITION_ENTRY_COUNT: usize = 4;
const MBR_SECTOR_SIZE: usize = 512;

/// Immutable Master Boot Record volume system metadata.
pub struct MbrVolumeSystem {
    bytes_per_sector: u32,
    disk_identity: u32,
    partitions: Vec<MbrPartition>,
}

#[derive(Clone, Copy, Debug, Default)]
struct MbrPartitionEntry {
    flags: u8,
    partition_type: u8,
    start_address_lba: u32,
    number_of_sectors: u32,
}

#[derive(Clone, Debug)]
struct MbrBootRecord {
    disk_identity: u32,
    partition_entries: [MbrPartitionEntry; MBR_PARTITION_ENTRY_COUNT],
}

impl MbrPartitionEntry {
    fn read_data(data: &[u8]) -> Result<Self, ErrorTrace> {
        if data.len() != MBR_PARTITION_ENTRY_SIZE {
            return Err(ErrorTrace::new(
                "Unsupported MBR partition entry size".to_string(),
            ));
        }

        Ok(Self {
            flags: data[0],
            partition_type: data[4],
            start_address_lba: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            number_of_sectors: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        })
    }

    fn is_empty(&self) -> bool {
        self.partition_type == 0 || self.number_of_sectors == 0
    }

    fn is_extended(&self) -> bool {
        matches!(self.partition_type, 0x05 | 0x0f)
    }
}

impl MbrBootRecord {
    fn read_at(source: &dyn DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data: [u8; MBR_SECTOR_SIZE] = [0; MBR_SECTOR_SIZE];

        source.read_exact_at(offset, &mut data)?;

        if data[510..512] != MBR_BOOT_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported MBR boot signature".to_string(),
            ));
        }

        let mut partition_entries = [MbrPartitionEntry::default(); MBR_PARTITION_ENTRY_COUNT];

        for (entry_index, partition_entry) in partition_entries.iter_mut().enumerate() {
            let entry_offset =
                MBR_PARTITION_ENTRY_OFFSET + (entry_index * MBR_PARTITION_ENTRY_SIZE);

            *partition_entry = MbrPartitionEntry::read_data(
                &data[entry_offset..entry_offset + MBR_PARTITION_ENTRY_SIZE],
            )?;
        }

        Ok(Self {
            disk_identity: u32::from_le_bytes([data[440], data[441], data[442], data[443]]),
            partition_entries,
        })
    }
}

impl MbrVolumeSystem {
    const SUPPORTED_BYTES_PER_SECTOR: [u32; 4] = [512, 4096, 2048, 1024];
    const MAX_LOGICAL_PARTITIONS: usize = 1024;

    /// Opens and parses a Master Boot Record volume system.
    pub fn open(source: &DataSourceReference) -> Result<Self, ErrorTrace> {
        let mut last_error: Option<ErrorTrace> = None;

        for bytes_per_sector in Self::SUPPORTED_BYTES_PER_SECTOR {
            match Self::open_with_bytes_per_sector(source, bytes_per_sector) {
                Ok(volume_system) => return Ok(volume_system),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ErrorTrace::new("Unable to determine MBR bytes-per-sector value".to_string())
        }))
    }

    /// Opens and parses a Master Boot Record volume system with a fixed bytes-per-sector value.
    pub fn open_with_bytes_per_sector(
        source: &DataSourceReference,
        bytes_per_sector: u32,
    ) -> Result<Self, ErrorTrace> {
        if !Self::SUPPORTED_BYTES_PER_SECTOR.contains(&bytes_per_sector) {
            return Err(ErrorTrace::new(format!(
                "Unsupported MBR bytes-per-sector value: {}",
                bytes_per_sector,
            )));
        }

        let source_size = source.size()?;
        let boot_record = MbrBootRecord::read_at(source.as_ref(), 0)?;
        let mut partitions = Vec::new();
        let mut next_partition_index: usize = 0;
        let mut extended_partition_offset: Option<u64> = None;

        for partition_entry in boot_record.partition_entries.iter() {
            if partition_entry.is_empty() {
                continue;
            }

            if partition_entry.is_extended() {
                if extended_partition_offset.is_some() {
                    return Err(ErrorTrace::new(
                        "More than one extended partition entry is not supported".to_string(),
                    ));
                }
                extended_partition_offset = Some(Self::lba_to_offset(
                    partition_entry.start_address_lba,
                    bytes_per_sector,
                )?);
                continue;
            }

            partitions.push(Self::partition_from_entry(
                next_partition_index,
                partition_entry,
                Self::lba_to_offset(partition_entry.start_address_lba, bytes_per_sector)?,
                bytes_per_sector,
            )?);
            next_partition_index += 1;
        }

        if let Some(extended_partition_offset) = extended_partition_offset {
            Self::read_extended_partition_chain(
                source.as_ref(),
                bytes_per_sector,
                extended_partition_offset,
                &mut next_partition_index,
                &mut partitions,
            )?;
        }

        Self::validate_partitions(&partitions, source_size)?;

        Ok(Self {
            bytes_per_sector,
            disk_identity: boot_record.disk_identity,
            partitions,
        })
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u32 {
        self.bytes_per_sector
    }

    /// Retrieves the disk identity.
    pub fn disk_identity(&self) -> u32 {
        self.disk_identity
    }

    /// Retrieves all parsed partitions.
    pub fn partitions(&self) -> &[MbrPartition] {
        self.partitions.as_slice()
    }

    /// Retrieves a partition by index.
    pub fn partition(&self, partition_index: usize) -> Result<&MbrPartition, ErrorTrace> {
        self.partitions.get(partition_index).ok_or_else(|| {
            ErrorTrace::new(format!("No MBR partition with index: {}", partition_index))
        })
    }

    fn lba_to_offset(start_address_lba: u32, bytes_per_sector: u32) -> Result<u64, ErrorTrace> {
        (start_address_lba as u64)
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("MBR offset overflow".to_string()))
    }

    fn partition_from_entry(
        entry_index: usize,
        partition_entry: &MbrPartitionEntry,
        offset: u64,
        bytes_per_sector: u32,
    ) -> Result<MbrPartition, ErrorTrace> {
        let size = (partition_entry.number_of_sectors as u64)
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("MBR partition size overflow".to_string()))?;

        Ok(MbrPartition::new(
            entry_index,
            offset,
            size,
            partition_entry.partition_type,
            partition_entry.flags,
        ))
    }

    fn read_extended_partition_chain(
        source: &dyn DataSource,
        bytes_per_sector: u32,
        extended_partition_offset: u64,
        next_partition_index: &mut usize,
        partitions: &mut Vec<MbrPartition>,
    ) -> Result<(), ErrorTrace> {
        let mut visited_offsets: HashSet<u64> = HashSet::new();
        let mut current_ebr_offset = extended_partition_offset;

        while visited_offsets.len() < Self::MAX_LOGICAL_PARTITIONS {
            if !visited_offsets.insert(current_ebr_offset) {
                return Err(ErrorTrace::new(
                    "Detected cycle in MBR extended partition chain".to_string(),
                ));
            }

            let boot_record = MbrBootRecord::read_at(source, current_ebr_offset)?;
            let mut logical_partition_entry: Option<MbrPartitionEntry> = None;
            let mut next_ebr_offset: Option<u64> = None;

            for partition_entry in boot_record.partition_entries.iter() {
                if partition_entry.is_empty() {
                    continue;
                }

                if partition_entry.is_extended() {
                    if next_ebr_offset.is_some() {
                        return Err(ErrorTrace::new(
                            "More than one extended partition link per EBR is not supported"
                                .to_string(),
                        ));
                    }

                    next_ebr_offset = Some(
                        extended_partition_offset
                            .checked_add(Self::lba_to_offset(
                                partition_entry.start_address_lba,
                                bytes_per_sector,
                            )?)
                            .ok_or_else(|| {
                                ErrorTrace::new(
                                    "MBR extended partition offset overflow".to_string(),
                                )
                            })?,
                    );
                } else if logical_partition_entry.is_some() {
                    return Err(ErrorTrace::new(
                        "More than one logical partition entry per EBR is not supported"
                            .to_string(),
                    ));
                } else {
                    logical_partition_entry = Some(*partition_entry);
                }
            }

            let Some(logical_partition_entry) = logical_partition_entry else {
                return Err(ErrorTrace::new(
                    "Encountered EBR without a logical partition entry".to_string(),
                ));
            };

            let logical_partition_offset = current_ebr_offset
                .checked_add(Self::lba_to_offset(
                    logical_partition_entry.start_address_lba,
                    bytes_per_sector,
                )?)
                .ok_or_else(|| {
                    ErrorTrace::new("MBR logical partition offset overflow".to_string())
                })?;

            partitions.push(Self::partition_from_entry(
                *next_partition_index,
                &logical_partition_entry,
                logical_partition_offset,
                bytes_per_sector,
            )?);
            *next_partition_index += 1;

            match next_ebr_offset {
                Some(next_ebr_offset) => current_ebr_offset = next_ebr_offset,
                None => return Ok(()),
            }
        }

        Err(ErrorTrace::new(
            "More than 1024 logical MBR partitions are not supported".to_string(),
        ))
    }

    fn validate_partitions(
        partitions: &[MbrPartition],
        source_size: u64,
    ) -> Result<(), ErrorTrace> {
        let mut sorted_partitions = partitions.to_vec();

        sorted_partitions.sort_by_key(|partition| partition.offset());

        let mut last_end_offset: u64 = 0;

        for partition in sorted_partitions.iter() {
            if partition.offset() < last_end_offset {
                return Err(ErrorTrace::new(
                    "Unsupported overlapping MBR partition entries".to_string(),
                ));
            }

            let end_offset = partition
                .offset()
                .checked_add(partition.size())
                .ok_or_else(|| ErrorTrace::new("MBR partition end offset overflow".to_string()))?;

            if end_offset > source_size {
                return Err(ErrorTrace::new(
                    "Invalid MBR partition entry size value out of bounds".to_string(),
                ));
            }

            last_end_offset = end_offset;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::source::{MemoryDataSource, open_local_data_source};
    use crate::tests::get_test_data_path;

    fn get_volume_system() -> Result<MbrVolumeSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("mbr/mbr.raw"));
        let source = open_local_data_source(&path)?;

        MbrVolumeSystem::open(&source)
    }

    fn build_extended_mbr_data() -> Vec<u8> {
        let mut data = vec![0u8; 64 * 512];

        fn write_entry(
            data: &mut [u8],
            sector_index: usize,
            entry_index: usize,
            flags: u8,
            partition_type: u8,
            start_lba: u32,
            sector_count: u32,
        ) {
            let entry_offset = (sector_index * 512) + 446 + (entry_index * 16);
            data[entry_offset] = flags;
            data[entry_offset + 4] = partition_type;
            data[entry_offset + 8..entry_offset + 12].copy_from_slice(&start_lba.to_le_bytes());
            data[entry_offset + 12..entry_offset + 16].copy_from_slice(&sector_count.to_le_bytes());
        }

        fn write_signature(data: &mut [u8], sector_index: usize) {
            let signature_offset = (sector_index * 512) + 510;
            data[signature_offset..signature_offset + 2].copy_from_slice(&MBR_BOOT_SIGNATURE);
        }

        // Primary MBR with one primary partition and one extended partition.
        write_entry(&mut data, 0, 0, 0x80, 0x83, 1, 10);
        write_entry(&mut data, 0, 1, 0x00, 0x0f, 20, 20);
        write_signature(&mut data, 0);

        // First EBR at sector 20: logical partition at sector 21, next EBR at sector 31.
        write_entry(&mut data, 20, 0, 0x00, 0x83, 1, 5);
        write_entry(&mut data, 20, 1, 0x00, 0x05, 11, 9);
        write_signature(&mut data, 20);

        // Second EBR at sector 31: logical partition at sector 32.
        write_entry(&mut data, 31, 0, 0x00, 0x07, 1, 3);
        write_signature(&mut data, 31);

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
        assert_eq!(partition.offset(), 512);
        assert_eq!(partition.size(), 1_049_088);
        assert_eq!(partition.partition_type(), 0x83);
        assert_eq!(partition.flags(), 0x00);
        Ok(())
    }

    #[test]
    fn test_partition_open_source() -> Result<(), ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("mbr/mbr.raw"));
        let source = open_local_data_source(&path)?;
        let volume_system = MbrVolumeSystem::open(&source)?;
        let partition_source = volume_system.partition(0)?.open_source(source);
        let mut data = vec![0; 2];

        partition_source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_extended_partition_chain() -> Result<(), ErrorTrace> {
        let source: DataSourceReference =
            Arc::new(MemoryDataSource::new(build_extended_mbr_data()));
        let volume_system = MbrVolumeSystem::open_with_bytes_per_sector(&source, 512)?;

        assert_eq!(volume_system.partitions().len(), 3);
        assert_eq!(volume_system.partition(0)?.offset(), 512);
        assert_eq!(volume_system.partition(1)?.offset(), 21 * 512);
        assert_eq!(volume_system.partition(2)?.offset(), 32 * 512);
        Ok(())
    }
}
