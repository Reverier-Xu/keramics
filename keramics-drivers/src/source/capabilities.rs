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

/// Describes how a data source behaves under parallel reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataSourceReadConcurrency {
    /// The implementation cannot describe its current behavior yet.
    Unknown,

    /// Reads are serialized by the implementation.
    Serialized,

    /// Reads can proceed independently at different offsets.
    Concurrent,
}

/// Describes the cost of issuing positioned reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataSourceSeekCost {
    /// The implementation cannot describe the current behavior yet.
    Unknown,

    /// Positioned reads are cheap relative to the surrounding workload.
    Cheap,

    /// Positioned reads are more expensive and should influence scheduling.
    Expensive,
}

/// Describes the current backend characteristics of a data source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSourceCapabilities {
    /// Whether this implementation can serve concurrent reads efficiently.
    pub read_concurrency: DataSourceReadConcurrency,

    /// The current positioned-read cost profile.
    pub seek_cost: DataSourceSeekCost,

    /// The preferred logical chunk size for higher-level callers.
    pub preferred_chunk_size: Option<usize>,
}

impl DataSourceCapabilities {
    /// Creates a new set of capabilities.
    pub const fn new(
        read_concurrency: DataSourceReadConcurrency,
        seek_cost: DataSourceSeekCost,
    ) -> Self {
        Self {
            read_concurrency,
            seek_cost,
            preferred_chunk_size: None,
        }
    }

    /// Creates a new set of capabilities for a serialized data source.
    pub const fn serialized(seek_cost: DataSourceSeekCost) -> Self {
        Self::new(DataSourceReadConcurrency::Serialized, seek_cost)
    }

    /// Creates a new set of capabilities for a concurrent data source.
    pub const fn concurrent(seek_cost: DataSourceSeekCost) -> Self {
        Self::new(DataSourceReadConcurrency::Concurrent, seek_cost)
    }

    /// Sets the preferred logical chunk size.
    pub fn with_preferred_chunk_size(mut self, preferred_chunk_size: usize) -> Self {
        self.preferred_chunk_size = Some(preferred_chunk_size);
        self
    }
}

impl Default for DataSourceCapabilities {
    fn default() -> Self {
        Self::new(
            DataSourceReadConcurrency::Unknown,
            DataSourceSeekCost::Unknown,
        )
    }
}
