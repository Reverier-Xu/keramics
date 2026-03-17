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

mod capabilities;
mod cursor;
mod data_source;
mod extent_map;
mod local;
mod memory;
mod observed;
mod probe_cache;
mod segmented;
mod slice;
mod util;

pub use capabilities::{DataSourceCapabilities, DataSourceReadConcurrency, DataSourceSeekCost};
pub use cursor::DataSourceCursor;
pub use data_source::{DataSource, DataSourceReference};
pub use extent_map::{ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};
pub use local::{LocalDataSource, open_local_data_source};
pub use memory::MemoryDataSource;
pub use observed::{DataSourceReadStats, DataSourceReadStatsSnapshot, ObservedDataSource};
pub use probe_cache::ProbeCachedDataSource;
pub use segmented::{SegmentedDataSource, SegmentedSourceSegment};
pub use slice::SliceDataSource;
