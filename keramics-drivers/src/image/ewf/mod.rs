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

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use keramics_checksums::Adler32Context;
use keramics_compression::ZlibContext;
use keramics_core::ErrorTrace;
use keramics_types::Uuid;

use crate::resolver::SourceResolverReference;
use crate::source::{DataSource, DataSourceCapabilities, DataSourceReference, DataSourceSeekCost};

const EWF_FILE_HEADER_SIGNATURE: &[u8; 8] = b"EVF\x09\x0d\x0a\xff\x00";
const EWF_SECTION_TYPE_DATA: &[u8; 16] = b"data\0\0\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_DIGEST: &[u8; 16] = b"digest\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_DISK: &[u8; 16] = b"disk\0\0\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_DONE: &[u8; 16] = b"done\0\0\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_HASH: &[u8; 16] = b"hash\0\0\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_NEXT: &[u8; 16] = b"next\0\0\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_SECTORS: &[u8; 16] = b"sectors\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_TABLE: &[u8; 16] = b"table\0\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_TABLE2: &[u8; 16] = b"table2\0\0\0\0\0\0\0\0\0\0";
const EWF_SECTION_TYPE_VOLUME: &[u8; 16] = b"volume\0\0\0\0\0\0\0\0\0\0";

/// EWF media type.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EwfMediaType {
    FixedDisk,
    LogicalEvidence,
    Memory,
    OpticalDisk,
    RemoveableDisk,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EwfNamingSchema {
    E01Lower,
    E01Upper,
    S01Lower,
    S01Upper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EwfSegmentContinuation {
    HasNext,
    Last,
    Unknown,
}

#[derive(Clone)]
struct EwfSectionHeader {
    file_offset: u64,
    section_type: [u8; 16],
    next_offset: u64,
    size: u64,
}

#[derive(Clone)]
struct EwfSegment {
    file_name: String,
    segment_number: u16,
    source: DataSourceReference,
    sections: Vec<EwfSectionHeader>,
    continuation: EwfSegmentContinuation,
}

#[derive(Clone)]
struct EwfChunkDescriptor {
    segment_index: usize,
    logical_offset: u64,
    logical_size: u64,
    data_offset: u64,
    data_size: u32,
    compressed: bool,
}

struct EwfRuntime {
    segments: Vec<EwfSegment>,
    chunks: Vec<EwfChunkDescriptor>,
    chunk_size: u32,
    media_size: u64,
    chunk_cache: RwLock<HashMap<u32, Arc<[u8]>>>,
}

struct EwfBuilder {
    media_type: EwfMediaType,
    set_identifier: Uuid,
    number_of_chunks: u32,
    sectors_per_chunk: u32,
    bytes_per_sector: u32,
    number_of_sectors: u64,
    chunk_size: u32,
    media_size: u64,
    md5_hash: Option<[u8; 16]>,
    sha1_hash: Option<[u8; 20]>,
    chunks: Vec<EwfChunkDescriptor>,
    next_chunk_logical_offset: u64,
    saw_volume: bool,
}

struct EwfVolumeInfo {
    media_type: EwfMediaType,
    set_identifier: Uuid,
    number_of_chunks: u32,
    sectors_per_chunk: u32,
    bytes_per_sector: u32,
    number_of_sectors: u64,
    error_granularity: u32,
}

/// Immutable EWF image metadata plus shared runtime state.
pub struct EwfImage {
    runtime: Arc<EwfRuntime>,
    media_type: EwfMediaType,
    set_identifier: Uuid,
    number_of_chunks: u32,
    sectors_per_chunk: u32,
    bytes_per_sector: u32,
    number_of_sectors: u64,
    media_size: u64,
    md5_hash: Option<[u8; 16]>,
    sha1_hash: Option<[u8; 20]>,
}

struct EwfDataSource {
    runtime: Arc<EwfRuntime>,
}

impl EwfImage {
    /// Opens an EWF image from a resolver and first segment file name.
    pub fn open(resolver: &SourceResolverReference, file_name: &Path) -> Result<Self, ErrorTrace> {
        let file_name_string = file_name
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ErrorTrace::new("EWF image requires a valid file name".to_string()))?;
        let name = file_name
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ErrorTrace::new("EWF image requires a valid file stem".to_string()))?
            .to_string();
        let naming_schema = Self::get_segment_file_naming_schema(file_name_string)?;
        let segments = Self::open_segments(resolver, &name, &naming_schema)?;
        let mut builder = EwfBuilder::new();

        builder.read_segments(&segments)?;

        let runtime = Arc::new(EwfRuntime {
            segments,
            chunks: builder.chunks,
            chunk_size: builder.chunk_size,
            media_size: builder.media_size,
            chunk_cache: RwLock::new(HashMap::new()),
        });

        Ok(Self {
            runtime,
            media_type: builder.media_type,
            set_identifier: builder.set_identifier,
            number_of_chunks: builder.number_of_chunks,
            sectors_per_chunk: builder.sectors_per_chunk,
            bytes_per_sector: builder.bytes_per_sector,
            number_of_sectors: builder.number_of_sectors,
            media_size: builder.media_size,
            md5_hash: builder.md5_hash,
            sha1_hash: builder.sha1_hash,
        })
    }

    /// Retrieves the media type.
    pub fn media_type(&self) -> EwfMediaType {
        self.media_type
    }

    /// Retrieves the set identifier.
    pub fn set_identifier(&self) -> &Uuid {
        &self.set_identifier
    }

    /// Retrieves the number of segments.
    pub fn number_of_segments(&self) -> usize {
        self.runtime.segments.len()
    }

    /// Retrieves the segment file names.
    pub fn segment_file_names(&self) -> Vec<&str> {
        self.runtime
            .segments
            .iter()
            .map(|segment| segment.file_name.as_str())
            .collect()
    }

    /// Retrieves the number of chunks.
    pub fn number_of_chunks(&self) -> u32 {
        self.number_of_chunks
    }

    /// Retrieves the sectors per chunk.
    pub fn sectors_per_chunk(&self) -> u32 {
        self.sectors_per_chunk
    }

    /// Retrieves the bytes per sector.
    pub fn bytes_per_sector(&self) -> u32 {
        self.bytes_per_sector
    }

    /// Retrieves the number of sectors.
    pub fn number_of_sectors(&self) -> u64 {
        self.number_of_sectors
    }

    /// Retrieves the chunk size in bytes.
    pub fn chunk_size(&self) -> u32 {
        self.runtime.chunk_size
    }

    /// Retrieves the media size in bytes.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Retrieves the MD5 hash if present.
    pub fn md5_hash(&self) -> Option<&[u8; 16]> {
        self.md5_hash.as_ref()
    }

    /// Retrieves the SHA1 hash if present.
    pub fn sha1_hash(&self) -> Option<&[u8; 20]> {
        self.sha1_hash.as_ref()
    }

    /// Opens the logical media source.
    pub fn open_source(&self) -> DataSourceReference {
        Arc::new(EwfDataSource {
            runtime: self.runtime.clone(),
        })
    }

    fn open_segments(
        resolver: &SourceResolverReference,
        name: &str,
        naming_schema: &EwfNamingSchema,
    ) -> Result<Vec<EwfSegment>, ErrorTrace> {
        let mut segments = Vec::new();
        let mut segment_number: u16 = 1;

        loop {
            let segment_file_name =
                Self::get_segment_file_name(name, segment_number, naming_schema)?;
            let source = resolver
                .open_source(Path::new(&segment_file_name))?
                .ok_or_else(|| {
                    ErrorTrace::new(format!("Missing EWF segment file: {}", segment_file_name))
                })?;
            let segment = EwfSegment::open(segment_file_name, source)?;

            if segment.segment_number != segment_number {
                return Err(ErrorTrace::new(format!(
                    "Unsupported EWF segment number: {} expected: {}",
                    segment.segment_number, segment_number,
                )));
            }

            let continuation = segment.continuation;
            segments.push(segment);

            match continuation {
                EwfSegmentContinuation::HasNext => {
                    segment_number = segment_number.checked_add(1).ok_or_else(|| {
                        ErrorTrace::new("EWF segment number overflow".to_string())
                    })?;
                }
                EwfSegmentContinuation::Last | EwfSegmentContinuation::Unknown => break,
            }
        }

        Ok(segments)
    }

    fn get_segment_file_extension(
        segment_number: u16,
        naming_schema: &EwfNamingSchema,
    ) -> Result<String, ErrorTrace> {
        if segment_number == 0 {
            return Err(ErrorTrace::new(
                "Unsupported EWF segment number: 0".to_string(),
            ));
        }

        let first_character = match naming_schema {
            EwfNamingSchema::E01Upper => b'E' as u32,
            EwfNamingSchema::S01Upper => b'S' as u32,
            EwfNamingSchema::E01Lower => b'e' as u32,
            EwfNamingSchema::S01Lower => b's' as u32,
        };
        let last_character = match naming_schema {
            EwfNamingSchema::E01Upper | EwfNamingSchema::S01Upper => b'Z' as u32,
            EwfNamingSchema::E01Lower | EwfNamingSchema::S01Lower => b'z' as u32,
        };
        let mut extension = [0u32; 3];

        if segment_number < 100 {
            extension[2] = b'0' as u32 + (segment_number % 10) as u32;
            extension[1] = b'0' as u32 + (segment_number / 10) as u32;
            extension[0] = first_character;
        } else {
            let base_character = match naming_schema {
                EwfNamingSchema::E01Upper | EwfNamingSchema::S01Upper => b'A' as u32,
                EwfNamingSchema::E01Lower | EwfNamingSchema::S01Lower => b'a' as u32,
            };
            let mut extension_segment_number = (segment_number as u32) - 100;

            extension[2] = base_character + (extension_segment_number % 26);
            extension_segment_number /= 26;

            extension[1] = base_character + (extension_segment_number % 26);
            extension_segment_number /= 26;

            extension[0] = first_character + extension_segment_number;
        }

        if extension[0] > last_character {
            return Err(ErrorTrace::new(format!(
                "Unsupported EWF segment number: {} value exceeds maximum for naming schema",
                segment_number,
            )));
        }

        let mut string = String::new();
        for code_point in extension {
            let character = char::from_u32(code_point).ok_or_else(|| {
                ErrorTrace::new("Unable to encode EWF segment file extension".to_string())
            })?;
            string.push(character);
        }

        Ok(string)
    }

    fn get_segment_file_name(
        name: &str,
        segment_number: u16,
        naming_schema: &EwfNamingSchema,
    ) -> Result<String, ErrorTrace> {
        let extension = Self::get_segment_file_extension(segment_number, naming_schema)?;

        Ok(format!("{}.{}", name, extension))
    }

    fn get_segment_file_naming_schema(file_name: &str) -> Result<EwfNamingSchema, ErrorTrace> {
        let extension = Path::new(file_name)
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ErrorTrace::new(format!(
                    "Missing extension in EWF segment file: {}",
                    file_name,
                ))
            })?;

        match extension {
            "E01" => Ok(EwfNamingSchema::E01Upper),
            "S01" => Ok(EwfNamingSchema::S01Upper),
            "e01" => Ok(EwfNamingSchema::E01Lower),
            "s01" => Ok(EwfNamingSchema::S01Lower),
            _ => Err(ErrorTrace::new(format!(
                "Unsupported extension in EWF segment file: {}",
                file_name,
            ))),
        }
    }
}

