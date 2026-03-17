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

use keramics_core::ErrorTrace;

use super::partition::ApmPartition;
use crate::source::{DataSource, DataSourceReference};

const APM_PARTITION_MAP_SIGNATURE: [u8; 2] = [0x50, 0x4d];
const APM_PARTITION_MAP_TYPE: &str = "Apple_partition_map";

/// Immutable Apple Partition Map metadata.
pub struct ApmVolumeSystem {
    bytes_per_sector: u16,
    partitions: Vec<ApmPartition>,
}

#[derive(Clone, Debug)]
struct ApmPartitionMapEntry {
    number_of_entries: u32,
    start_sector: u32,
    number_of_sectors: u32,
    name: String,
    type_identifier: String,
    status_flags: u32,
}

impl ApmPartitionMapEntry {
    fn read_at(source: &dyn DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(offset, &mut data)?;

        if data[0..2] != APM_PARTITION_MAP_SIGNATURE {
            return Err(ErrorTrace::new(
                "Unsupported APM partition map signature".to_string(),
            ));
        }

        Ok(Self {
            number_of_entries: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
            start_sector: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
            number_of_sectors: u32::from_be_bytes([data[12], data[13], data[14], data[15]]),
            name: read_ascii_c_string(&data[16..48]),
            type_identifier: read_ascii_c_string(&data[48..80]),
            status_flags: u32::from_be_bytes([data[88], data[89], data[90], data[91]]),
        })
    }
}

impl ApmVolumeSystem {
    const BYTES_PER_SECTOR: u16 = 512;

    /// Opens and parses an Apple Partition Map.
    pub fn open(source: &DataSourceReference) -> Result<Self, ErrorTrace> {
        let source_size = source.size()?;
        let mut partitions = Vec::new();
        let mut number_of_entries: Option<u32> = None;
        let mut previous_end_offset: u64 = 0;
        let mut partition_map_entry_index: u32 = 0;
        let mut partition_map_entry_offset: u64 = Self::BYTES_PER_SECTOR as u64;

        loop {
            let partition_map_entry =
                ApmPartitionMapEntry::read_at(source.as_ref(), partition_map_entry_offset)?;

            if partition_map_entry.number_of_entries == 0 {
                return Err(ErrorTrace::new(
                    "Unsupported APM number of partition map entries: 0".to_string(),
                ));
            }

            match number_of_entries {
                Some(expected_number_of_entries) => {
                    if partition_map_entry.number_of_entries != expected_number_of_entries {
                        return Err(ErrorTrace::new(format!(
                            "Unsupported APM partition map entry {} number of entries value: {}",
                            partition_map_entry_index, partition_map_entry.number_of_entries,
                        )));
                    }
                }
                None => {
                    if partition_map_entry.type_identifier != APM_PARTITION_MAP_TYPE {
                        return Err(ErrorTrace::new(format!(
                            "Unsupported APM partition map type: {}",
                            partition_map_entry.type_identifier,
                        )));
                    }
                    number_of_entries = Some(partition_map_entry.number_of_entries);
                }
            }

            if partition_map_entry_index != 0 {
                let partition_offset = (partition_map_entry.start_sector as u64)
                    .checked_mul(Self::BYTES_PER_SECTOR as u64)
                    .ok_or_else(|| ErrorTrace::new("APM partition offset overflow".to_string()))?;
                let partition_size = (partition_map_entry.number_of_sectors as u64)
                    .checked_mul(Self::BYTES_PER_SECTOR as u64)
                    .ok_or_else(|| ErrorTrace::new("APM partition size overflow".to_string()))?;
                let partition_end_offset = partition_offset
                    .checked_add(partition_size)
                    .ok_or_else(|| {
                        ErrorTrace::new("APM partition end offset overflow".to_string())
                    })?;

                if partition_offset < previous_end_offset {
                    return Err(ErrorTrace::new(
                        "Unsupported overlapping APM partitions".to_string(),
                    ));
                }
                if partition_end_offset > source_size {
                    return Err(ErrorTrace::new(
                        "Invalid APM partition size value out of bounds".to_string(),
                    ));
                }

                partitions.push(ApmPartition::new(
                    (partition_map_entry_index - 1) as usize,
                    partition_offset,
                    partition_size,
                    partition_map_entry.type_identifier,
                    partition_map_entry.name,
                    partition_map_entry.status_flags,
                ));
                previous_end_offset = partition_end_offset;
            }

            partition_map_entry_index += 1;
            partition_map_entry_offset = partition_map_entry_offset
                .checked_add(Self::BYTES_PER_SECTOR as u64)
                .ok_or_else(|| ErrorTrace::new("APM partition map offset overflow".to_string()))?;

            if partition_map_entry_index >= number_of_entries.unwrap() {
                break;
            }
        }

        Ok(Self {
            bytes_per_sector: Self::BYTES_PER_SECTOR,
            partitions,
        })
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves all parsed partitions.
    pub fn partitions(&self) -> &[ApmPartition] {
        self.partitions.as_slice()
    }

    /// Retrieves a partition by index.
    pub fn partition(&self, partition_index: usize) -> Result<&ApmPartition, ErrorTrace> {
        self.partitions.get(partition_index).ok_or_else(|| {
            ErrorTrace::new(format!("No APM partition with index: {}", partition_index))
        })
    }
}

fn read_ascii_c_string(data: &[u8]) -> String {
    let string_end_offset = data
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.len());

