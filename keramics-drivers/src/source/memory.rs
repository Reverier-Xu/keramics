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

use std::cmp::min;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use super::capabilities::{DataSourceCapabilities, DataSourceSeekCost};
use super::data_source::DataSource;

/// In-memory immutable data source.
#[derive(Clone, Debug)]
pub struct MemoryDataSource {
    data: Arc<[u8]>,
}

impl MemoryDataSource {
    /// Creates a new in-memory data source.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data: data.into() }
    }

    /// Creates a new in-memory data source from a shared byte slice.
    pub fn from_bytes(data: Arc<[u8]>) -> Self {
        Self { data }
    }
}

impl DataSource for MemoryDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        let offset: usize = match usize::try_from(offset) {
            Ok(offset) => offset,
            Err(_) => return Ok(0),
        };

        if offset >= self.data.len() || buf.is_empty() {
            return Ok(0);
        }

        let read_count: usize = min(buf.len(), self.data.len() - offset);

        buf[..read_count].copy_from_slice(&self.data[offset..offset + read_count]);
        Ok(read_count)
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        Ok(self.data.len() as u64)
    }

    fn capabilities(&self) -> DataSourceCapabilities {
        DataSourceCapabilities::concurrent(DataSourceSeekCost::Cheap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_at() -> Result<(), ErrorTrace> {
        let source = MemoryDataSource::new(b"abcdef".to_vec());
        let mut data: Vec<u8> = vec![0; 3];

        let read_count: usize = source.read_at(2, &mut data)?;

        assert_eq!(read_count, 3);
        assert_eq!(data, b"cde");
        Ok(())
    }

    #[test]
    fn test_read_all() -> Result<(), ErrorTrace> {
        let source = MemoryDataSource::new(b"abcdef".to_vec());

        assert_eq!(source.read_all()?, b"abcdef");
        Ok(())
    }
}