impl EwfSegment {
    fn open(file_name: String, source: DataSourceReference) -> Result<Self, ErrorTrace> {
        let segment_number = read_file_header(source.as_ref())?;
        let sections = read_section_headers(source.as_ref())?;
        let continuation = sections
            .last()
            .map(|section| {
                if section.is_type(EWF_SECTION_TYPE_DONE) {
                    EwfSegmentContinuation::Last
                } else if section.is_type(EWF_SECTION_TYPE_NEXT) {
                    EwfSegmentContinuation::HasNext
                } else {
                    EwfSegmentContinuation::Unknown
                }
            })
            .unwrap_or(EwfSegmentContinuation::Unknown);

        Ok(Self {
            file_name,
            segment_number,
            source,
            sections,
            continuation,
        })
    }
}

impl EwfSectionHeader {
    fn read_at(source: &dyn DataSource, file_offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 76];

        source.read_exact_at(file_offset, &mut data)?;

        let stored_checksum = u32::from_le_bytes([data[72], data[73], data[74], data[75]]);
        let calculated_checksum = adler32(&data[0..72]);

        if stored_checksum != calculated_checksum {
            return Err(ErrorTrace::new(format!(
                "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF section checksums",
                stored_checksum, calculated_checksum,
            )));
        }

        let mut section_type = [0u8; 16];
        section_type.copy_from_slice(&data[0..16]);

