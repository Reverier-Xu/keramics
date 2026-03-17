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

use std::sync::Arc;

use crate::source::{DataSourceReference, SliceDataSource};

/// Immutable Master Boot Record partition metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MbrPartition {
    entry_index: usize,
    offset: u64,
    size: u64,
    partition_type: u8,
    flags: u8,
}

impl MbrPartition {
    pub(crate) fn new(
        entry_index: usize,
        offset: u64,
        size: u64,
        partition_type: u8,
        flags: u8,
    ) -> Self {
        Self {
            entry_index,
            offset,
            size,
            partition_type,
            flags,
        }
    }

    /// Retrieves the partition table entry index.
    pub fn entry_index(&self) -> usize {
        self.entry_index
    }

    /// Retrieves the partition offset relative to the start of the image.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Retrieves the partition size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Retrieves the raw partition type value.
    pub fn partition_type(&self) -> u8 {
        self.partition_type
    }

    /// Retrieves the raw partition flags.
    pub fn flags(&self) -> u8 {
        self.flags
    }

    /// Indicates whether the partition is marked bootable.
    pub fn is_bootable(&self) -> bool {
        (self.flags & 0x80) == 0x80
    }

    /// Opens the partition as an immutable slice data source.
    pub fn open_source(&self, source: DataSourceReference) -> DataSourceReference {
        Arc::new(SliceDataSource::new(source, self.offset, self.size))
    }
}
