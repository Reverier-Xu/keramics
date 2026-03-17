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

pub mod image;
pub mod resolver;
pub mod source;
pub mod volume;

pub use image::{
    EwfImage, EwfMediaType, SparseBundleImage, SparseImageFile, SplitRawImage, VhdDiskType, VhdFile,
};
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
pub use volume::{ApmPartition, ApmVolumeSystem};
pub use volume::{GptPartition, GptVolumeSystem, MbrPartition, MbrVolumeSystem};

#[cfg(test)]
mod tests {
    use keramics_core::ErrorTrace;
    use keramics_core::formatters::format_as_string;
    use keramics_hashes::{DigestHashContext, Md5Context};

    use crate::source::{DataSourceCursor, DataSourceReference};

    pub fn get_test_data_path(path: &str) -> String {
        format!("../test_data/{}", path)
    }

    pub fn read_data_source_md5(source: DataSourceReference) -> Result<(u64, String), ErrorTrace> {
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
}
