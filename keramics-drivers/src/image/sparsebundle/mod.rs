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

use std::path::Path;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::resolver::SourceResolverReference;
use crate::source::{DataSourceReference, ExtentMapDataSource, ExtentMapEntry, ExtentMapTarget};

const INFO_PLIST_FILE_NAMES: [&str; 2] = ["Info.plist", "Info.bckup"];

#[derive(Clone)]
struct SparseBundleBand {
    file_name: String,
    logical_offset: u64,
    logical_size: u64,
    source: Option<DataSourceReference>,
    source_size: u64,
}

/// Immutable sparsebundle metadata plus opened band sources.
pub struct SparseBundleImage {
    band_size: u32,
    media_size: u64,
    bands: Vec<SparseBundleBand>,
}

impl SparseBundleImage {
    /// Opens a sparsebundle image from a resolver rooted at the bundle directory.
    pub fn open(resolver: &SourceResolverReference) -> Result<Self, ErrorTrace> {
        let info_plist = Self::read_info_plist(resolver)?;
        let band_count = info_plist.media_size.div_ceil(info_plist.band_size as u64);
        let mut bands = Vec::with_capacity(band_count as usize);

        for band_index in 0..band_count {
            let logical_offset = band_index * (info_plist.band_size as u64);
            let logical_size =
                (info_plist.media_size - logical_offset).min(info_plist.band_size as u64);
            let file_name = format!("bands/{:x}", band_index);
            let source = resolver.open_source(Path::new(&file_name))?;
            let source_size = match &source {
                Some(source) => source.size()?,
                None => 0,
            };

            if source.is_some() && source_size == 0 {
                return Err(ErrorTrace::new(format!(
                    "Unsupported zero-sized sparsebundle band file: {}",
                    file_name,
                )));
            }
            if source_size > logical_size {
                return Err(ErrorTrace::new(format!(
                    "Sparsebundle band file exceeds logical band size: {}",
                    file_name,
                )));
            }

            bands.push(SparseBundleBand {
                file_name,
                logical_offset,
                logical_size,
                source,
                source_size,
            });
        }

        Ok(Self {
            band_size: info_plist.band_size,
            media_size: info_plist.media_size,
            bands,
        })
    }

    /// Retrieves the logical band size.
    pub fn band_size(&self) -> u32 {
        self.band_size
    }

    /// Retrieves the logical media size.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Retrieves the number of logical bands.
    pub fn number_of_bands(&self) -> usize {
        self.bands.len()
    }

    /// Retrieves the logical band file names.
    pub fn band_file_names(&self) -> Vec<&str> {
        self.bands
            .iter()
            .map(|band| band.file_name.as_str())
            .collect()
    }

    /// Opens the logical media source backed by sparsebundle band files.
    pub fn open_source(&self) -> Result<DataSourceReference, ErrorTrace> {
        let mut extents = Vec::new();

        for band in self.bands.iter() {
            if let Some(source) = &band.source {
                extents.push(ExtentMapEntry {
                    logical_offset: band.logical_offset,
                    size: band.source_size,
                    target: ExtentMapTarget::Data {
                        source: source.clone(),
                        source_offset: 0,
                    },
                });
                if band.source_size < band.logical_size {
                    extents.push(ExtentMapEntry {
                        logical_offset: band.logical_offset + band.source_size,
                        size: band.logical_size - band.source_size,
                        target: ExtentMapTarget::Zero,
                    });
                }
            } else {
                extents.push(ExtentMapEntry {
                    logical_offset: band.logical_offset,
                    size: band.logical_size,
                    target: ExtentMapTarget::Zero,
                });
            }
        }

        Ok(Arc::new(ExtentMapDataSource::new(extents)?))
    }

    fn read_info_plist(
        resolver: &SourceResolverReference,
    ) -> Result<SparseBundleInfoPlist, ErrorTrace> {
        let mut last_error: Option<ErrorTrace> = None;

        for file_name in INFO_PLIST_FILE_NAMES {
            match Self::read_info_plist_by_name(resolver, file_name) {
                Ok(info_plist) => return Ok(info_plist),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ErrorTrace::new("Unable to read sparsebundle Info.plist".to_string())
        }))
    }

    fn read_info_plist_by_name(
        resolver: &SourceResolverReference,
        file_name: &str,
    ) -> Result<SparseBundleInfoPlist, ErrorTrace> {
        let source = resolver
            .open_source(Path::new(file_name))?
            .ok_or_else(|| ErrorTrace::new(format!("Missing sparsebundle file: {}", file_name)))?;
        let size = source.size()?;

        if size == 0 || size > 65_536 {
            return Err(ErrorTrace::new(format!(
                "Unsupported sparsebundle plist size for {}: {}",
                file_name, size,
            )));
        }

        let data = source.read_all()?;
        let string = String::from_utf8(data).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to convert sparsebundle plist data into UTF-8 string with error: {}",
                error,
            ))
        })?;

        SparseBundleInfoPlist::parse(&string)
    }
}

struct SparseBundleInfoPlist {
    band_size: u32,
    media_size: u64,
}

