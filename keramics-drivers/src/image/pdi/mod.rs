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
use std::collections::HashMap;
use std::sync::Arc;

use keramics_core::ErrorTrace;
use keramics_types::Uuid;

use crate::resolver::SourceResolverReference;
use crate::source::{DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};

const PDI_SPARSE_FILE_HEADER_SIGNATURE1: &[u8; 16] = b"WithoutFreeSpace";
const PDI_SPARSE_FILE_HEADER_SIGNATURE2: &[u8; 16] = b"WithouFreSpacExt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PdiDescriptorImageType {
    Compressed,
    Plain,
}

#[derive(Clone)]
struct PdiDescriptorImage {
    file: String,
    image_type: PdiDescriptorImageType,
    snapshot_identifier: Uuid,
}

#[derive(Clone)]
struct PdiDescriptorExtent {
    start_sector: u64,
    end_sector: u64,
    images: Vec<PdiDescriptorImage>,
}

#[derive(Clone)]
struct PdiDescriptorSnapshot {
    identifier: Uuid,
    parent_identifier: Option<Uuid>,
}

#[derive(Clone)]
struct PdiSparseFileHeader {
    sectors_per_block: u32,
    number_of_blocks: u32,
    number_of_sectors: u64,
    data_start_sector: u32,
}

/// Immutable PDI image layer metadata plus opened logical source.
#[derive(Clone)]
pub struct PdiImageLayer {
    identifier: Uuid,
    parent_identifier: Option<Uuid>,
    media_size: u64,
    logical_source: DataSourceReference,
}

/// Immutable PDI image metadata plus opened layers.
pub struct PdiImage {
    bytes_per_sector: u16,
    media_size: u64,
    layers: Vec<PdiImageLayer>,
}

impl PdiImage {
    /// Opens and parses a PDI image from a resolver rooted at the `.hdd` directory.
    pub fn open(resolver: &SourceResolverReference) -> Result<Self, ErrorTrace> {
        let disk_descriptor_source = resolver
            .open_source(std::path::Path::new("DiskDescriptor.xml"))?
            .ok_or_else(|| {
                ErrorTrace::new("Missing data source: DiskDescriptor.xml".to_string())
            })?;
        let disk_descriptor_size = disk_descriptor_source.size()?;

        if disk_descriptor_size == 0 || disk_descriptor_size > 65_536 {
            return Err(ErrorTrace::new(
                "Unsupported PDI DiskDescriptor.xml size".to_string(),
            ));
        }

        let disk_descriptor_data = disk_descriptor_source.read_all()?;
        let disk_descriptor = String::from_utf8(disk_descriptor_data).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to convert PDI XML data into UTF-8 string with error: {}",
                error,
            ))
        })?;

        if !disk_descriptor.contains("<Parallels_disk_image") {
            return Err(ErrorTrace::new(
                "Unsupported PDI XML document - missing root element".to_string(),
            ));
        }

        let bytes_per_sector: u16 = 512;
        let media_size = parse_disk_parameters(&disk_descriptor, bytes_per_sector)?;
        let snapshots = parse_snapshots(&disk_descriptor)?;
        let descriptor_extents = parse_storage_data(&disk_descriptor)?;
        let layers = build_layers(
            resolver,
            &snapshots,
            &descriptor_extents,
            media_size,
            bytes_per_sector,
        )?;

        Ok(Self {
            bytes_per_sector,
            media_size,
            layers,
        })
    }

    /// Retrieves the bytes-per-sector value.
    pub fn bytes_per_sector(&self) -> u16 {
        self.bytes_per_sector
    }

    /// Retrieves the media size.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Retrieves the number of layers.
    pub fn number_of_layers(&self) -> usize {
        self.layers.len()
    }

    /// Retrieves a layer by index.
    pub fn layer(&self, layer_index: usize) -> Result<&PdiImageLayer, ErrorTrace> {
        self.layers
            .get(layer_index)
            .ok_or_else(|| ErrorTrace::new(format!("No layer with index: {}", layer_index)))
    }

    /// Opens the logical source of the top image layer.
    pub fn open_source(&self) -> Result<DataSourceReference, ErrorTrace> {
        self.layers
            .last()
            .map(|layer| layer.open_source())
            .ok_or_else(|| ErrorTrace::new("Missing PDI image layer".to_string()))
    }
}

