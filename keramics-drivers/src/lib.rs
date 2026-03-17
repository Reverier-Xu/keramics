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

pub mod resolver;
pub mod source;
pub mod volume;

pub use resolver::{
    LocalSourceResolver, SourceResolver, SourceResolverReference, open_local_source_resolver,
};
pub use source::{
    DataSource, DataSourceCapabilities, DataSourceCursor, DataSourceReadConcurrency,
    DataSourceReadStats, DataSourceReadStatsSnapshot, DataSourceReference, DataSourceSeekCost,
    ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget, LocalDataSource, MemoryDataSource,
    ObservedDataSource, ProbeCachedDataSource, SegmentedDataSource, SegmentedSourceSegment,
    SliceDataSource, open_local_data_source,
};
pub use volume::{MbrPartition, MbrVolumeSystem};

#[cfg(test)]
mod tests {
    pub fn get_test_data_path(path: &str) -> String {
        format!("../test_data/{}", path)
    }
}
