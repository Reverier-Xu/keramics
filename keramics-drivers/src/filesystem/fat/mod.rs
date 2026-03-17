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
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::{
    DataSource, DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget,
    MemoryDataSource,
};

const FAT_BOOT_SIGNATURE: [u8; 2] = [0x55, 0xaa];
const FAT_SUPPORTED_BYTES_PER_SECTOR: [u16; 4] = [512, 1024, 2048, 4096];
const FAT_SUPPORTED_SECTORS_PER_CLUSTER_BLOCK: [u8; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
const FAT_FILE_ATTRIBUTE_FLAG_VOLUME_LABEL: u8 = 0x08;
const FAT_FILE_ATTRIBUTE_FLAG_DIRECTORY: u8 = 0x10;

/// FAT format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FatFormat {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Clone)]
struct FatRuntime {
    source: DataSourceReference,
    bytes_per_sector: u16,
    cluster_block_size: u32,
    first_cluster_offset: u64,
    root_directory_offset: u64,
    root_directory_size: u32,
    root_directory_cluster_block_number: u32,
    format: FatFormat,
    block_allocation_table: FatBlockAllocationTable,
    volume_serial_number: u32,
    volume_label: Option<String>,
    root_directory_volume_label: Option<String>,
}

#[derive(Clone)]
struct FatBootRecord {
    bytes_per_sector: u16,
    sectors_per_cluster_block: u8,
    number_of_reserved_sectors: u16,
    number_of_allocation_tables: u8,
    number_of_root_directory_entries: u16,
    allocation_table_size: u32,
    number_of_sectors: u32,
    root_directory_cluster_block_number: u32,
    volume_serial_number: u32,
    volume_label: Option<String>,
}

#[derive(Clone)]
struct FatBlockAllocationTable {
    format: FatFormat,
    offset: u64,
    number_of_entries: u32,
    first_cluster_offset: u64,
    cluster_block_size: u32,
}

#[derive(Clone)]
struct FatLongNameDirectoryEntry {
    sequence_number: u8,
    name: Vec<u16>,
}

#[derive(Clone)]
struct FatDirectoryEntry {
    identifier: u32,
    short_name: String,
    file_attribute_flags: u8,
    data_start_cluster: u32,
    data_size: u32,
    long_name: Option<String>,
}

/// Immutable FAT file entry.
#[derive(Clone)]
pub struct FatFileEntry {
    runtime: Arc<FatRuntime>,
    identifier: u32,
    directory_entry: Option<FatDirectoryEntry>,
}

/// Immutable FAT file system.
pub struct FatFileSystem {
    runtime: Arc<FatRuntime>,
}

impl FatFileSystem {
    /// Opens and parses a FAT file system.
    pub fn open(source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let boot_record = FatBootRecord::read_at(source.as_ref())?;
        let number_of_clusters = determine_number_of_clusters(&boot_record);
        let format = if boot_record.root_directory_cluster_block_number != 0
            || number_of_clusters >= 65525
        {
            FatFormat::Fat32
        } else if number_of_clusters >= 4085 {
            FatFormat::Fat16
        } else {
            FatFormat::Fat12
        };
        let allocation_table_offset = (boot_record.number_of_reserved_sectors as u64)
            .checked_mul(boot_record.bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("FAT allocation table offset overflow".to_string()))?;
        let allocation_table_size = (boot_record.allocation_table_size as u64)
            .checked_mul(boot_record.bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("FAT allocation table size overflow".to_string()))?;
        let mut first_cluster_offset = allocation_table_offset
            + (boot_record.number_of_allocation_tables as u64) * allocation_table_size;
        let cluster_block_size = (boot_record.bytes_per_sector as u32)
            .checked_mul(boot_record.sectors_per_cluster_block as u32)
            .ok_or_else(|| ErrorTrace::new("FAT cluster block size overflow".to_string()))?;

        let (root_directory_offset, root_directory_size) = if boot_record
            .root_directory_cluster_block_number
            == 0
        {
            let root_directory_size = (boot_record.number_of_root_directory_entries as u32)
                .checked_mul(32)
                .ok_or_else(|| ErrorTrace::new("FAT root directory size overflow".to_string()))?;
            let root_directory_offset = first_cluster_offset;
            first_cluster_offset += root_directory_size as u64;
            (root_directory_offset, root_directory_size)
        } else {
            (0, 0)
        };

        let block_allocation_table = FatBlockAllocationTable {
            format,
            offset: allocation_table_offset,
            number_of_entries: number_of_clusters as u32,
            first_cluster_offset,
            cluster_block_size,
        };
        let root_directory_entries = read_root_directory(
            source.as_ref(),
            &block_allocation_table,
            root_directory_offset,
            root_directory_size,
            boot_record.root_directory_cluster_block_number,
        )?;

        let runtime = Arc::new(FatRuntime {
            source,
            bytes_per_sector: boot_record.bytes_per_sector,
            cluster_block_size,
            first_cluster_offset,
            root_directory_offset,
            root_directory_size,
            root_directory_cluster_block_number: boot_record.root_directory_cluster_block_number,
            format,
            block_allocation_table,
            volume_serial_number: boot_record.volume_serial_number,
            volume_label: boot_record.volume_label,
            root_directory_volume_label: root_directory_entries.volume_label,
        });

        Ok(Self { runtime })
    }

    /// Retrieves the format.
    pub fn format(&self) -> FatFormat {
        self.runtime.format
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.runtime.bytes_per_sector
    }

    /// Retrieves the volume serial number.
    pub fn volume_serial_number(&self) -> u32 {
        self.runtime.volume_serial_number
    }

    /// Retrieves the volume label.
    pub fn volume_label(&self) -> Option<&str> {
        self.runtime
            .root_directory_volume_label
            .as_deref()
            .or(self.runtime.volume_label.as_deref())
    }

    /// Retrieves the root directory.
    pub fn root_directory(&self) -> Result<FatFileEntry, ErrorTrace> {
        Ok(FatFileEntry {
            runtime: self.runtime.clone(),
            identifier: self.root_identifier(),
            directory_entry: None,
        })
    }

    /// Retrieves a file entry by absolute path.
    pub fn file_entry_by_path(&self, path: &str) -> Result<Option<FatFileEntry>, ErrorTrace> {
        if path.is_empty() || !path.starts_with('/') {
            return Ok(None);
        }
        if path == "/" {
            return Ok(Some(self.root_directory()?));
        }

        let path_components: Vec<String> = path
            .split('/')
            .filter(|component| !component.is_empty())
            .map(|component| component.to_string())
            .collect();
        let mut file_entry = self.root_directory()?;

        for path_component in path_components {
            file_entry = match file_entry.sub_file_entry_by_name(path_component.as_str())? {
                Some(file_entry) => file_entry,
                None => return Ok(None),
            };
        }

        Ok(Some(file_entry))
    }

    fn root_identifier(&self) -> u32 {
        if self.runtime.root_directory_size > 0 {
            self.runtime.root_directory_offset as u32
        } else {
            (self.runtime.first_cluster_offset
                + ((self.runtime.root_directory_cluster_block_number - 2) as u64)
                    * self.runtime.cluster_block_size as u64) as u32
        }
    }
}

impl FatFileEntry {
    /// Retrieves the identifier.
    pub fn identifier(&self) -> u32 {
        self.identifier
    }

    /// Retrieves the name.
    pub fn name(&self) -> Option<&str> {
        self.directory_entry.as_ref().map(FatDirectoryEntry::name)
    }

    /// Retrieves the size.
    pub fn size(&self) -> u64 {
        self.directory_entry
            .as_ref()
            .map_or(0, |entry| entry.data_size as u64)
    }

    /// Determines if the file entry is a directory.
    pub fn is_directory(&self) -> bool {
        match &self.directory_entry {
            Some(directory_entry) => {
                directory_entry.file_attribute_flags & 0x58 == FAT_FILE_ATTRIBUTE_FLAG_DIRECTORY
            }
            None => true,
        }
    }

    /// Determines if the file entry is the root directory.
    pub fn is_root_directory(&self) -> bool {
        self.directory_entry.is_none()
    }

    /// Retrieves the number of sub file entries.
    pub fn number_of_sub_file_entries(&self) -> Result<usize, ErrorTrace> {
        Ok(self.read_sub_directory_entries()?.entries.len())
    }

    /// Retrieves a specific sub file entry by name.
    pub fn sub_file_entry_by_name(&self, name: &str) -> Result<Option<FatFileEntry>, ErrorTrace> {
        let directory_entries = self.read_sub_directory_entries()?;
        let lookup_name = normalize_lookup_name(name);
        let directory_entry = match directory_entries.entries.get(&lookup_name) {
            Some(entry) => entry.clone(),
            None => return Ok(None),
        };

        Ok(Some(FatFileEntry {
            runtime: self.runtime.clone(),
            identifier: directory_entry.identifier,
            directory_entry: Some(directory_entry),
        }))
    }

    /// Opens the default data source of the file entry.
    pub fn open_source(&self) -> Result<Option<DataSourceReference>, ErrorTrace> {
        if self.is_directory() {
            return Ok(None);
        }

        let directory_entry = match self.directory_entry.as_ref() {
            Some(directory_entry) => directory_entry,
            None => return Ok(None),
        };

        if directory_entry.data_size == 0 {
            return Ok(Some(Arc::new(MemoryDataSource::new(Vec::new()))));
        }

        let cluster_chain = self.read_cluster_chain(directory_entry.data_start_cluster)?;
        let mut extents = Vec::new();
        let mut current_extent: Option<ExtentMapEntry> = None;
        let mut logical_offset: u64 = 0;
        let mut remaining_size = directory_entry.data_size as u64;

        for cluster_block_number in cluster_chain {
            let size = min(self.runtime.cluster_block_size as u64, remaining_size);
            let physical_offset = self.runtime.first_cluster_offset
                + ((cluster_block_number - 2) as u64) * self.runtime.cluster_block_size as u64;
            let next_extent = ExtentMapEntry {
                logical_offset,
                size,
                target: ExtentMapTarget::Data {
                    source: self.runtime.source.clone(),
                    source_offset: physical_offset,
                },
            };

            current_extent = merge_extent(current_extent, next_extent, &mut extents);
            logical_offset += size;
            remaining_size -= size;

            if remaining_size == 0 {
                break;
            }
        }

        if let Some(current_extent) = current_extent {
            extents.push(current_extent);
        }
        if remaining_size != 0 {
            return Err(ErrorTrace::new(
                "FAT cluster chain ended before the file data size was satisfied".to_string(),
            ));
        }

        Ok(Some(Arc::new(ExtentMapDataSource::new(extents)?)))
    }

    fn read_sub_directory_entries(&self) -> Result<FatDirectoryEntries, ErrorTrace> {
        if !self.is_directory() {
            return Ok(FatDirectoryEntries::default());
        }

        if self.is_root_directory() {
            return read_root_directory(
                self.runtime.source.as_ref(),
                &self.runtime.block_allocation_table,
                self.runtime.root_directory_offset,
                self.runtime.root_directory_size,
                self.runtime.root_directory_cluster_block_number,
            );
        }

        let cluster_block_number = self
            .directory_entry
            .as_ref()
            .ok_or_else(|| ErrorTrace::new("Missing FAT directory entry".to_string()))?
            .data_start_cluster;

        read_directory_entries_at_cluster(
            self.runtime.source.as_ref(),
            &self.runtime.block_allocation_table,
            cluster_block_number,
        )
    }

    fn read_cluster_chain(&self, mut cluster_block_number: u32) -> Result<Vec<u32>, ErrorTrace> {
        let mut read_cluster_block_numbers = HashSet::new();
        let mut cluster_chain = Vec::new();
        let largest_cluster_block_number = self
            .runtime
            .block_allocation_table
            .largest_cluster_block_number();

        while cluster_block_number >= 2 && cluster_block_number < largest_cluster_block_number {
            if !read_cluster_block_numbers.insert(cluster_block_number) {
                return Err(ErrorTrace::new(format!(
                    "Cluster block: {} already read",
                    cluster_block_number,
                )));
            }

            cluster_chain.push(cluster_block_number);
            cluster_block_number = self
                .runtime
                .block_allocation_table
                .read_entry(self.runtime.source.as_ref(), cluster_block_number)?;
        }

        Ok(cluster_chain)
    }
}

impl FatBootRecord {
    fn read_at(source: &dyn DataSource) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 512];

        source.read_exact_at(0, &mut data)?;

        if data[17..21] == [0; 4] && data[22..24] == [0; 2] {
            read_fat32_boot_record(&data)
        } else {
            read_fat12_boot_record(&data)
        }
    }
}