impl PdiImageLayer {
    /// Retrieves the layer identifier.
    pub fn identifier(&self) -> &Uuid {
        &self.identifier
    }

    /// Retrieves the parent identifier if present.
    pub fn parent_identifier(&self) -> Option<&Uuid> {
        self.parent_identifier.as_ref()
    }

    /// Retrieves the media size.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Opens the logical source of the image layer.
    pub fn open_source(&self) -> DataSourceReference {
        self.logical_source.clone()
    }
}

fn parse_disk_parameters(data: &str, bytes_per_sector: u16) -> Result<u64, ErrorTrace> {
    let disk_parameters = extract_first_section(data, "Disk_Parameters")?;
    let disk_size = extract_text(disk_parameters, "Disk_size")?
        .parse::<u64>()
        .map_err(|error| {
            ErrorTrace::new(format!(
                "Unsupported PDI Disk_size value with error: {}",
                error,
            ))
        })?;
    let logical_sector_size = extract_text(disk_parameters, "LogicSectorSize")?
        .parse::<u64>()
        .map_err(|error| {
            ErrorTrace::new(format!(
                "Unsupported PDI LogicSectorSize value with error: {}",
                error,
            ))
        })?;
    let physical_sector_size = extract_text(disk_parameters, "PhysicalSectorSize")?
        .parse::<u64>()
        .map_err(|error| {
            ErrorTrace::new(format!(
                "Unsupported PDI PhysicalSectorSize value with error: {}",
                error,
            ))
        })?;
    let padding = extract_text(disk_parameters, "Padding")?
        .parse::<u64>()
        .map_err(|error| {
            ErrorTrace::new(format!(
                "Unsupported PDI Padding value with error: {}",
                error,
            ))
        })?;

    if logical_sector_size != bytes_per_sector as u64 {
        return Err(ErrorTrace::new(format!(
            "Unsupported PDI logical sector size: {}",
            logical_sector_size,
        )));
    }
    if physical_sector_size != 4096 {
        return Err(ErrorTrace::new(format!(
            "Unsupported PDI physical sector size: {}",
            physical_sector_size,
        )));
    }
    if padding != 0 {
        return Err(ErrorTrace::new(format!(
            "Unsupported PDI padding value: {}",
            padding,
        )));
    }

    disk_size
        .checked_mul(bytes_per_sector as u64)
        .ok_or_else(|| ErrorTrace::new("Unsupported PDI disk size value out of bounds".to_string()))
}

fn parse_snapshots(data: &str) -> Result<Vec<PdiDescriptorSnapshot>, ErrorTrace> {
    let snapshots_section = extract_first_section(data, "Snapshots")?;
    let mut snapshots = Vec::new();

    for snapshot_section in extract_all_sections(snapshots_section, "Shot")? {
        let identifier =
            Uuid::from_string(extract_text(snapshot_section, "GUID")?).map_err(|error| {
                ErrorTrace::new(format!(
                    "Unsupported PDI snapshot GUID value with error: {}",
                    error
                ))
            })?;
        let parent_identifier = Uuid::from_string(extract_text(snapshot_section, "ParentGUID")?)
            .map_err(|error| {
                ErrorTrace::new(format!(
                    "Unsupported PDI snapshot ParentGUID value with error: {}",
                    error,
                ))
            })?;

        snapshots.push(PdiDescriptorSnapshot {
            identifier,
            parent_identifier: if parent_identifier.is_nil() {
                None
            } else {
                Some(parent_identifier)
            },
        });
    }
    Ok(snapshots)
}