        Ok(Self {
            file_offset,
            section_type,
            next_offset: u64::from_le_bytes([
                data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
            ]),
            size: u64::from_le_bytes([
                data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
            ]),
        })
    }

    fn data_offset(&self) -> u64 {
        self.file_offset + 76
    }

    fn data_size(&self) -> u64 {
        self.size - 76
    }

    fn is_type(&self, expected: &[u8; 16]) -> bool {
        self.section_type == *expected
    }
}

impl EwfBuilder {
    fn new() -> Self {
        Self {
            media_type: EwfMediaType::Unknown,
            set_identifier: Uuid::new(),
            number_of_chunks: 0,
            sectors_per_chunk: 0,
            bytes_per_sector: 0,
            number_of_sectors: 0,
            chunk_size: 0,
            media_size: 0,
            md5_hash: None,
            sha1_hash: None,
            chunks: Vec::new(),
            next_chunk_logical_offset: 0,
            saw_volume: false,
        }
    }

    fn read_segments(&mut self, segments: &[EwfSegment]) -> Result<(), ErrorTrace> {
        for (segment_index, segment) in segments.iter().enumerate() {
            let mut last_sectors_section: Option<&EwfSectionHeader> = None;

            for section in segment.sections.iter() {
                if section.is_type(EWF_SECTION_TYPE_SECTORS) {
                    last_sectors_section = Some(section);
                    continue;
                }
                if section.is_type(EWF_SECTION_TYPE_DISK)
                    || section.is_type(EWF_SECTION_TYPE_VOLUME)
                {
                    self.read_volume_section(segment, section)?;
                    continue;
                }
                if section.is_type(EWF_SECTION_TYPE_TABLE) {
                    self.read_table_section(segment_index, segment, section, last_sectors_section)?;
                    continue;
                }
                if section.is_type(EWF_SECTION_TYPE_HASH) {
                    self.md5_hash = Some(read_hash_section(segment.source.as_ref(), section)?);
                    continue;
                }
                if section.is_type(EWF_SECTION_TYPE_DIGEST) {
                    let (md5_hash, sha1_hash) =
                        read_digest_section(segment.source.as_ref(), section)?;

                    self.md5_hash = Some(md5_hash);
                    self.sha1_hash = Some(sha1_hash);
                    continue;
                }
                if section.is_type(EWF_SECTION_TYPE_DATA)
                    || section.is_type(EWF_SECTION_TYPE_TABLE2)
                    || section.is_type(EWF_SECTION_TYPE_DONE)
                    || section.is_type(EWF_SECTION_TYPE_NEXT)
                {
                    continue;
                }
            }
        }

        if !self.saw_volume {
            return Err(ErrorTrace::new(
                "Missing EWF disk or volume section".to_string(),
            ));
        }
        if self.chunks.len() as u32 != self.number_of_chunks {
            return Err(ErrorTrace::new(format!(
                "Mismatch between parsed: {} and declared: {} EWF chunks",
                self.chunks.len(),
                self.number_of_chunks,
            )));
        }
        if self.next_chunk_logical_offset != self.media_size {
            return Err(ErrorTrace::new(format!(
                "Mismatch between parsed: {} and declared: {} EWF media size",
                self.next_chunk_logical_offset, self.media_size,
            )));
        }

        Ok(())
    }