impl FatBlockAllocationTable {
    fn largest_cluster_block_number(&self) -> u32 {
        match self.format {
            FatFormat::Fat12 => 0x0000_0ff0,
            FatFormat::Fat16 => 0x0000_fff0,
            FatFormat::Fat32 => 0x0fff_fff0,
        }
    }

    fn read_entry(&self, source: &dyn DataSource, entry_index: u32) -> Result<u32, ErrorTrace> {
        if entry_index >= self.number_of_entries {
            return Err(ErrorTrace::new(format!(
                "Unsupported FAT entry index: {} value out of bounds",
                entry_index,
            )));
        }

        let (entry_offset, entry_size): (u64, usize) = match self.format {
            FatFormat::Fat12 => (((entry_index as u64) / 2) * 3, 3),
            FatFormat::Fat16 => ((entry_index as u64) * 2, 2),
            FatFormat::Fat32 => ((entry_index as u64) * 4, 4),
        };
        let mut data = vec![0; entry_size];

        source.read_exact_at(self.offset + entry_offset, &mut data)?;

        Ok(match self.format {
            FatFormat::Fat12 => {
                if entry_index.is_multiple_of(2) {
                    read_u16_le(&data, 0)? as u32 & 0x0fff
                } else {
                    (read_u16_le(&data, 1)? as u32) >> 4
                }
            }
            FatFormat::Fat16 => read_u16_le(&data, 0)? as u32,
            FatFormat::Fat32 => read_u32_le(&data, 0)?,
        })
    }
}

