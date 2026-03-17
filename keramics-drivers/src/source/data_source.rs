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

use std::path::Path;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use super::capabilities::DataSourceCapabilities;

/// Shared data source reference.
pub type DataSourceReference = Arc<dyn DataSource>;

/// Immutable positioned-read byte source.
pub trait DataSource: Send + Sync {
    /// Reads data starting at the specified offset.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace>;

    /// Retrieves the total size of the source.
    fn size(&self) -> Result<u64, ErrorTrace>;

    /// Retrieves the current backend capabilities.
    fn capabilities(&self) -> DataSourceCapabilities {
        DataSourceCapabilities::default()
    }

    /// Retrieves a telemetry backend name.
    fn telemetry_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Retrieves the host path, when available.
    fn origin_path(&self) -> Option<&Path> {
        None
    }

    /// Reads an exact amount of data starting at the specified offset.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ErrorTrace> {
        let mut read_offset: usize = 0;

        while read_offset < buf.len() {
            let current_offset: u64 = offset
                .checked_add(read_offset as u64)
                .ok_or_else(|| ErrorTrace::new("Data source offset overflow".to_string()))?;
            let read_count: usize = self.read_at(current_offset, &mut buf[read_offset..])?;

            if read_count == 0 {
                return Err(ErrorTrace::new(
                    "Unable to read exact amount from data source".to_string(),
                ));
            }
            read_offset += read_count;
        }
        Ok(())
    }

    /// Reads the entire source into memory.
    fn read_all(&self) -> Result<Vec<u8>, ErrorTrace> {
        let size: u64 = self.size()?;
        let capacity: usize = usize::try_from(size).map_err(|_| {
            ErrorTrace::new("Data source is too large to read into memory".to_string())
        })?;
        let mut data: Vec<u8> = vec![0; capacity];
        let mut read_offset: usize = 0;

        while read_offset < data.len() {
            let read_count: usize = self.read_at(read_offset as u64, &mut data[read_offset..])?;

            if read_count == 0 {
                break;
            }
            read_offset += read_count;
        }
        data.truncate(read_offset);
        Ok(data)
    }
}