    fn read_volume_section(
        &mut self,
        segment: &EwfSegment,
        section: &EwfSectionHeader,
    ) -> Result<(), ErrorTrace> {
        if segment.segment_number != 1 {
            return Err(ErrorTrace::new(format!(
                "Unsupported EWF disk or volume section found in segment file: {}",
                segment.file_name,
            )));
        }
        if self.saw_volume {
            return Err(ErrorTrace::new(format!(
                "Multiple EWF disk or volume sections found in segment file: {}",
                segment.file_name,
            )));
        }

        let volume = read_volume_section(segment.source.as_ref(), section)?;

        if volume.number_of_chunks == 0
            || volume.sectors_per_chunk == 0
            || volume.bytes_per_sector == 0
        {
            return Err(ErrorTrace::new(
                "Invalid EWF volume geometry values".to_string(),
            ));
        }

        let chunk_size = volume
            .sectors_per_chunk
            .checked_mul(volume.bytes_per_sector)
            .ok_or_else(|| ErrorTrace::new("EWF chunk size overflow".to_string()))?;
        let media_size = volume
            .number_of_sectors
            .checked_mul(volume.bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("EWF media size overflow".to_string()))?;

        self.media_type = volume.media_type;
        self.set_identifier = volume.set_identifier;
        self.number_of_chunks = volume.number_of_chunks;
        self.sectors_per_chunk = volume.sectors_per_chunk;
        self.bytes_per_sector = volume.bytes_per_sector;
        self.number_of_sectors = volume.number_of_sectors;
        self.chunk_size = chunk_size;
        self.media_size = media_size;
        self.saw_volume = true;

        let _ = volume.error_granularity;

        Ok(())
    }

    fn read_table_section(
        &mut self,
        segment_index: usize,
        segment: &EwfSegment,
        section: &EwfSectionHeader,
        last_sectors_section: Option<&EwfSectionHeader>,
    ) -> Result<(), ErrorTrace> {
        if !self.saw_volume {
            return Err(ErrorTrace::new(
                "Missing EWF disk or volume section before table section".to_string(),
            ));
        }

        let table = read_table_section(segment.source.as_ref(), section)?;
        if table.entries.is_empty() {
            return Err(ErrorTrace::new("Missing EWF table entries".to_string()));
        }

        for (table_entry_index, table_entry) in table.entries.iter().enumerate() {
            let compressed = (table_entry & 0x8000_0000) != 0;
            let chunk_data_offset = u64::from(table_entry & 0x7fff_ffff);
            let data_offset = table
                .base_offset
                .checked_add(chunk_data_offset)
                .ok_or_else(|| ErrorTrace::new("EWF chunk data offset overflow".to_string()))?;
            let data_end_offset =
                if let Some(next_table_entry) = table.entries.get(table_entry_index + 1) {
                    table
                        .base_offset
                        .checked_add(u64::from(next_table_entry & 0x7fff_ffff))
                        .ok_or_else(|| {
                            ErrorTrace::new("EWF next chunk data offset overflow".to_string())
                        })?
                } else {
                    match last_sectors_section {
                        Some(sectors_section) => sectors_section.next_offset,
                        None => section.next_offset,
                    }
                };

            if data_end_offset <= data_offset {
                return Err(ErrorTrace::new(format!(
                    "Unsupported EWF table entry: {} in segment file: {}",
                    table_entry_index, segment.file_name,
                )));
            }

            let logical_offset = self.next_chunk_logical_offset;
            let logical_size = self.remaining_chunk_logical_size()?;
            let data_size_u64 = data_end_offset - data_offset;
            let data_size = u32::try_from(data_size_u64).map_err(|_| {
                ErrorTrace::new("EWF chunk data size exceeds u32 range".to_string())
            })?;

            self.chunks.push(EwfChunkDescriptor {
                segment_index,
                logical_offset,
                logical_size,
                data_offset,
                data_size,
                compressed,
            });

            self.next_chunk_logical_offset = self
                .next_chunk_logical_offset
                .checked_add(logical_size)
                .ok_or_else(|| ErrorTrace::new("EWF logical media offset overflow".to_string()))?;
        }

        Ok(())
    }