fn parse_storage_data(data: &str) -> Result<Vec<PdiDescriptorExtent>, ErrorTrace> {
    let storage_data = extract_first_section(data, "StorageData")?;
    let mut descriptor_extents = Vec::new();
    let mut last_end_sector: u64 = 0;

    for storage_section in extract_all_sections(storage_data, "Storage")? {
        let start_sector = extract_text(storage_section, "Start")?
            .parse::<u64>()
            .map_err(|error| {
                ErrorTrace::new(format!("Unsupported PDI Start value with error: {}", error))
            })?;
        let end_sector = extract_text(storage_section, "End")?
            .parse::<u64>()
            .map_err(|error| {
                ErrorTrace::new(format!("Unsupported PDI End value with error: {}", error))
            })?;
        let block_size = extract_text(storage_section, "Blocksize")?
            .parse::<u64>()
            .map_err(|error| {
                ErrorTrace::new(format!(
                    "Unsupported PDI Blocksize value with error: {}",
                    error
                ))
            })?;

        if block_size != 2048 {
            return Err(ErrorTrace::new(format!(
                "Unsupported PDI block size: {}",
                block_size,
            )));
        }
        if start_sector >= end_sector {
            return Err(ErrorTrace::new(format!(
                "Unsupported PDI extent start sector: {} exceeds end sector: {}",
                start_sector, end_sector,
            )));
        }
        if start_sector != last_end_sector {
            return Err(ErrorTrace::new(format!(
                "Unsupported PDI extent start sector: {} value not aligned with last end sector: {}",
                start_sector, last_end_sector,
            )));
        }

        let mut images = Vec::new();
        for image_section in extract_all_sections(storage_section, "Image")? {
            let snapshot_identifier = Uuid::from_string(extract_text(image_section, "GUID")?)
                .map_err(|error| {
                    ErrorTrace::new(format!(
                        "Unsupported PDI image GUID value with error: {}",
                        error,
                    ))
                })?;
            let image_type = match extract_text(image_section, "Type")? {
                "Compressed" => PdiDescriptorImageType::Compressed,
                "Plain" => PdiDescriptorImageType::Plain,
                value => {
                    return Err(ErrorTrace::new(format!(
                        "Unsupported PDI image Type value: {}",
                        value,
                    )));
                }
            };
            let file = extract_text(image_section, "File")?.to_string();

            if file.is_empty() {
                return Err(ErrorTrace::new("Missing PDI File value".to_string()));
            }

            images.push(PdiDescriptorImage {
                file,
                image_type,
                snapshot_identifier,
            });
        }

        descriptor_extents.push(PdiDescriptorExtent {
            start_sector,
            end_sector,
            images,
        });
        last_end_sector = end_sector;
    }

    Ok(descriptor_extents)
}