impl FatDirectoryEntry {
    fn name(&self) -> &str {
        match self.long_name.as_deref() {
            Some(long_name) => long_name,
            None => self.short_name.as_str(),
        }
    }
}

fn determine_number_of_clusters(boot_record: &FatBootRecord) -> u64 {
    let mut number_of_clusters = (boot_record.number_of_sectors as u64)
        .saturating_sub(boot_record.number_of_reserved_sectors as u64);
    number_of_clusters = number_of_clusters.saturating_sub(
        (boot_record.number_of_allocation_tables as u64)
            * (boot_record.allocation_table_size as u64),
    );
    number_of_clusters / boot_record.sectors_per_cluster_block as u64
}

fn read_fat12_boot_record(data: &[u8]) -> Result<FatBootRecord, ErrorTrace> {
    if data.len() < 512 {
        return Err(ErrorTrace::new(
            "Unsupported FAT boot record size".to_string(),
        ));
    }
    if data[510..512] != FAT_BOOT_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported FAT boot signature".to_string(),
        ));
    }

    let bytes_per_sector = read_u16_le(data, 11)?;
    if !FAT_SUPPORTED_BYTES_PER_SECTOR.contains(&bytes_per_sector) {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT bytes per sector: {}",
            bytes_per_sector,
        )));
    }

    let sectors_per_cluster_block = data[13];
    if !FAT_SUPPORTED_SECTORS_PER_CLUSTER_BLOCK.contains(&sectors_per_cluster_block) {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT sectors per cluster block: {}",
            sectors_per_cluster_block,
        )));
    }

    let number_of_sectors_16bit = read_u16_le(data, 19)?;
    let number_of_sectors_32bit = read_u32_le(data, 32)?;
    let volume_label = if data[38] == 0x29 {
        let label = trim_ascii_end_spaces(&data[43..54]);

        if label.is_empty() { None } else { Some(label) }
    } else {
        None
    };

    Ok(FatBootRecord {
        bytes_per_sector,
        sectors_per_cluster_block,
        number_of_reserved_sectors: read_u16_le(data, 14)?,
        number_of_allocation_tables: data[16],
        number_of_root_directory_entries: read_u16_le(data, 17)?,
        allocation_table_size: read_u16_le(data, 22)? as u32,
        number_of_sectors: if number_of_sectors_32bit != 0 {
            number_of_sectors_32bit
        } else {
            number_of_sectors_16bit as u32
        },
        root_directory_cluster_block_number: 0,
        volume_serial_number: if data[38] == 0x29 {
            read_u32_le(data, 39)?
        } else {
            0
        },
        volume_label,
    })
}