    fn remaining_chunk_logical_size(&self) -> Result<u64, ErrorTrace> {
        if self.next_chunk_logical_offset >= self.media_size {
            return Err(ErrorTrace::new(
                "Parsed more EWF chunks than fit in the declared media size".to_string(),
            ));
        }

        Ok((self.chunk_size as u64).min(self.media_size - self.next_chunk_logical_offset))
    }
}

impl EwfDataSource {
    fn read_cached_chunk(&self, chunk_index: u32) -> Result<Option<Arc<[u8]>>, ErrorTrace> {
        let cache = self.runtime.chunk_cache.read().map_err(|_| {
            ErrorTrace::new("Unable to acquire EWF chunk cache read lock".to_string())
        })?;

        Ok(cache.get(&chunk_index).cloned())
    }

    fn cache_chunk(&self, chunk_index: u32, data: Arc<[u8]>) -> Result<Arc<[u8]>, ErrorTrace> {
        let mut cache = self.runtime.chunk_cache.write().map_err(|_| {
            ErrorTrace::new("Unable to acquire EWF chunk cache write lock".to_string())
        })?;

        let entry = cache.entry(chunk_index).or_insert_with(|| data);
        Ok(entry.clone())
    }

    fn read_compressed_chunk(
        &self,
        chunk_index: u32,
        chunk: &EwfChunkDescriptor,
    ) -> Result<Arc<[u8]>, ErrorTrace> {
        if let Some(chunk_data) = self.read_cached_chunk(chunk_index)? {
            return Ok(chunk_data);
        }

        let segment = self
            .runtime
            .segments
            .get(chunk.segment_index)
            .ok_or_else(|| {
                ErrorTrace::new("Missing EWF segment for chunk descriptor".to_string())
            })?;
        let mut compressed_data = vec![0; chunk.data_size as usize];

        segment
            .source
            .read_exact_at(chunk.data_offset, &mut compressed_data)?;

        let mut decompressed_data = vec![0; self.runtime.chunk_size as usize];
        let mut zlib_context = ZlibContext::new();

        zlib_context.decompress(&compressed_data, &mut decompressed_data)?;

        if zlib_context.uncompressed_data_size < chunk.logical_size as usize {
            return Err(ErrorTrace::new(
                "Decompressed EWF chunk is smaller than its logical size".to_string(),
            ));
        }

        let chunk_data: Arc<[u8]> = decompressed_data[..chunk.logical_size as usize]
            .to_vec()
            .into();

        self.cache_chunk(chunk_index, chunk_data)
    }
}

impl DataSource for EwfDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if offset >= self.runtime.media_size || buf.is_empty() {
            return Ok(0);
        }

        let mut written: usize = 0;
        let mut current_offset: u64 = offset;

        while written < buf.len() && current_offset < self.runtime.media_size {
            let chunk_index_u64 = current_offset / (self.runtime.chunk_size as u64);
            let chunk_index = usize::try_from(chunk_index_u64)
                .map_err(|_| ErrorTrace::new("EWF chunk index exceeds usize range".to_string()))?;
            let chunk = self.runtime.chunks.get(chunk_index).ok_or_else(|| {
                ErrorTrace::new("Missing EWF chunk descriptor for logical offset".to_string())
            })?;
            let chunk_relative_offset = current_offset - chunk.logical_offset;
            let chunk_available = usize::try_from(chunk.logical_size - chunk_relative_offset)
                .unwrap_or(usize::MAX)
                .min(buf.len() - written);

            if chunk.compressed {
                let chunk_data = self.read_compressed_chunk(chunk_index as u32, chunk)?;

                buf[written..written + chunk_available].copy_from_slice(
                    &chunk_data[chunk_relative_offset as usize
                        ..chunk_relative_offset as usize + chunk_available],
                );
            } else {
                let segment = self
                    .runtime
                    .segments
                    .get(chunk.segment_index)
                    .ok_or_else(|| {
                        ErrorTrace::new("Missing EWF segment for chunk descriptor".to_string())
                    })?;
                let data_offset = chunk
                    .data_offset
                    .checked_add(chunk_relative_offset)
                    .ok_or_else(|| ErrorTrace::new("EWF data offset overflow".to_string()))?;

                segment
                    .source
                    .read_exact_at(data_offset, &mut buf[written..written + chunk_available])?;
            }

            written += chunk_available;
            current_offset = current_offset
                .checked_add(chunk_available as u64)
                .ok_or_else(|| ErrorTrace::new("EWF read offset overflow".to_string()))?;
        }

        Ok(written)
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        Ok(self.runtime.media_size)
    }

    fn capabilities(&self) -> DataSourceCapabilities {
        DataSourceCapabilities::concurrent(DataSourceSeekCost::Expensive)
            .with_preferred_chunk_size(self.runtime.chunk_size as usize)
    }

    fn telemetry_name(&self) -> &'static str {
        "ewf"
    }
}