fn build_layers(
    resolver: &SourceResolverReference,
    snapshots: &[PdiDescriptorSnapshot],
    descriptor_extents: &[PdiDescriptorExtent],
    media_size: u64,
    bytes_per_sector: u16,
) -> Result<Vec<PdiImageLayer>, ErrorTrace> {
    let snapshot_lookup: HashMap<Uuid, PdiDescriptorSnapshot> = snapshots
        .iter()
        .cloned()
        .map(|snapshot| (snapshot.identifier.clone(), snapshot))
        .collect();
    let mut ordered_snapshot_identifiers = snapshots
        .iter()
        .map(|snapshot| {
            Ok((
                count_snapshot_ancestors(&snapshot.identifier, &snapshot_lookup)?,
                snapshot.identifier.clone(),
            ))
        })
        .collect::<Result<Vec<(usize, Uuid)>, ErrorTrace>>()?;
    let mut layer_sources: HashMap<Uuid, DataSourceReference> = HashMap::new();
    let mut layers = Vec::new();

    ordered_snapshot_identifiers.sort_by(|left, right| left.0.cmp(&right.0));

    for (_, snapshot_identifier) in ordered_snapshot_identifiers {
        let snapshot = snapshot_lookup.get(&snapshot_identifier).ok_or_else(|| {
            ErrorTrace::new(format!("Missing PDI layer: {}", snapshot_identifier))
        })?;
        let parent_source = snapshot
            .parent_identifier
            .as_ref()
            .and_then(|parent_identifier| layer_sources.get(parent_identifier).cloned());
        let extents = build_layer_extents(
            resolver,
            descriptor_extents,
            &snapshot.identifier,
            parent_source.clone(),
            bytes_per_sector,
        )?;
        let logical_source: DataSourceReference = Arc::new(ExtentMapDataSource::new(extents)?);
        let layer = PdiImageLayer {
            identifier: snapshot.identifier.clone(),
            parent_identifier: snapshot.parent_identifier.clone(),
            media_size,
            logical_source: logical_source.clone(),
        };

        layer_sources.insert(snapshot.identifier.clone(), logical_source);
        layers.push(layer);
    }

    Ok(layers)
}

fn count_snapshot_ancestors(
    identifier: &Uuid,
    snapshot_lookup: &HashMap<Uuid, PdiDescriptorSnapshot>,
) -> Result<usize, ErrorTrace> {
    let mut number_of_ancestors: usize = 0;
    let mut current_parent_identifier = snapshot_lookup
        .get(identifier)
        .ok_or_else(|| ErrorTrace::new(format!("Missing PDI layer: {}", identifier)))?
        .parent_identifier
        .as_ref();

    while let Some(parent_identifier) = current_parent_identifier {
        let parent_snapshot = snapshot_lookup
            .get(parent_identifier)
            .ok_or_else(|| ErrorTrace::new(format!("Missing PDI layer: {}", parent_identifier)))?;

        number_of_ancestors += 1;
        current_parent_identifier = parent_snapshot.parent_identifier.as_ref();
    }

    Ok(number_of_ancestors)
}

fn build_layer_extents(
    resolver: &SourceResolverReference,
    descriptor_extents: &[PdiDescriptorExtent],
    snapshot_identifier: &Uuid,
    parent_source: Option<DataSourceReference>,
    bytes_per_sector: u16,
) -> Result<Vec<ExtentMapEntry>, ErrorTrace> {
    let mut extents = Vec::new();

    for descriptor_extent in descriptor_extents {
        let extent_offset = descriptor_extent
            .start_sector
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("PDI extent offset overflow".to_string()))?;
        let extent_size = (descriptor_extent.end_sector - descriptor_extent.start_sector)
            .checked_mul(bytes_per_sector as u64)
            .ok_or_else(|| ErrorTrace::new("PDI extent size overflow".to_string()))?;
        let image = descriptor_extent
            .images
            .iter()
            .find(|image| &image.snapshot_identifier == snapshot_identifier);

        match image {
            Some(image) => {
                let image_source = resolver
                    .open_source(std::path::Path::new(image.file.as_str()))?
                    .ok_or_else(|| {
                        ErrorTrace::new(format!("Missing PDI image data source: {}", image.file))
                    })?;

                match image.image_type {
                    PdiDescriptorImageType::Plain => {
                        if image_source.size()? != extent_size {
                            return Err(ErrorTrace::new(format!(
                                "Unsupported PDI file size: {} value does not align with extent: {}",
                                image_source.size()?,
                                extent_size,
                            )));
                        }

                        extents.push(ExtentMapEntry {
                            logical_offset: extent_offset,
                            size: extent_size,
                            target: ExtentMapTarget::Data {
                                source: image_source,
                                source_offset: 0,
                            },
                        });
                    }
                    PdiDescriptorImageType::Compressed => {
                        extents.extend(build_sparse_extent_entries(
                            image_source,
                            extent_offset,
                            extent_size,
                            parent_source.clone(),
                        )?);
                    }
                }
            }
            None => {
                extents.push(ExtentMapEntry {
                    logical_offset: extent_offset,
                    size: extent_size,
                    target: match &parent_source {
                        Some(parent_source) => ExtentMapTarget::Data {
                            source: parent_source.clone(),
                            source_offset: extent_offset,
                        },
                        None => ExtentMapTarget::Zero,
                    },
                });
            }
        }
    }

    Ok(extents)
}