fn read_fat32_boot_record(data: &[u8]) -> Result<FatBootRecord, ErrorTrace> {
    if data.len() < 512 {
        return Err(ErrorTrace::new(
            "Unsupported FAT boot record size".to_string(),
        ));
    }
    if data[510..512] != FAT_BOOT_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported FAT boot signature".to_string(),
        ));
    }

    let bytes_per_sector = read_u16_le(data, 11)?;
    if !FAT_SUPPORTED_BYTES_PER_SECTOR.contains(&bytes_per_sector) {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT bytes per sector: {}",
            bytes_per_sector,
        )));
    }

    let sectors_per_cluster_block = data[13];
    if !FAT_SUPPORTED_SECTORS_PER_CLUSTER_BLOCK.contains(&sectors_per_cluster_block) {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT sectors per cluster block: {}",
            sectors_per_cluster_block,
        )));
    }

    let number_of_root_directory_entries = read_u16_le(data, 17)?;
    if number_of_root_directory_entries != 0 {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT-32 number of root directory entries: {}",
            number_of_root_directory_entries,
        )));
    }
    if read_u16_le(data, 19)? != 0 {
        return Err(ErrorTrace::new(
            "Unsupported FAT-32 number of sectors 16-bit".to_string(),
        ));
    }
    if read_u16_le(data, 22)? != 0 {
        return Err(ErrorTrace::new(
            "Unsupported FAT-32 allocation table size 16-bit".to_string(),
        ));
    }

    let root_directory_cluster_block_number = read_u32_le(data, 44)?;
    if root_directory_cluster_block_number < 2 {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT-32 root directory cluster block number: {}",
            root_directory_cluster_block_number,
        )));
    }

    let volume_label = if data[66] == 0x29 {
        let label = trim_ascii_end_spaces(&data[71..82]);

        if label.is_empty() { None } else { Some(label) }
    } else {
        None
    };

    Ok(FatBootRecord {
        bytes_per_sector,
        sectors_per_cluster_block,
        number_of_reserved_sectors: read_u16_le(data, 14)?,
        number_of_allocation_tables: data[16],
        number_of_root_directory_entries: 0,
        allocation_table_size: read_u32_le(data, 36)?,
        number_of_sectors: read_u32_le(data, 32)?,
        root_directory_cluster_block_number,
        volume_serial_number: if data[66] == 0x29 {
            read_u32_le(data, 67)?
        } else {
            0
        },
        volume_label,
    })
}