    String::from_utf8_lossy(&data[..string_end_offset]).into_owned()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::source::{MemoryDataSource, open_local_data_source};
    use crate::tests::get_test_data_path;

    fn get_volume_system() -> Result<ApmVolumeSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("apm/apm.dmg"));
        let source = open_local_data_source(&path)?;

        ApmVolumeSystem::open(&source)
    }

    fn build_apm_data() -> Vec<u8> {
        let mut data = vec![0u8; 64 * 512];

        fn write_string(target: &mut [u8], value: &str) {
            let bytes = value.as_bytes();
            let copy_size = bytes.len().min(target.len());

            target[..copy_size].copy_from_slice(&bytes[..copy_size]);
        }

        struct EntrySpec<'a> {
            entry_index: usize,
            number_of_entries: u32,
            start_sector: u32,
            number_of_sectors: u32,
            name: &'a str,
            type_identifier: &'a str,
            status_flags: u32,
        }

        fn write_entry(data: &mut [u8], spec: &EntrySpec<'_>) {
            let offset = 512 + (spec.entry_index * 512);

            data[offset..offset + 2].copy_from_slice(&APM_PARTITION_MAP_SIGNATURE);
            data[offset + 4..offset + 8].copy_from_slice(&spec.number_of_entries.to_be_bytes());
            data[offset + 8..offset + 12].copy_from_slice(&spec.start_sector.to_be_bytes());
            data[offset + 12..offset + 16].copy_from_slice(&spec.number_of_sectors.to_be_bytes());
            write_string(&mut data[offset + 16..offset + 48], spec.name);
            write_string(&mut data[offset + 48..offset + 80], spec.type_identifier);
            data[offset + 88..offset + 92].copy_from_slice(&spec.status_flags.to_be_bytes());
        }

        write_entry(
            &mut data,
            &EntrySpec {
                entry_index: 0,
                number_of_entries: 3,
                start_sector: 1,
                number_of_sectors: 3,
                name: "partition_map",
                type_identifier: APM_PARTITION_MAP_TYPE,
                status_flags: 0,
            },
        );
        write_entry(
            &mut data,
            &EntrySpec {
                entry_index: 1,
                number_of_entries: 3,
                start_sector: 10,
                number_of_sectors: 4,
                name: "MyHFS",
                type_identifier: "Apple_HFS",
                status_flags: 0x0000_007f,
            },
        );
        write_entry(
            &mut data,
            &EntrySpec {
                entry_index: 2,
                number_of_entries: 3,
                start_sector: 20,
                number_of_sectors: 8,
                name: "Extra",
                type_identifier: "Apple_Free",
                status_flags: 0,
            },
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
        assert_eq!(partition.offset(), 32_768);
        assert_eq!(partition.size(), 4_153_344);
        assert!(!partition.type_identifier().is_empty());
        assert!(!partition.name().is_empty());
        Ok(())
    }

    #[test]
    fn test_partition_open_source() -> Result<(), ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("apm/apm.dmg"));
        let source = open_local_data_source(&path)?;
        let volume_system = ApmVolumeSystem::open(&source)?;
        let partition_source = volume_system.partition(0)?.open_source(source);
        let mut data = vec![0; 2];

        partition_source.read_exact_at(1024, &mut data)?;

        assert_eq!(data, [0x48, 0x2b]);
        Ok(())
    }

    #[test]
    fn test_open_synthetic_partition_map() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(build_apm_data()));
        let volume_system = ApmVolumeSystem::open(&source)?;
        let partition = volume_system.partition(0)?;

        assert_eq!(volume_system.partitions().len(), 2);
        assert_eq!(partition.name(), "MyHFS");
        assert_eq!(partition.type_identifier(), "Apple_HFS");
        assert_eq!(partition.status_flags(), 0x0000_007f);
        Ok(())
    }
}
