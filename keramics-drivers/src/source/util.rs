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

use super::capabilities::{DataSourceCapabilities, DataSourceReadConcurrency, DataSourceSeekCost};

pub(super) fn combine_data_source_capabilities<I>(capabilities_iter: I) -> DataSourceCapabilities
where
    I: IntoIterator<Item = DataSourceCapabilities>,
{
    let mut capabilities_iter = capabilities_iter.into_iter();
    let Some(mut capabilities) = capabilities_iter.next() else {
        return DataSourceCapabilities::default();
    };

    for next_capabilities in capabilities_iter {
        capabilities.read_concurrency = match (
            capabilities.read_concurrency,
            next_capabilities.read_concurrency,
        ) {
            (DataSourceReadConcurrency::Serialized, _)
            | (_, DataSourceReadConcurrency::Serialized) => DataSourceReadConcurrency::Serialized,
            (DataSourceReadConcurrency::Unknown, _) | (_, DataSourceReadConcurrency::Unknown) => {
                DataSourceReadConcurrency::Unknown
            }
            (DataSourceReadConcurrency::Concurrent, DataSourceReadConcurrency::Concurrent) => {
                DataSourceReadConcurrency::Concurrent
            }
        };
        capabilities.seek_cost = match (capabilities.seek_cost, next_capabilities.seek_cost) {
            (DataSourceSeekCost::Expensive, _) | (_, DataSourceSeekCost::Expensive) => {
                DataSourceSeekCost::Expensive
            }
            (DataSourceSeekCost::Unknown, _) | (_, DataSourceSeekCost::Unknown) => {
                DataSourceSeekCost::Unknown
            }
            (DataSourceSeekCost::Cheap, DataSourceSeekCost::Cheap) => DataSourceSeekCost::Cheap,
        };
        capabilities.preferred_chunk_size = match (
            capabilities.preferred_chunk_size,
            next_capabilities.preferred_chunk_size,
        ) {
            (Some(preferred_chunk_size), Some(next_preferred_chunk_size))
                if preferred_chunk_size == next_preferred_chunk_size =>
            {
                Some(preferred_chunk_size)
            }
            (None, preferred_chunk_size) | (preferred_chunk_size, None) => preferred_chunk_size,
            _ => None,
        };
    }
    capabilities
}