#[derive(Default)]
struct FatDirectoryEntries {
    entries: BTreeMap<String, FatDirectoryEntry>,
    volume_label: Option<String>,
}

fn read_root_directory(
    source: &dyn DataSource,
    block_allocation_table: &FatBlockAllocationTable,
    root_directory_offset: u64,
    root_directory_size: u32,
    root_directory_cluster_block_number: u32,
) -> Result<FatDirectoryEntries, ErrorTrace> {
    if root_directory_size > 0 {
        read_directory_entries_at_offset(
            source,
            root_directory_offset,
            root_directory_size as usize,
            block_allocation_table.format,
        )
    } else {
        read_directory_entries_at_cluster(
            source,
            block_allocation_table,
            root_directory_cluster_block_number,
        )
    }
}

fn read_directory_entries_at_offset(
    source: &dyn DataSource,
    offset: u64,
    size: usize,
    format: FatFormat,
) -> Result<FatDirectoryEntries, ErrorTrace> {
    if !(32..=2_097_152).contains(&size) {
        return Err(ErrorTrace::new(format!(
            "Unsupported FAT directory entries size: {} value out of bounds",
            size,
        )));
    }

    let mut data = vec![0; size];
    source.read_exact_at(offset, &mut data)?;

    let mut entries = FatDirectoryEntries::default();
    read_directory_entries_from_data(&mut entries, &data, offset, format)?;

    Ok(entries)
}

