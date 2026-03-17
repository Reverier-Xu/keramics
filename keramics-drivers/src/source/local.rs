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

use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[cfg(not(any(unix, windows)))]
use std::io::{Read, Seek, SeekFrom};
#[cfg(not(any(unix, windows)))]
use std::sync::Mutex;

use keramics_core::ErrorTrace;

use super::capabilities::{DataSourceCapabilities, DataSourceSeekCost};
use super::data_source::{DataSource, DataSourceReference};

#[cfg(any(unix, windows))]
struct LocalReadHandle {
    file: File,
}

#[cfg(any(unix, windows))]
impl LocalReadHandle {
    fn new(file: File) -> Self {
        Self { file }
    }
}

#[cfg(unix)]
impl LocalReadHandle {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        use std::os::unix::fs::FileExt as _;

        self.file.read_at(buf, offset).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to read data from local file at offset {} with error: {}",
                offset, error,
            ))
        })
    }
}

#[cfg(windows)]
impl LocalReadHandle {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        use std::os::windows::fs::FileExt as _;

        self.file.seek_read(buf, offset).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to read data from local file at offset {} with error: {}",
                offset, error,
            ))
        })
    }
}

#[cfg(not(any(unix, windows)))]
struct LocalReadHandle {
    file: Mutex<File>,
}

#[cfg(not(any(unix, windows)))]
impl LocalReadHandle {
    fn new(file: File) -> Self {
        Self {
            file: Mutex::new(file),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| ErrorTrace::new("Unable to acquire local file lock".to_string()))?;

        file.seek(SeekFrom::Start(offset)).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to seek local file to offset {} with error: {}",
                offset, error,
            ))
        })?;

        file.read(buf).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to read local file data with error: {}",
                error,
            ))
        })
    }
}

/// Local file data source backed by operating system positioned reads.
pub struct LocalDataSource {
    reader: LocalReadHandle,
    path: PathBuf,
    size: u64,
}

impl LocalDataSource {
    /// Opens a local file as a data source.
    pub fn open(path: &Path) -> Result<Self, ErrorTrace> {
        let file: File = fs::File::open(path).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to open local file: {} with error: {}",
                path.display(),
                error,
            ))
        })?;
        let size: u64 = file
            .metadata()
            .map_err(|error| {
                ErrorTrace::new(format!(
                    "Unable to retrieve local file metadata: {} with error: {}",
                    path.display(),
                    error,
                ))
            })?
            .len();

        Ok(Self {
            reader: LocalReadHandle::new(file),
            path: path.to_path_buf(),
            size,
        })
    }
}

impl DataSource for LocalDataSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        self.reader.read_at(offset, buf)
    }

    fn size(&self) -> Result<u64, ErrorTrace> {
        Ok(self.size)
    }

    fn capabilities(&self) -> DataSourceCapabilities {
        #[cfg(any(unix, windows))]
        {
            DataSourceCapabilities::concurrent(DataSourceSeekCost::Cheap)
        }

        #[cfg(not(any(unix, windows)))]
        {
            DataSourceCapabilities::serialized(DataSourceSeekCost::Cheap)
        }
    }

    fn telemetry_name(&self) -> &'static str {
        "local"
    }

    fn origin_path(&self) -> Option<&Path> {
        Some(&self.path)
    }
}

/// Opens a local file and returns it as a shared data source.
pub fn open_local_data_source(path: &Path) -> Result<DataSourceReference, ErrorTrace> {
    Ok(std::sync::Arc::new(LocalDataSource::open(path)?))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::tests::get_test_data_path;

    #[test]
    fn test_open_local_data_source() -> Result<(), ErrorTrace> {
        let path: PathBuf = PathBuf::from(get_test_data_path("directory/file.txt"));
        let source = open_local_data_source(&path)?;

        assert_eq!(source.size()?, 202);
        Ok(())
    }

    #[test]
    fn test_read_at() -> Result<(), ErrorTrace> {
        let path: PathBuf = PathBuf::from(get_test_data_path("directory/file.txt"));
        let source = LocalDataSource::open(&path)?;
        let mut data: Vec<u8> = vec![0; 8];

        let read_count: usize = source.read_at(0, &mut data)?;

        assert_eq!(read_count, 8);
        assert_eq!(data, b"A cerami");
        Ok(())
    }
}
