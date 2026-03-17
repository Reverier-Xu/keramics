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
use std::path::Path;

use keramics_core::ErrorTrace;

use super::capabilities::DataSourceCapabilities;
use super::data_source::{DataSource, DataSourceReference};

/// Contiguous subrange view over a parent data source.
pub struct SliceDataSource {
    inner: DataSourceReference,
    base_offset: u64,
    size: u64,
}

impl SliceDataSource {
    /// Creates a new slice data source.
    pub fn new(inner: DataSourceReference, base_offset: u64, size: u64) -> Self {
        Self {
            inner,
            base_offset,
            size,
        }
    }
}

impl DataSource for SliceDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if offset >= self.size || buf.is_empty() {
            return Ok(0);
        }

        let available: usize = min(
            buf.len(),
            usize::try_from(self.size - offset).unwrap_or(usize::MAX),
        );
        let absolute_offset: u64 = self
            .base_offset
            .checked_add(offset)
            .ok_or_else(|| ErrorTrace::new("Slice data source offset overflow".to_string()))?;

        self.inner.read_at(absolute_offset, &mut buf[..available])
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        Ok(self.size)
    }

    fn capabilities(&self) -> DataSourceCapabilities {
        self.inner.capabilities()
    }

    fn telemetry_name(&self) -> &'static str {
        self.inner.telemetry_name()
    }

    fn origin_path(&self) -> Option<&Path> {
        self.inner.origin_path()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::source::MemoryDataSource;

    #[test]
    fn test_read_at() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"abcdefgh".to_vec()));
        let slice = SliceDataSource::new(source, 2, 4);
        let mut data: Vec<u8> = vec![0; 4];

        let read_count: usize = slice.read_at(0, &mut data)?;

        assert_eq!(read_count, 4);
        assert_eq!(data, b"cdef");
        Ok(())
    }

    #[test]
    fn test_read_at_clamps_to_slice_size() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"abcdefgh".to_vec()));
        let slice = SliceDataSource::new(source, 2, 4);
        let mut data: Vec<u8> = vec![0; 4];

        let read_count: usize = slice.read_at(3, &mut data)?;

        assert_eq!(read_count, 1);
        assert_eq!(&data[..1], b"f");
        Ok(())
    }
}