fn build_sparse_extent_entries(
    source: DataSourceReference,
    extent_offset: u64,
    extent_size: u64,
    parent_source: Option<DataSourceReference>,
) -> Result<Vec<ExtentMapEntry>, ErrorTrace> {
    let source_size = source.size()?;
    let file_header = PdiSparseFileHeader::read_at(source.as_ref(), 0)?;
    let block_size = (file_header.sectors_per_block as u64)
        .checked_mul(512)
        .ok_or_else(|| ErrorTrace::new("PDI block size overflow".to_string()))?;

    if file_header
        .number_of_sectors
        .checked_mul(512)
        .ok_or_else(|| ErrorTrace::new("PDI sparse file number of sectors overflow".to_string()))?
        != extent_size
    {
        return Err(ErrorTrace::new(format!(
            "Unsupported PDI sparse file header number of sectors: {} value does not align with extent size: {}",
            file_header.number_of_sectors, extent_size,
        )));
    }
    if file_header.data_start_sector == 0 {
        return Err(ErrorTrace::new(
            "Invalid PDI data start sector value out of bounds".to_string(),
        ));
    }
    if (file_header.number_of_blocks as u64) * block_size < extent_size {
        return Err(ErrorTrace::new(
            "PDI sparse file block table is too small for extent size".to_string(),
        ));
    }

    let mut extents = Vec::new();
    let mut current_extent: Option<ExtentMapEntry> = None;

    for block_index in 0..file_header.number_of_blocks as u64 {
        let block_extent_offset = block_index
            .checked_mul(block_size)
            .ok_or_else(|| ErrorTrace::new("PDI block extent offset overflow".to_string()))?;

        if block_extent_offset >= extent_size {
            break;
        }

        let block_logical_size = min(block_size, extent_size - block_extent_offset);
        let sector_number = read_u32_le(source.as_ref(), 64 + (block_index * 4))?;
        let extent = if sector_number == 0 {
            ExtentMapEntry {
                logical_offset: extent_offset + block_extent_offset,
                size: block_logical_size,
                target: match &parent_source {
                    Some(parent_source) => ExtentMapTarget::Data {
                        source: parent_source.clone(),
                        source_offset: extent_offset + block_extent_offset,
                    },
                    None => ExtentMapTarget::Zero,
                },
            }
        } else {
            let block_data_offset = (sector_number as u64)
                .checked_mul(512)
                .ok_or_else(|| ErrorTrace::new("PDI block data offset overflow".to_string()))?;
            let block_data_end = block_data_offset
                .checked_add(block_logical_size)
                .ok_or_else(|| ErrorTrace::new("PDI block data end offset overflow".to_string()))?;

            if block_data_end > source_size {
                return Err(ErrorTrace::new(format!(
                    "PDI block data exceeds file size at logical offset: {}",
                    extent_offset + block_extent_offset,
                )));
            }

            ExtentMapEntry {
                logical_offset: extent_offset + block_extent_offset,
                size: block_logical_size,
                target: ExtentMapTarget::Data {
                    source: source.clone(),
                    source_offset: block_data_offset,
                },
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

impl PdiSparseFileHeader {
    fn read_at(source: &dyn crate::source::DataSource, offset: u64) -> Result<Self, ErrorTrace> {
        let mut data = [0u8; 64];

        source.read_exact_at(offset, &mut data)?;

        if &data[0..16] != PDI_SPARSE_FILE_HEADER_SIGNATURE1
            && &data[0..16] != PDI_SPARSE_FILE_HEADER_SIGNATURE2
        {
            return Err(ErrorTrace::new(
                "Unsupported PDI sparse file signature".to_string(),
            ));
        }

        Ok(Self {
            sectors_per_block: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            number_of_blocks: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            number_of_sectors: u64::from_le_bytes([
                data[36], data[37], data[38], data[39], data[40], data[41], data[42], data[43],
            ]),
            data_start_sector: u32::from_le_bytes([data[48], data[49], data[50], data[51]]),
        })
    }
}

fn extract_first_section<'a>(data: &'a str, tag: &str) -> Result<&'a str, ErrorTrace> {
    extract_all_sections(data, tag)?
        .into_iter()
        .next()
        .ok_or_else(|| ErrorTrace::new(format!("Missing PDI XML element: {}", tag)))
}

fn extract_all_sections<'a>(data: &'a str, tag: &str) -> Result<Vec<&'a str>, ErrorTrace> {
    let open_tag = format!("<{}>", tag);
    let close_tag = format!("</{}>", tag);
    let mut sections = Vec::new();
    let mut data_offset: usize = 0;

    while let Some(start_offset_relative) = data[data_offset..].find(open_tag.as_str()) {
        let start_offset = data_offset + start_offset_relative + open_tag.len();
        let end_offset_relative = data[start_offset..]
            .find(close_tag.as_str())
            .ok_or_else(|| ErrorTrace::new(format!("Missing closing PDI XML element: {}", tag)))?;
        let end_offset = start_offset + end_offset_relative;

        sections.push(&data[start_offset..end_offset]);
        data_offset = end_offset + close_tag.len();
    }

    Ok(sections)
}

