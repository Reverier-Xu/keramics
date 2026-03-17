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

use keramics_core::ErrorTrace;

use super::capabilities::DataSourceCapabilities;
use super::data_source::{DataSource, DataSourceReference};
use super::util::combine_data_source_capabilities;

/// Single logical segment within a segmented data source.
#[derive(Clone)]
pub struct SegmentedSourceSegment {
    /// Logical start offset of the segment.
    pub logical_offset: u64,

    /// Size of the segment in bytes.
    pub size: u64,

    /// Underlying source that stores the segment bytes.
    pub source: DataSourceReference,

    /// Physical start offset within the underlying source.
    pub source_offset: u64,
}

/// Concatenated logical source backed by multiple segments.
pub struct SegmentedDataSource {
    segments: Vec<SegmentedSourceSegment>,
    size: u64,
    capabilities: DataSourceCapabilities,
}

impl SegmentedDataSource {
    /// Creates a new segmented data source.
    pub fn new(segments: Vec<SegmentedSourceSegment>) -> Result<Self, ErrorTrace> {
        let mut expected_logical_offset: u64 = 0;

        for segment in segments.iter() {
            if segment.size == 0 {
                return Err(ErrorTrace::new(
                    "Segmented data source cannot contain zero-sized segments".to_string(),
                ));
            }
            if segment.logical_offset != expected_logical_offset {
                return Err(ErrorTrace::new(
                    "Segmented data source must use contiguous logical offsets".to_string(),
                ));
            }
            expected_logical_offset = expected_logical_offset
                .checked_add(segment.size)
                .ok_or_else(|| {
                    ErrorTrace::new("Segmented data source size overflow".to_string())
                })?;
        }

        let capabilities = combine_data_source_capabilities(
            segments.iter().map(|segment| segment.source.capabilities()),
        );

        Ok(Self {
            segments,
            size: expected_logical_offset,
            capabilities,
        })
    }

    fn find_segment(&self, offset: u64) -> Option<&SegmentedSourceSegment> {
        let segment_index: usize = self
            .segments
            .partition_point(|segment| segment.logical_offset <= offset);

        if segment_index == 0 {
            None
        } else {
            self.segments.get(segment_index - 1)
        }
    }

    fn read_from_segment(
        segment: &SegmentedSourceSegment,
        segment_offset: u64,
        data: &mut [u8],
    ) -> Result<usize, ErrorTrace> {
        let mut read_offset: usize = 0;

        while read_offset < data.len() {
            let physical_offset: u64 = segment
                .source_offset
                .checked_add(segment_offset)
                .and_then(|offset| offset.checked_add(read_offset as u64))
                .ok_or_else(|| {
                    ErrorTrace::new("Segmented data source offset overflow".to_string())
                })?;
            let read_count: usize = segment
                .source
                .read_at(physical_offset, &mut data[read_offset..])?;

            if read_count == 0 {
                break;
            }
            read_offset += read_count;
        }
        Ok(read_offset)
    }
}

impl DataSource for SegmentedDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if offset >= self.size || buf.is_empty() {
            return Ok(0);
        }

        let mut written: usize = 0;
        let mut current_offset: u64 = offset;

        while written < buf.len() && current_offset < self.size {
            let Some(segment) = self.find_segment(current_offset) else {
                return Err(ErrorTrace::new(
                    "Missing segment for logical offset in segmented data source".to_string(),
                ));
            };
            let segment_relative_offset: u64 = current_offset - segment.logical_offset;
            let segment_available: usize = usize::try_from(segment.size - segment_relative_offset)
                .unwrap_or(usize::MAX)
                .min(buf.len() - written);
            let read_count: usize = Self::read_from_segment(
                segment,
                segment_relative_offset,
                &mut buf[written..written + segment_available],
            )?;

            if read_count == 0 {
                break;
            }
            written += read_count;
            current_offset = current_offset
                .checked_add(read_count as u64)
                .ok_or_else(|| {
                    ErrorTrace::new("Segmented data source read offset overflow".to_string())
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
        "segmented"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::source::MemoryDataSource;

    #[test]
    fn test_read_across_segments() -> Result<(), ErrorTrace> {
        let segment_1: DataSourceReference = Arc::new(MemoryDataSource::new(b"abc".to_vec()));
        let segment_2: DataSourceReference = Arc::new(MemoryDataSource::new(b"defg".to_vec()));
        let source = SegmentedDataSource::new(vec![
            SegmentedSourceSegment {
                logical_offset: 0,
                size: 3,
                source: segment_1,
                source_offset: 0,
            },
            SegmentedSourceSegment {
                logical_offset: 3,
                size: 4,
                source: segment_2,
                source_offset: 0,
            },
        ])?;
        let mut data: Vec<u8> = vec![0; 4];

        let read_count: usize = source.read_at(2, &mut data)?;

        assert_eq!(read_count, 4);
        assert_eq!(data, b"cdef");
        Ok(())
    }

    #[test]
    fn test_rejects_non_contiguous_segments() {
        let segment: DataSourceReference = Arc::new(MemoryDataSource::new(b"abc".to_vec()));
        let result = SegmentedDataSource::new(vec![SegmentedSourceSegment {
            logical_offset: 1,
            size: 3,
            source: segment,
            source_offset: 0,
        }]);

        assert!(result.is_err());
    }
}