fn read_file_header(source: &dyn DataSource) -> Result<u16, ErrorTrace> {
    let mut data = [0u8; 13];

    source.read_exact_at(0, &mut data)?;

    if &data[0..8] != EWF_FILE_HEADER_SIGNATURE {
        return Err(ErrorTrace::new(
            "Unsupported EWF file header signature".to_string(),
        ));
    }
    if data[8] != 1 {
        return Err(ErrorTrace::new(
            "Unsupported EWF file header start of fields".to_string(),
        ));
    }
    if data[11..13] != [0; 2] {
        return Err(ErrorTrace::new(
            "Unsupported EWF file header end of fields".to_string(),
        ));
    }

    Ok(u16::from_le_bytes([data[9], data[10]]))
}

fn read_section_headers(source: &dyn DataSource) -> Result<Vec<EwfSectionHeader>, ErrorTrace> {
    let file_size = source.size()?;

    if file_size < 13 {
        return Err(ErrorTrace::new(
            "Unsupported EWF segment size smaller than file header".to_string(),
        ));
    }

    let mut section_headers = Vec::new();
    let mut file_offset: u64 = 13;

    while file_offset < file_size {
        let mut section_header = EwfSectionHeader::read_at(source, file_offset)?;
        let is_done_or_next = section_header.is_type(EWF_SECTION_TYPE_DONE)
            || section_header.is_type(EWF_SECTION_TYPE_NEXT);

        if is_done_or_next {
            if section_header.size == 0 {
                section_header.size = 76;
            } else if section_header.size != 76 {
                return Err(ErrorTrace::new(
                    "Unsupported EWF done or next section size".to_string(),
                ));
            }
            if section_header.next_offset != file_offset {
                return Err(ErrorTrace::new(
                    "Unsupported EWF done or next section next offset does not align with file offset"
                        .to_string(),
                ));
            }
        } else {
            if section_header.next_offset <= file_offset {
                return Err(ErrorTrace::new(
                    "Unsupported EWF section next offset".to_string(),
                ));
            }
            let calculated_size = section_header.next_offset - file_offset;

            if section_header.size == 0 {
                section_header.size = calculated_size;
            } else if section_header.size != calculated_size {
                return Err(ErrorTrace::new(
                    "Unsupported EWF section size value does not align with next offset"
                        .to_string(),
                ));
            }
            if section_header.size < 76 {
                return Err(ErrorTrace::new(
                    "Unsupported EWF section size value too small".to_string(),
                ));
            }
        }

        file_offset = file_offset
            .checked_add(section_header.size)
            .ok_or_else(|| {
                ErrorTrace::new("EWF section offset overflow while walking headers".to_string())
            })?;
        section_headers.push(section_header.clone());

        if is_done_or_next {
            break;
        }
    }

    if file_offset != file_size {
        return Err(ErrorTrace::new(
            "Unsupported trailing data after the last EWF section header".to_string(),
        ));
    }

    Ok(section_headers)
}

fn read_volume_section(
    source: &dyn DataSource,
    section: &EwfSectionHeader,
) -> Result<EwfVolumeInfo, ErrorTrace> {
    let data = read_section_data(source, section)?;

    match data.len() {
        1052 => read_e01_volume_section(&data),
        94 => read_s01_volume_section(&data),
        _ => Err(ErrorTrace::new(format!(
            "Unsupported EWF volume section data size: {}",
            data.len(),
        ))),
    }
}

