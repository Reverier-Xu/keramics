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

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use keramics_core::ErrorTrace;

use super::source_resolver::{SourceResolver, SourceResolverReference};
use crate::source::{DataSourceReference, open_local_data_source};

/// Source resolver rooted at a local directory.
pub struct LocalSourceResolver {
    root_path: PathBuf,
}

impl LocalSourceResolver {
    /// Creates a new local source resolver.
    pub fn new(root_path: &Path) -> Result<Self, ErrorTrace> {
        if !root_path.is_dir() {
            return Err(ErrorTrace::new(format!(
                "Local source resolver root is not a directory: {}",
                root_path.display(),
            )));
        }

        Ok(Self {
            root_path: root_path.to_path_buf(),
        })
    }

    fn sanitize_relative_path(relative_path: &Path) -> Result<PathBuf, ErrorTrace> {
        let mut sanitized_path = PathBuf::new();

        for component in relative_path.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(component) => sanitized_path.push(component),
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ErrorTrace::new(
                        "Local source resolver requires a safe relative path".to_string(),
                    ));
                }
            }
        }

        if sanitized_path.as_os_str().is_empty() {
            return Err(ErrorTrace::new(
                "Local source resolver requires a non-empty relative path".to_string(),
            ));
        }

        Ok(sanitized_path)
    }
}

impl SourceResolver for LocalSourceResolver {
    fn open_source(&self, relative_path: &Path) -> Result<Option<DataSourceReference>, ErrorTrace> {
        let sanitized_path = Self::sanitize_relative_path(relative_path)?;
        let path = self.root_path.join(sanitized_path);

        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(open_local_data_source(&path)?))
    }
}

/// Opens a shared local source resolver.
pub fn open_local_source_resolver(root_path: &Path) -> Result<SourceResolverReference, ErrorTrace> {
    Ok(Arc::new(LocalSourceResolver::new(root_path)?))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::tests::get_test_data_path;

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let root_path = PathBuf::from(get_test_data_path("directory"));
        let resolver = LocalSourceResolver::new(&root_path)?;
        let source = resolver.open_source(Path::new("file.txt"))?;

        assert_eq!(source.expect("source").size()?, 202);
        Ok(())
    }

    #[test]
    fn test_rejects_parent_dir_escape() -> Result<(), ErrorTrace> {
        let root_path = PathBuf::from(get_test_data_path("directory"));
        let resolver = LocalSourceResolver::new(&root_path)?;

        let result = resolver.open_source(Path::new("../file.txt"));

        assert!(result.is_err());
        Ok(())
    }
}
