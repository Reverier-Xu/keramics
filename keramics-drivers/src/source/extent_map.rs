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

use super::capabilities::{DataSourceCapabilities, DataSourceSeekCost};
use super::data_source::{DataSource, DataSourceReference};
use super::util::combine_data_source_capabilities;

/// Physical extent target.
#[derive(Clone)]
pub enum ExtentMapTarget {
    /// Data stored in an underlying source.
    Data {
        /// Underlying source.
        source: DataSourceReference,

        /// Physical offset within the underlying source.
        source_offset: u64,
    },

    /// Zero-filled sparse extent.
    Zero,
}

/// Logical extent mapping entry.
#[derive(Clone)]
pub struct ExtentMapEntry {
    /// Logical start offset of the extent.
    pub logical_offset: u64,

    /// Size of the extent in bytes.
    pub size: u64,

    /// Physical target for the extent.
    pub target: ExtentMapTarget,
}

/// Logical source backed by immutable extents and sparse regions.
pub struct ExtentMapDataSource {
    entries: Vec<ExtentMapEntry>,
    size: u64,
    capabilities: DataSourceCapabilities,
}

impl ExtentMapDataSource {
    /// Creates a new extent map data source.
    pub fn new(entries: Vec<ExtentMapEntry>) -> Result<Self, ErrorTrace> {
        let mut expected_logical_offset: u64 = 0;
        let mut data_capabilities: Vec<DataSourceCapabilities> = Vec::new();

        for entry in entries.iter() {
            if entry.size == 0 {
                return Err(ErrorTrace::new(
                    "Extent map data source cannot contain zero-sized extents".to_string(),
                ));
            }
            if entry.logical_offset != expected_logical_offset {
                return Err(ErrorTrace::new(
                    "Extent map data source must use contiguous logical offsets".to_string(),
                ));
            }
            if let ExtentMapTarget::Data { source, .. } = &entry.target {
                data_capabilities.push(source.capabilities());
            }
            expected_logical_offset =
                expected_logical_offset
                    .checked_add(entry.size)
                    .ok_or_else(|| {
                        ErrorTrace::new("Extent map data source size overflow".to_string())
                    })?;
        }

        let capabilities = if data_capabilities.is_empty() {
            DataSourceCapabilities::concurrent(DataSourceSeekCost::Cheap)
        } else {
            combine_data_source_capabilities(data_capabilities)
        };

        Ok(Self {
            entries,
            size: expected_logical_offset,
            capabilities,
        })
    }

    fn find_entry(&self, offset: u64) -> Option<&ExtentMapEntry> {
        let entry_index: usize = self
            .entries
            .partition_point(|entry| entry.logical_offset <= offset);

        if entry_index == 0 {
            None
        } else {
            self.entries.get(entry_index - 1)
        }
    }

    fn read_from_source(
        source: &DataSourceReference,
        offset: u64,
        data: &mut [u8],
    ) -> Result<usize, ErrorTrace> {
        let mut read_offset: usize = 0;

        while read_offset < data.len() {
            let current_offset: u64 = offset.checked_add(read_offset as u64).ok_or_else(|| {
                ErrorTrace::new("Extent map physical offset overflow".to_string())
            })?;
            let read_count: usize = source.read_at(current_offset, &mut data[read_offset..])?;

            if read_count == 0 {
                break;
            }
            read_offset += read_count;
        }
        Ok(read_offset)
    }
}

impl DataSource for ExtentMapDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if offset >= self.size || buf.is_empty() {
            return Ok(0);
        }

        let mut written: usize = 0;
        let mut current_offset: u64 = offset;

        while written < buf.len() && current_offset < self.size {
            let Some(entry) = self.find_entry(current_offset) else {
                return Err(ErrorTrace::new(
                    "Missing extent for logical offset in extent map data source".to_string(),
                ));
            };
            let entry_relative_offset: u64 = current_offset - entry.logical_offset;
            let entry_available: usize = usize::try_from(entry.size - entry_relative_offset)
                .unwrap_or(usize::MAX)
                .min(buf.len() - written);
            let read_count: usize = match &entry.target {
                ExtentMapTarget::Data {
                    source,
                    source_offset,
                } => {
                    let physical_offset: u64 = source_offset
                        .checked_add(entry_relative_offset)
                        .ok_or_else(|| {
                            ErrorTrace::new("Extent map physical offset overflow".to_string())
                        })?;

                    Self::read_from_source(
                        source,
                        physical_offset,
                        &mut buf[written..written + entry_available],
                    )?
                }
                ExtentMapTarget::Zero => {
                    buf[written..written + entry_available].fill(0);
                    entry_available
                }
            };

            if read_count == 0 {
                break;
            }
            written += read_count;
            current_offset = current_offset
                .checked_add(read_count as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("Extent map data source read offset overflow".to_string())
                })?;
        }

        Ok(written)
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        Ok(self.size)
    }

    fn capabilities(&self) -> DataSourceCapabilities {
        self.capabilities
    }

    fn telemetry_name(&self) -> &'static str {
        "extent_map"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::source::MemoryDataSource;

    #[test]
    fn test_read_data_and_zero_extents() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"abcdef".to_vec()));
        let extent_map = ExtentMapDataSource::new(vec![
            ExtentMapEntry {
                logical_offset: 0,
                size: 2,
                target: ExtentMapTarget::Data {
                    source: source.clone(),
                    source_offset: 1,
                },
            },
            ExtentMapEntry {
                logical_offset: 2,
                size: 2,
                target: ExtentMapTarget::Zero,
            },
            ExtentMapEntry {
                logical_offset: 4,
                size: 2,
                target: ExtentMapTarget::Data {
                    source,
                    source_offset: 4,
                },
            },
        ])?;
        let mut data: Vec<u8> = vec![0xff; 6];

        let read_count: usize = extent_map.read_at(0, &mut data)?;

        assert_eq!(read_count, 6);
        assert_eq!(data, [0x62, 0x63, 0x00, 0x00, 0x65, 0x66]);
        Ok(())
    }

    #[test]
    fn test_rejects_non_contiguous_extents() {
        let result = ExtentMapDataSource::new(vec![ExtentMapEntry {
            logical_offset: 1,
            size: 2,
            target: ExtentMapTarget::Zero,
        }]);

        assert!(result.is_err());
    }
}