fn read_e01_volume_section(data: &[u8]) -> Result<EwfVolumeInfo, ErrorTrace> {
    let stored_checksum = u32::from_le_bytes([data[1048], data[1049], data[1050], data[1051]]);
    let calculated_checksum = adler32(&data[0..1048]);

    if stored_checksum != 0 && stored_checksum != calculated_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF volume checksums",
            stored_checksum, calculated_checksum,
        )));
    }

    Ok(EwfVolumeInfo {
        media_type: match data[0] {
            0x00 => EwfMediaType::RemoveableDisk,
            0x01 => EwfMediaType::FixedDisk,
            0x03 => EwfMediaType::OpticalDisk,
            0x0e => EwfMediaType::LogicalEvidence,
            0x10 => EwfMediaType::Memory,
            _ => EwfMediaType::Unknown,
        },
        set_identifier: Uuid::from_le_bytes(&data[64..80]),
        number_of_chunks: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        sectors_per_chunk: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        bytes_per_sector: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        number_of_sectors: u64::from_le_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]),
        error_granularity: u32::from_le_bytes([data[56], data[57], data[58], data[59]]),
    })
}

fn read_s01_volume_section(data: &[u8]) -> Result<EwfVolumeInfo, ErrorTrace> {
    let stored_checksum = u32::from_le_bytes([data[90], data[91], data[92], data[93]]);
    let calculated_checksum = adler32(&data[0..90]);

    if stored_checksum != calculated_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF S01 volume checksums",
            stored_checksum, calculated_checksum,
        )));
    }

    Ok(EwfVolumeInfo {
        media_type: EwfMediaType::LogicalEvidence,
        set_identifier: Uuid::new(),
        number_of_chunks: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        sectors_per_chunk: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        bytes_per_sector: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        number_of_sectors: u64::from(u32::from_le_bytes([data[16], data[17], data[18], data[19]])),
        error_granularity: 0,
    })
}

struct EwfTable {
    base_offset: u64,
    entries: Vec<u32>,
}

fn read_table_section(
    source: &dyn DataSource,
    section: &EwfSectionHeader,
) -> Result<EwfTable, ErrorTrace> {
    let data = read_section_data(source, section)?;

    if data.len() < 28 || data.len() > 16_777_216 {
        return Err(ErrorTrace::new(format!(
            "Unsupported EWF table data size: {} value out of bounds",
            data.len(),
        )));
    }

    let stored_header_checksum = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let calculated_header_checksum = adler32(&data[0..20]);

    if stored_header_checksum != calculated_header_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF table header checksums",
            stored_header_checksum, calculated_header_checksum,
        )));
    }

    let number_of_entries = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let base_offset = u64::from_le_bytes([
        data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
    ]);
    let expected_size =
        24usize
            .checked_add(number_of_entries.checked_mul(4).ok_or_else(|| {
                ErrorTrace::new("EWF table entry array size overflow".to_string())
            })?)
            .and_then(|value| value.checked_add(4))
            .ok_or_else(|| ErrorTrace::new("EWF table size overflow".to_string()))?;

    if expected_size != data.len() {
        return Err(ErrorTrace::new(format!(
            "Unsupported EWF table data size: {} does not match number of entries: {}",
            data.len(),
            number_of_entries,
        )));
    }

    let footer_offset = 24 + (number_of_entries * 4);
    let stored_footer_checksum = u32::from_le_bytes([
        data[footer_offset],
        data[footer_offset + 1],
        data[footer_offset + 2],
        data[footer_offset + 3],
    ]);
    let calculated_footer_checksum = adler32(&data[24..footer_offset]);

    if stored_footer_checksum != calculated_footer_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF table checksums",
            stored_footer_checksum, calculated_footer_checksum,
        )));
    }

    let mut entries = Vec::with_capacity(number_of_entries);

    for entry_index in 0..number_of_entries {
        let data_offset = 24 + (entry_index * 4);

        entries.push(u32::from_le_bytes([
            data[data_offset],
            data[data_offset + 1],
            data[data_offset + 2],
            data[data_offset + 3],
        ]));
    }

    Ok(EwfTable {
        base_offset,
        entries,
    })
}

fn read_hash_section(
    source: &dyn DataSource,
    section: &EwfSectionHeader,
) -> Result<[u8; 16], ErrorTrace> {
    let data = read_section_data(source, section)?;

    if data.len() < 36 {
        return Err(ErrorTrace::new(
            "Unsupported EWF hash data size".to_string(),
        ));
    }

    let stored_checksum = u32::from_le_bytes([data[32], data[33], data[34], data[35]]);
    let calculated_checksum = adler32(&data[0..32]);

    if stored_checksum != calculated_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF hash checksums",
            stored_checksum, calculated_checksum,
        )));
    }

    let mut md5_hash = [0u8; 16];
    md5_hash.copy_from_slice(&data[0..16]);
    Ok(md5_hash)
}

