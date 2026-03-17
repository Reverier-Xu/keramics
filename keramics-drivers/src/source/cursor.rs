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

use std::io::SeekFrom;

use keramics_core::ErrorTrace;

use super::data_source::DataSourceReference;

/// Local sequential adapter over a shared immutable data source.
#[derive(Clone)]
pub struct DataSourceCursor {
    source: DataSourceReference,
    position: u64,
}

impl DataSourceCursor {
    /// Creates a new cursor positioned at the start of the source.
    pub fn new(source: DataSourceReference) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    /// Creates a new cursor at the specified position.
    pub fn with_position(source: DataSourceReference, position: u64) -> Self {
        Self { source, position }
    }

    /// Creates a forked cursor at the current position.
    pub fn fork(&self) -> Self {
        Self {
            source: self.source.clone(),
            position: self.position,
        }
    }

    /// Creates a forked cursor at the specified position.
    pub fn fork_at(&self, position: u64) -> Self {
        Self {
            source: self.source.clone(),
            position,
        }
    }

    /// Retrieves the current position.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Reads data at the current cursor position.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        let read_count: usize = self.source.read_at(self.position, buf)?;

        self.position = self
            .position
            .checked_add(read_count as u64)
            .ok_or_else(|| ErrorTrace::new("Data source cursor position overflow".to_string()))?;

        Ok(read_count)
    }

    /// Reads an exact amount of data at the current cursor position.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ErrorTrace> {
        self.source.read_exact_at(self.position, buf)?;

        self.position = self
            .position
            .checked_add(buf.len() as u64)
            .ok_or_else(|| ErrorTrace::new("Data source cursor position overflow".to_string()))?;

        Ok(())
    }

    /// Sets the current cursor position.
    pub fn seek(&mut self, position: SeekFrom) -> Result<u64, ErrorTrace> {
        let size: u64 = self.source.size()?;
        let new_position: i128 = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(size) + i128::from(offset),
        };

        if new_position < 0 || new_position > i128::from(u64::MAX) {
            return Err(ErrorTrace::new(
                "Invalid data source cursor seek position".to_string(),
            ));
        }

        self.position = new_position as u64;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::source::MemoryDataSource;

    #[test]
    fn test_read_advances_position() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"hello world".to_vec()));
        let mut cursor = DataSourceCursor::new(source);
        let mut data: Vec<u8> = vec![0; 5];

        let read_count: usize = cursor.read(&mut data)?;

        assert_eq!(read_count, 5);
        assert_eq!(data, b"hello");
        assert_eq!(cursor.position(), 5);
        Ok(())
    }

    #[test]
    fn test_seek_and_read_exact() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"hello world".to_vec()));
        let mut cursor = DataSourceCursor::new(source);
        let mut data: Vec<u8> = vec![0; 5];

        let offset: u64 = cursor.seek(SeekFrom::Start(6))?;
        assert_eq!(offset, 6);

        cursor.read_exact(&mut data)?;

        assert_eq!(data, b"world");
        assert_eq!(cursor.position(), 11);
        Ok(())
    }

    #[test]
    fn test_fork_keeps_independent_position() -> Result<(), ErrorTrace> {
        let source: DataSourceReference = Arc::new(MemoryDataSource::new(b"abcdef".to_vec()));
        let mut cursor_a = DataSourceCursor::new(source);
        let mut cursor_b = cursor_a.fork();
        let mut data_a: Vec<u8> = vec![0; 3];
        let mut data_b: Vec<u8> = vec![0; 2];

        cursor_a.read_exact(&mut data_a)?;
        cursor_b.seek(SeekFrom::Start(3))?;
        cursor_b.read_exact(&mut data_b)?;

        assert_eq!(data_a, b"abc");
        assert_eq!(data_b, b"de");
        assert_eq!(cursor_a.position(), 3);
        assert_eq!(cursor_b.position(), 5);
        Ok(())
    }
}
