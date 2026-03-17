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

use std::path::Path;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::source::DataSourceReference;

/// Shared source resolver reference.
pub type SourceResolverReference = Arc<dyn SourceResolver>;

/// Resolver for sibling or related data sources.
pub trait SourceResolver: Send + Sync {
    /// Opens a source by logical relative path.
    fn open_source(&self, relative_path: &Path) -> Result<Option<DataSourceReference>, ErrorTrace>;
}