fn read_directory_entries_at_cluster(
    source: &dyn DataSource,
    block_allocation_table: &FatBlockAllocationTable,
    mut cluster_block_number: u32,
) -> Result<FatDirectoryEntries, ErrorTrace> {
    let largest_cluster_block_number = block_allocation_table.largest_cluster_block_number();
    let mut data = vec![0; block_allocation_table.cluster_block_size as usize];
    let mut read_cluster_block_numbers = HashSet::new();
    let mut entries = FatDirectoryEntries::default();

    while cluster_block_number >= 2 && cluster_block_number < largest_cluster_block_number {
        if !read_cluster_block_numbers.insert(cluster_block_number) {
            return Err(ErrorTrace::new(format!(
                "Cluster block: {} already read",
                cluster_block_number,
            )));
        }

        let offset = block_allocation_table.first_cluster_offset
            + ((cluster_block_number - 2) as u64)
                * block_allocation_table.cluster_block_size as u64;

        source.read_exact_at(offset, &mut data)?;
        read_directory_entries_from_data(
            &mut entries,
            &data,
            offset,
            block_allocation_table.format,
        )?;
        cluster_block_number = block_allocation_table.read_entry(source, cluster_block_number)?;
    }

    Ok(entries)
}

fn read_directory_entries_from_data(
    entries: &mut FatDirectoryEntries,
    data: &[u8],
    mut directory_entry_offset: u64,
    format: FatFormat,
) -> Result<(), ErrorTrace> {
    let mut data_offset: usize = 0;
    let mut long_name_entries: Vec<FatLongNameDirectoryEntry> = Vec::new();
    let mut last_vfat_sequence_number: u8 = 0;

    while data_offset + 32 <= data.len() {
        let entry_data = &data[data_offset..data_offset + 32];

        match classify_directory_entry(entry_data) {
            FatDirectoryEntryType::LongName => {
                let entry = read_long_name_directory_entry(entry_data)?;
                let vfat_sequence_number = entry.sequence_number & 0x1f;

                if entry.sequence_number & 0x40 != 0 {
                    long_name_entries.clear();
                } else if last_vfat_sequence_number != 0
                    && vfat_sequence_number + 1 != last_vfat_sequence_number
                {
                    return Err(ErrorTrace::new(format!(
                        "VFAT long name sequence number mismatch at offset: {} (0x{:08x})",
                        directory_entry_offset, directory_entry_offset,
                    )));
                }

                long_name_entries.push(entry);
                last_vfat_sequence_number = vfat_sequence_number;
            }
            FatDirectoryEntryType::ShortName => {
                let short_name = read_short_name_directory_entry(entry_data, format)?;

                if short_name.file_attribute_flags & 0x58 == FAT_FILE_ATTRIBUTE_FLAG_VOLUME_LABEL {
                    entries.volume_label = Some(short_name.name().to_string());
                } else if short_name.name() != "." && short_name.name() != ".." {
                    let long_name = if long_name_entries.is_empty() {
                        None
                    } else {
                        Some(build_long_name(&mut long_name_entries)?)
                    };
                    let directory_entry = FatDirectoryEntry {
                        identifier: directory_entry_offset as u32,
                        short_name: short_name.name().to_string(),
                        file_attribute_flags: short_name.file_attribute_flags,
                        data_start_cluster: short_name.data_start_cluster,
                        data_size: short_name.data_size,
                        long_name,
                    };
                    let lookup_name = normalize_lookup_name(directory_entry.name());

                    entries.entries.insert(lookup_name, directory_entry);
                }
                last_vfat_sequence_number = 0;
            }
            FatDirectoryEntryType::Terminator => break,
            FatDirectoryEntryType::Unallocated => {
                long_name_entries.clear();
                last_vfat_sequence_number = 0;
            }
        }

        data_offset += 32;
        directory_entry_offset += 32;
    }

    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum FatDirectoryEntryType {
    LongName,
    ShortName,
    Terminator,
    Unallocated,
}

fn classify_directory_entry(data: &[u8]) -> FatDirectoryEntryType {
    if data[0] == 0xe5 {
        FatDirectoryEntryType::Unallocated
    } else if data[11..13] == [0x0f, 0x00]
        && data[26..28] == [0x00, 0x00]
        && ((data[0] >= 0x01 && data[0] <= 0x13) || (data[0] >= 0x41 && data[0] <= 0x54))
    {
        FatDirectoryEntryType::LongName
    } else if data[0..32] == [0; 32] {
        FatDirectoryEntryType::Terminator
    } else {
        FatDirectoryEntryType::ShortName
    }
}

fn read_long_name_directory_entry(data: &[u8]) -> Result<FatLongNameDirectoryEntry, ErrorTrace> {
    if data.len() < 32 {
        return Err(ErrorTrace::new(
            "Unsupported FAT long name entry size".to_string(),
        ));
    }

    let mut name = Vec::new();
    read_ucs2_segments(&mut name, &data[1..11]);
    if name.len() == 5 || data[14..16] != [0xff, 0xff] {
        read_ucs2_segments(&mut name, &data[14..26]);
    }
    if name.len() == 11 || data[28..30] != [0xff, 0xff] {
        read_ucs2_segments(&mut name, &data[28..32]);
    }

    Ok(FatLongNameDirectoryEntry {
        sequence_number: data[0],
        name,
    })
}

fn read_ucs2_segments(result: &mut Vec<u16>, data: &[u8]) {
    for chunk in data.chunks_exact(2) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);

        if value == 0 || value == 0xffff {
            break;
        }
        result.push(value);
    }
}