fn extract_text<'a>(data: &'a str, tag: &str) -> Result<&'a str, ErrorTrace> {
    Ok(extract_first_section(data, tag)?.trim())
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
    use crate::resolver::open_local_source_resolver;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn open_image() -> Result<PdiImage, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("pdi/hfsplus.hdd"));
        let resolver = open_local_source_resolver(&path)?;

        PdiImage::open(&resolver)
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let image = open_image()?;

        assert_eq!(image.bytes_per_sector(), 512);
        assert_eq!(image.media_size(), 33_554_432);
        assert_eq!(image.number_of_layers(), 1);
        Ok(())
    }

    #[test]
    fn test_layer() -> Result<(), ErrorTrace> {
        let image = open_image()?;
        let layer = image.layer(0)?;

        assert_eq!(
            layer.identifier().to_string(),
            "5fbaabe3-6958-40ff-92a7-860e329aab41"
        );
        assert_eq!(layer.parent_identifier(), None);
        assert_eq!(layer.media_size(), 33_554_432);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let image = open_image()?;
        let capabilities = image.open_source()?.capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let image = open_image()?;
        let source = image.open_source()?;
        let mut data = vec![0; 2];

        source.read_exact_at(1024, &mut data)?;

        assert_eq!(data, [0x00, 0x53]);
        Ok(())
    }

    #[test]
    fn test_open_layer_source() -> Result<(), ErrorTrace> {
        let image = open_image()?;
        let source = image.layer(0)?.open_source();
        let mut data = vec![0; 2];

        source.read_exact_at(1024, &mut data)?;

        assert_eq!(data, [0x00, 0x53]);
        Ok(())
    }

    #[test]
    fn test_read_media() -> Result<(), ErrorTrace> {
        let image = open_image()?;
        let (media_offset, md5_hash) = read_data_source_md5(image.open_source()?)?;

        assert_eq!(media_offset, image.media_size());
        assert_eq!(md5_hash.as_str(), "ecaef634016fc699807cec47cef11dda");
        Ok(())
    }
}
