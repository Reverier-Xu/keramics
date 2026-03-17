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

use keramics_types::Uuid;

use crate::source::{DataSourceReference, SliceDataSource};

/// Immutable GUID Partition Table partition metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GptPartition {
    entry_index: usize,
    offset: u64,
    size: u64,
    type_identifier: Uuid,
    identifier: Uuid,
    attribute_flags: u64,
    name: String,
}

impl GptPartition {
    pub(crate) fn new(
        entry_index: usize,
        offset: u64,
        size: u64,
        type_identifier: Uuid,
        identifier: Uuid,
        attribute_flags: u64,
        name: String,
    ) -> Self {
        Self {
            entry_index,
            offset,
            size,
            type_identifier,
            identifier,
            attribute_flags,
            name,
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

    /// Retrieves the partition type identifier.
    pub fn type_identifier(&self) -> &Uuid {
        &self.type_identifier
    }

    /// Retrieves the partition identifier.
    pub fn identifier(&self) -> &Uuid {
        &self.identifier
    }

    /// Retrieves the partition attribute flags.
    pub fn attribute_flags(&self) -> u64 {
        self.attribute_flags
    }

    /// Retrieves the partition name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Opens the partition as an immutable slice data source.
    pub fn open_source(&self, source: DataSourceReference) -> DataSourceReference {
        Arc::new(SliceDataSource::new(source, self.offset, self.size))
    }
}
