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
use std::sync::{Arc, Mutex};
use std::time::Instant;

use keramics_core::ErrorTrace;

use super::capabilities::DataSourceCapabilities;
use super::data_source::{DataSource, DataSourceReference};

/// Mutable statistics collector for observed data sources.
#[derive(Clone, Debug, Default)]
pub struct DataSourceReadStats {
    inner: Arc<Mutex<DataSourceReadStatsState>>,
}

/// Read statistics snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataSourceReadStatsSnapshot {
    /// Number of read operations.
    pub read_count: u64,

    /// Total number of bytes read.
    pub read_bytes: u64,

    /// Average read size.
    pub average_read_size: u64,

    /// Total distance between read requests.
    pub request_offset_distance_bytes: u64,

    /// Average distance between read requests.
    pub average_offset_distance_bytes: u64,

    /// Maximum read size.
    pub max_read_size: usize,

    /// Maximum read distance.
    pub max_offset_distance_bytes: u64,

    /// Total read time in microseconds.
    pub total_read_micros: u128,

    /// Average read time in microseconds.
    pub average_read_micros: u128,
}

#[derive(Debug, Default)]
struct DataSourceReadStatsState {
    read_count: u64,
    read_bytes: u64,
    request_offset_distance_bytes: u64,
    max_read_size: usize,
    max_offset_distance_bytes: u64,
    total_read_micros: u128,
    last_offset: Option<u64>,
    last_len: usize,
}

impl DataSourceReadStats {
    fn record_read(&self, offset: u64, len: usize, started_at: Instant) {
        let mut state = self.inner.lock().unwrap();

        state.read_count = state.read_count.saturating_add(1);
        state.read_bytes = state.read_bytes.saturating_add(len as u64);
        state.max_read_size = state.max_read_size.max(len);
        state.total_read_micros = state
            .total_read_micros
            .saturating_add(started_at.elapsed().as_micros());

        if let Some(last_offset) = state.last_offset {
            let last_end: u64 = last_offset.saturating_add(state.last_len as u64);
            let distance: u64 = offset.abs_diff(last_end);

            state.request_offset_distance_bytes =
                state.request_offset_distance_bytes.saturating_add(distance);
            state.max_offset_distance_bytes = state.max_offset_distance_bytes.max(distance);
        }

        state.last_offset = Some(offset);
        state.last_len = len;
    }

    /// Captures the current statistics snapshot.
    pub fn snapshot(&self) -> DataSourceReadStatsSnapshot {
        let state = self.inner.lock().unwrap();
        let average_read_size: u64 = if state.read_count == 0 {
            0
        } else {
            state.read_bytes / state.read_count
        };
        let average_offset_distance_bytes: u64 = if state.read_count <= 1 {
            0
        } else {
            state.request_offset_distance_bytes / (state.read_count - 1)
        };
        let average_read_micros: u128 = if state.read_count == 0 {
            0
        } else {
            state.total_read_micros / u128::from(state.read_count)
        };

        DataSourceReadStatsSnapshot {
            read_count: state.read_count,
            read_bytes: state.read_bytes,
            average_read_size,
            request_offset_distance_bytes: state.request_offset_distance_bytes,
            average_offset_distance_bytes,
            max_read_size: state.max_read_size,
            max_offset_distance_bytes: state.max_offset_distance_bytes,
            total_read_micros: state.total_read_micros,
            average_read_micros,
        }
    }
}

/// Data source wrapper that records read behavior metrics.
pub struct ObservedDataSource {
    inner: DataSourceReference,
    stats: DataSourceReadStats,
}

impl ObservedDataSource {
    /// Creates a new observed data source.
    pub fn new(inner: DataSourceReference) -> Self {
        Self {
            inner,
            stats: DataSourceReadStats::default(),
        }
    }

    /// Retrieves the shared statistics collector.
    pub fn stats(&self) -> DataSourceReadStats {
        self.stats.clone()
    }
}

impl DataSource for ObservedDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        let started_at = Instant::now();
        let read_count: usize = self.inner.read_at(offset, buf)?;

        self.stats.record_read(offset, read_count, started_at);
        Ok(read_count)
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        self.inner.size()
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
    fn test_stats_record_reads() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"abcdef".to_vec()));
        let observed = ObservedDataSource::new(source);
        let mut data: Vec<u8> = vec![0; 2];

        observed.read_at(0, &mut data)?;
        observed.read_at(3, &mut data)?;

        let snapshot: DataSourceReadStatsSnapshot = observed.stats().snapshot();

        assert_eq!(snapshot.read_count, 2);
        assert_eq!(snapshot.read_bytes, 4);
        assert_eq!(snapshot.max_read_size, 2);
        assert_eq!(snapshot.request_offset_distance_bytes, 1);
        Ok(())
    }
}