struct FatShortNameDirectoryEntry {
    name: String,
    file_attribute_flags: u8,
    data_start_cluster: u32,
    data_size: u32,
}

impl FatShortNameDirectoryEntry {
    fn name(&self) -> &str {
        self.name.as_str()
    }
}

fn read_short_name_directory_entry(
    data: &[u8],
    format: FatFormat,
) -> Result<FatShortNameDirectoryEntry, ErrorTrace> {
    if data.len() < 32 {
        return Err(ErrorTrace::new(
            "Unsupported FAT short name entry size".to_string(),
        ));
    }

    let file_attribute_flags = data[11];
    let flags = data[12];
    let mut name = trim_ascii_end_spaces(&data[0..8]);

    if flags & 0x08 != 0 {
        name = name.to_ascii_lowercase();
    }

    let extension = trim_ascii_end_spaces(&data[8..11]);
    if !extension.is_empty() {
        if file_attribute_flags & 0x58 != FAT_FILE_ATTRIBUTE_FLAG_VOLUME_LABEL {
            name.push('.');
        }

        if flags & 0x10 != 0 {
            name.push_str(extension.to_ascii_lowercase().as_str());
        } else {
            name.push_str(extension.as_str());
        }
    }

    let data_start_cluster = match format {
        FatFormat::Fat32 => {
            let lower_16bit = read_u16_le(data, 26)? as u32;
            let upper_16bit = read_u16_le(data, 20)? as u32;
            (upper_16bit << 16) | lower_16bit
        }
        _ => read_u16_le(data, 26)? as u32,
    };

    Ok(FatShortNameDirectoryEntry {
        name,
        file_attribute_flags,
        data_start_cluster,
        data_size: read_u32_le(data, 28)?,
    })
}