impl SparseBundleInfoPlist {
    fn parse(data: &str) -> Result<Self, ErrorTrace> {
        let dictionary_version =
            extract_plist_value(data, "CFBundleInfoDictionaryVersion", "string").ok_or_else(
                || {
                    ErrorTrace::new(
                        "Unable to retrieve sparsebundle CFBundleInfoDictionaryVersion value"
                            .to_string(),
                    )
                },
            )?;
        if dictionary_version != "6.0" {
            return Err(ErrorTrace::new(format!(
                "Unsupported sparsebundle CFBundleInfoDictionaryVersion: {}",
                dictionary_version,
            )));
        }

        let bundle_type =
            extract_plist_value(data, "diskimage-bundle-type", "string").ok_or_else(|| {
                ErrorTrace::new(
                    "Unable to retrieve sparsebundle diskimage-bundle-type value".to_string(),
                )
            })?;
        if bundle_type != "com.apple.diskimage.sparsebundle" {
            return Err(ErrorTrace::new(format!(
                "Unsupported sparsebundle diskimage-bundle-type: {}",
                bundle_type,
            )));
        }

        let band_size = extract_plist_value(data, "band-size", "integer")
            .ok_or_else(|| {
                ErrorTrace::new("Unable to retrieve sparsebundle band-size value".to_string())
            })?
            .parse::<u32>()
            .map_err(|error| {
                ErrorTrace::new(format!(
                    "Unable to parse sparsebundle band-size value with error: {}",
                    error,
                ))
            })?;
        if band_size == 0 {
            return Err(ErrorTrace::new(
                "Invalid sparsebundle band-size value: 0".to_string(),
            ));
        }

        let media_size = extract_plist_value(data, "size", "integer")
            .ok_or_else(|| {
                ErrorTrace::new("Unable to retrieve sparsebundle size value".to_string())
            })?
            .parse::<u64>()
            .map_err(|error| {
                ErrorTrace::new(format!(
                    "Unable to parse sparsebundle size value with error: {}",
                    error,
                ))
            })?;
        if media_size == 0 {
            return Err(ErrorTrace::new(
                "Invalid sparsebundle size value: 0".to_string(),
            ));
        }

        Ok(Self {
            band_size,
            media_size,
        })
    }
}

fn extract_plist_value<'a>(data: &'a str, key: &str, value_tag: &str) -> Option<&'a str> {
    let key_marker = format!("<key>{}</key>", key);
    let value_start_marker = format!("<{}>", value_tag);
    let value_end_marker = format!("</{}>", value_tag);

    let key_offset = data.find(&key_marker)? + key_marker.len();
    let value_start_offset =
        data[key_offset..].find(&value_start_marker)? + key_offset + value_start_marker.len();
    let value_end_offset = data[value_start_offset..].find(&value_end_marker)? + value_start_offset;

    Some(data[value_start_offset..value_end_offset].trim())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::resolver::open_local_source_resolver;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn get_image() -> Result<SparseBundleImage, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("sparsebundle/hfsplus.sparsebundle"));
        let resolver = open_local_source_resolver(&path)?;

        SparseBundleImage::open(&resolver)
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let image = get_image()?;

        assert_eq!(image.band_size(), 8_388_608);
        assert_eq!(image.media_size(), 4_194_304);
        assert_eq!(image.number_of_bands(), 1);
        assert_eq!(image.band_file_names(), vec!["bands/0"]);
        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source()?;
        let mut data = vec![0; 2];

        source.read_exact_at(1024, &mut data)?;

        assert_eq!(data, [0x00, 0x53]);
        Ok(())
    }

    #[test]
    fn test_open_source_capabilities() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source()?;
        let capabilities = source.capabilities();

        assert_eq!(
            capabilities.read_concurrency,
            DataSourceReadConcurrency::Concurrent
        );
        assert_eq!(capabilities.seek_cost, DataSourceSeekCost::Cheap);
        Ok(())
    }

    #[test]
    fn test_open_source_uses_zero_fill_for_missing_bands() -> Result<(), ErrorTrace> {
        let temp = tempfile::tempdir().map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to create temporary directory with error: {}",
                error
            ))
        })?;

        fs::create_dir(temp.path().join("bands")).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to create sparsebundle bands directory with error: {}",
                error
            ))
        })?;
        fs::write(
            temp.path().join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>band-size</key><integer>4</integer>
<key>diskimage-bundle-type</key><string>com.apple.diskimage.sparsebundle</string>
<key>size</key><integer>12</integer>
</dict></plist>"#,
        )
        .map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to write sparsebundle Info.plist with error: {}",
                error
            ))
        })?;
        fs::write(temp.path().join("bands/0"), b"abcd").map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to write sparsebundle band file with error: {}",
                error
            ))
        })?;
        fs::write(temp.path().join("bands/2"), b"xy").map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to write sparsebundle band file with error: {}",
                error
            ))
        })?;

        let resolver = open_local_source_resolver(temp.path())?;
        let image = SparseBundleImage::open(&resolver)?;
        let source = image.open_source()?;
        let data = source.read_all()?;

        assert_eq!(image.number_of_bands(), 3);
        assert_eq!(data, b"abcd\0\0\0\0xy\0\0");
        Ok(())
    }

    #[test]
    fn test_read_media() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source()?;
        let (media_offset, md5_hash) = read_data_source_md5(source)?;

        assert_eq!(media_offset, image.media_size());
        assert_eq!(md5_hash.as_str(), "7adf013daec71e509669a9315a6a173c");
        Ok(())
    }
}
