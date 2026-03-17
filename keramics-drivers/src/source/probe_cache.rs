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

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use keramics_core::ErrorTrace;

use super::capabilities::DataSourceCapabilities;
use super::data_source::{DataSource, DataSourceReference};

const DEFAULT_PROBE_CACHE_WINDOW_SIZE: usize = 4096;
const DEFAULT_PROBE_CACHE_LIMIT: u64 = 64 * 1024;

/// Data source wrapper that caches small probe windows.
pub struct ProbeCachedDataSource {
    inner: DataSourceReference,
    window_size: usize,
    cache_limit: u64,
    windows: RwLock<HashMap<u64, Arc<[u8]>>>,
}

impl ProbeCachedDataSource {
    /// Creates a new probe cache wrapper with default settings.
    pub fn new(inner: DataSourceReference) -> Self {
        Self::with_options(
            inner,
            DEFAULT_PROBE_CACHE_WINDOW_SIZE,
            DEFAULT_PROBE_CACHE_LIMIT,
        )
    }

    /// Creates a new probe cache wrapper with custom settings.
    pub fn with_options(inner: DataSourceReference, window_size: usize, cache_limit: u64) -> Self {
        Self {
            inner,
            window_size: window_size.max(1),
            cache_limit,
            windows: RwLock::new(HashMap::new()),
        }
    }

    fn cacheable(&self, offset: u64, size: usize) -> bool {
        if size == 0 {
            return false;
        }
        match offset.checked_add(size as u64) {
            Some(end_offset) => end_offset <= self.cache_limit,
            None => false,
        }
    }

    fn read_window(&self, window_offset: u64) -> Result<Arc<[u8]>, ErrorTrace> {
        if let Some(window) = self.windows.read().unwrap().get(&window_offset).cloned() {
            return Ok(window);
        }

        let mut data: Vec<u8> = vec![0; self.window_size];
        let read_count: usize = self.inner.read_at(window_offset, &mut data)?;
        let window: Arc<[u8]> = data[..read_count].to_vec().into();

        let mut cache = self.windows.write().unwrap();
        let entry = cache.entry(window_offset).or_insert_with(|| window.clone());
        Ok(entry.clone())
    }
}

impl DataSource for ProbeCachedDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if !self.cacheable(offset, buf.len()) {
            return self.inner.read_at(offset, buf);
        }

        let mut written: usize = 0;

        while written < buf.len() {
            let absolute_offset: u64 = offset
                .checked_add(written as u64)
                .ok_or_else(|| ErrorTrace::new("Probe cache offset overflow".to_string()))?;
            let window_offset: u64 =
                (absolute_offset / self.window_size as u64) * self.window_size as u64;
            let window: Arc<[u8]> = self.read_window(window_offset)?;
            let window_inner_offset: usize = (absolute_offset - window_offset) as usize;

            if window_inner_offset >= window.len() {
                break;
            }

            let available: usize =
                std::cmp::min(window.len() - window_inner_offset, buf.len() - written);

            buf[written..written + available]
                .copy_from_slice(&window[window_inner_offset..window_inner_offset + available]);

            written += available;

            if window.len() < self.window_size {
                break;
            }
        }

        Ok(written)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::source::{DataSourceCapabilities, DataSourceSeekCost, MemoryDataSource};

    struct CountingDataSource {
        inner: MemoryDataSource,
        read_count: AtomicUsize,
    }

    impl CountingDataSource {
        fn new(data: Vec<u8>) -> Self {
            Self {
                inner: MemoryDataSource::new(data),
                read_count: AtomicUsize::new(0),
            }
        }

        fn read_count(&self) -> usize {
            self.read_count.load(Ordering::SeqCst)
        }
    }

    impl DataSource for CountingDataSource {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            self.inner.read_at(offset, buf)
        }

        fn size(&self) -> Result<u64, ErrorTrace> {
            self.inner.size()
        }

        fn capabilities(&self) -> DataSourceCapabilities {
            DataSourceCapabilities::concurrent(DataSourceSeekCost::Cheap)
        }
    }

    #[test]
    fn test_probe_cache_reuses_cached_window() -> Result<(), ErrorTrace> {
        let source = Arc::new(CountingDataSource::new(vec![0x41; 8192]));
        let cached = ProbeCachedDataSource::with_options(source.clone(), 4096, 8192);
        let mut data: Vec<u8> = vec![0; 16];

        cached.read_at(0, &mut data)?;
        cached.read_at(32, &mut data)?;

        assert_eq!(source.read_count(), 1);
        Ok(())
    }

    #[test]
    fn test_probe_cache_bypasses_large_offsets() -> Result<(), ErrorTrace> {
        let source = Arc::new(CountingDataSource::new(vec![0x41; 8192]));
        let cached = ProbeCachedDataSource::with_options(source.clone(), 4096, 4096);
        let mut data: Vec<u8> = vec![0; 16];

        cached.read_at(5000, &mut data)?;
        cached.read_at(5000, &mut data)?;

        assert_eq!(source.read_count(), 2);
        Ok(())
    }
}