fn build_long_name(
    long_name_entries: &mut Vec<FatLongNameDirectoryEntry>,
) -> Result<String, ErrorTrace> {
    let mut elements = Vec::new();

    for entry in long_name_entries.iter().rev() {
        elements.extend_from_slice(entry.name.as_slice());
    }
    long_name_entries.clear();

    String::from_utf16(elements.as_slice()).map_err(|error| {
        ErrorTrace::new(format!(
            "Unable to convert FAT long name into string with error: {}",
            error,
        ))
    })
}

fn trim_ascii_end_spaces(data: &[u8]) -> String {
    let end_offset = data
        .iter()
        .rposition(|value| *value != b' ')
        .map(|index| index + 1)
        .unwrap_or(0);

    String::from_utf8_lossy(&data[..end_offset]).into_owned()
}

fn normalize_lookup_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, ErrorTrace> {
    Ok(u16::from_le_bytes(
        data[offset..offset + 2]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read FAT u16 value".to_string()))?,
    ))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, ErrorTrace> {
    Ok(u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| ErrorTrace::new("Unable to read FAT u32 value".to_string()))?,
    ))
}

fn merge_extent(
    current_extent: Option<ExtentMapEntry>,
    next_extent: ExtentMapEntry,
    extents: &mut Vec<ExtentMapEntry>,
) -> Option<ExtentMapEntry> {
    match current_extent {
        Some(mut current_extent) => {
            let can_merge = match (&current_extent.target, &next_extent.target) {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost, open_local_data_source};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn open_file_system() -> Result<FatFileSystem, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("fat/fat12.raw"));
        let source = open_local_data_source(&path)?;

        FatFileSystem::open(source)
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;

        assert_eq!(file_system.bytes_per_sector(), 512);
        assert_eq!(file_system.format(), FatFormat::Fat12);
        assert_eq!(file_system.volume_serial_number(), 0x56f30d5b);
        assert_eq!(file_system.volume_label(), Some("FAT12_TEST"));
        Ok(())
    }

    #[test]
    fn test_root_directory() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let root_directory = file_system.root_directory()?;

        assert_eq!(root_directory.identifier(), 0x0000_1a00);
        assert!(root_directory.is_directory());
        assert!(root_directory.is_root_directory());
        Ok(())
    }

    #[test]
    fn test_file_entry_by_path() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;

        let empty_file = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        assert_eq!(empty_file.identifier(), 0x0000_1a40);
        assert_eq!(empty_file.name(), Some("emptyfile"));
        assert_eq!(empty_file.size(), 0);

        let regular_file = file_system
            .file_entry_by_path("/testdir1/testfile1")?
            .unwrap();
        assert_eq!(regular_file.identifier(), 0x0000_6260);
        assert_eq!(regular_file.name(), Some("testfile1"));
        assert_eq!(regular_file.size(), 9);

        let long_name_file = file_system
            .file_entry_by_path("/testdir1/My long, very long file name, so very long")?
            .unwrap();
        assert_eq!(
            long_name_file.name(),
            Some("My long, very long file name, so very long")
        );
        Ok(())
    }

    #[test]
    fn test_directory_queries() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let directory = file_system.file_entry_by_path("/testdir1")?.unwrap();
        assert!(directory.is_directory());
        assert!(!directory.is_root_directory());
        assert_eq!(directory.number_of_sub_file_entries()?, 3);
        assert!(directory.sub_file_entry_by_name("testfile1")?.is_some());
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/testfile1")?
            .unwrap();
        let source = file_entry.open_source()?.unwrap();
        let capabilities = source.capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_read_empty_file() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system.file_entry_by_path("/emptyfile")?.unwrap();
        let (offset, md5_hash) = read_data_source_md5(file_entry.open_source()?.unwrap())?;

        assert_eq!(offset, 0);
        assert_eq!(md5_hash.as_str(), "d41d8cd98f00b204e9800998ecf8427e");
        Ok(())
    }

    #[test]
    fn test_read_regular_file() -> Result<(), ErrorTrace> {
        let file_system = open_file_system()?;
        let file_entry = file_system
            .file_entry_by_path("/testdir1/testfile1")?
            .unwrap();
        let source = file_entry.open_source()?.unwrap();

        assert_eq!(source.read_all()?, b"Keramics\n");
        Ok(())
    }
}