fn read_digest_section(
    source: &dyn DataSource,
    section: &EwfSectionHeader,
) -> Result<([u8; 16], [u8; 20]), ErrorTrace> {
    let data = read_section_data(source, section)?;

    if data.len() < 80 {
        return Err(ErrorTrace::new(
            "Unsupported EWF digest data size".to_string(),
        ));
    }

    let stored_checksum = u32::from_le_bytes([data[76], data[77], data[78], data[79]]);
    let calculated_checksum = adler32(&data[0..76]);

    if stored_checksum != calculated_checksum {
        return Err(ErrorTrace::new(format!(
            "Mismatch between stored: 0x{:08x} and calculated: 0x{:08x} EWF digest checksums",
            stored_checksum, calculated_checksum,
        )));
    }

    let mut md5_hash = [0u8; 16];
    let mut sha1_hash = [0u8; 20];

    md5_hash.copy_from_slice(&data[0..16]);
    sha1_hash.copy_from_slice(&data[16..36]);

    Ok((md5_hash, sha1_hash))
}

fn read_section_data(
    source: &dyn DataSource,
    section: &EwfSectionHeader,
) -> Result<Vec<u8>, ErrorTrace> {
    let data_size = usize::try_from(section.data_size())
        .map_err(|_| ErrorTrace::new("EWF section data size exceeds usize range".to_string()))?;
    let mut data = vec![0; data_size];

    source.read_exact_at(section.data_offset(), &mut data)?;

    Ok(data)
}

fn adler32(data: &[u8]) -> u32 {
    let mut context = Adler32Context::new(1);
    context.update(data);
    context.finalize()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use keramics_core::formatters::format_as_string;
    use keramics_hashes::{DigestHashContext, Md5Context};

    use super::*;
    use crate::resolver::open_local_source_resolver;
    use crate::source::DataSourceCursor;
    use crate::tests::get_test_data_path;

    fn get_image() -> Result<EwfImage, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("ewf"));
        let resolver = open_local_source_resolver(&path)?;

        EwfImage::open(&resolver, Path::new("ext2.E01"))
    }

    fn read_media_from_image(image: &EwfImage) -> Result<(u64, String), ErrorTrace> {
        let source = image.open_source();
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

    #[test]
    fn test_get_segment_file_extension() -> Result<(), ErrorTrace> {
        assert_eq!(
            EwfImage::get_segment_file_extension(1, &EwfNamingSchema::E01Upper)?,
            "E01"
        );
        assert_eq!(
            EwfImage::get_segment_file_extension(99, &EwfNamingSchema::E01Upper)?,
            "E99"
        );
        assert_eq!(
            EwfImage::get_segment_file_extension(100, &EwfNamingSchema::E01Upper)?,
            "EAA"
        );
        assert_eq!(
            EwfImage::get_segment_file_extension(14971, &EwfNamingSchema::E01Upper)?,
            "ZZZ"
        );
        assert!(EwfImage::get_segment_file_extension(14972, &EwfNamingSchema::E01Upper).is_err());
        assert_eq!(
            EwfImage::get_segment_file_extension(1, &EwfNamingSchema::S01Lower)?,
            "s01"
        );

        Ok(())
    }

    #[test]
    fn test_get_segment_file_name() -> Result<(), ErrorTrace> {
        let name = EwfImage::get_segment_file_name("image", 1, &EwfNamingSchema::E01Upper)?;
        assert_eq!(name, "image.E01");
        Ok(())
    }

    #[test]
    fn test_get_segment_file_naming_schema() -> Result<(), ErrorTrace> {
        assert_eq!(
            EwfImage::get_segment_file_naming_schema("image.E01")?,
            EwfNamingSchema::E01Upper
        );
        assert_eq!(
            EwfImage::get_segment_file_naming_schema("image.e01")?,
            EwfNamingSchema::E01Lower
        );
        assert_eq!(
            EwfImage::get_segment_file_naming_schema("image.S01")?,
            EwfNamingSchema::S01Upper
        );
        assert!(EwfImage::get_segment_file_naming_schema("image.raw").is_err());
        Ok(())
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let image = get_image()?;

        assert_eq!(image.media_type(), EwfMediaType::FixedDisk);
        assert_eq!(image.number_of_segments(), 1);
        assert_eq!(image.segment_file_names(), vec!["ext2.E01"]);
        assert_eq!(image.number_of_chunks(), 128);
        assert_eq!(image.sectors_per_chunk(), 64);
        assert_eq!(image.bytes_per_sector(), 512);
        assert_eq!(image.number_of_sectors(), 8192);
        assert_eq!(image.chunk_size(), 32_768);
        assert_eq!(image.media_size(), 4_194_304);

        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source();
        let capabilities = source.capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            crate::source::DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Expensive);
        assert_eq!(capabilities.preferred_chunk_size, Some(32_768));
        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
        Ok(())
    }

    #[test]
    fn test_read_media() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let (media_offset, md5_hash) = read_media_from_image(&image)?;

        assert_eq!(media_offset, image.media_size());
        assert_eq!(md5_hash.as_str(), "b1760d0b35a512ef56970df4e6f8c5d6");
        Ok(())
    }
}
