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

use std::iter::Rev;
use std::path::Path;
use std::str::Chars;
use std::sync::Arc;

use keramics_core::ErrorTrace;

use crate::resolver::SourceResolverReference;
use crate::source::{DataSourceReference, SegmentedDataSource, SegmentedSourceSegment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitRawNamingSchema {
    Alphabetic,
    Numeric,
    XOfN,
}

#[derive(Clone)]
struct SplitRawSegment {
    file_name: String,
    logical_offset: u64,
    size: u64,
    source: DataSourceReference,
}

/// Immutable split raw image metadata plus opened segment sources.
pub struct SplitRawImage {
    segment_size: u64,
    segments: Vec<SplitRawSegment>,
    media_size: u64,
}

impl SplitRawImage {
    /// Opens a split raw image from a resolver and first segment file name.
    pub fn open(resolver: &SourceResolverReference, file_name: &Path) -> Result<Self, ErrorTrace> {
        let file_name_string = file_name
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ErrorTrace::new("Split raw image requires a valid file name".to_string())
            })?;
        let naming_schema =
            Self::get_segment_file_naming_schema(file_name_string).ok_or_else(|| {
                ErrorTrace::new(format!(
                    "Unable to determine split raw naming schema from segment file: {}",
                    file_name.display(),
                ))
            })?;

        let (name, name_first_segment_number, name_suffix_size, fixed_segment_count) =
            Self::parse_segment_naming(&naming_schema, file_name_string)?;
        let mut segment_size: u64 = 0;
        let mut segments = Vec::new();
        let mut last_segment_file_name = String::new();
        let mut last_segment_file_size: u64 = 0;
        let mut media_size: u64 = 0;
        let mut logical_offset: u64 = 0;
        let mut segment_number: u16 = 1;

        loop {
            if fixed_segment_count != 0 && segment_number > fixed_segment_count {
                break;
            }

            let segment_file_name = Self::get_segment_file_name(
                &name,
                segment_number,
                fixed_segment_count,
                &naming_schema,
                name_first_segment_number,
                name_suffix_size,
            )?;
            let Some(source) = resolver.open_source(Path::new(&segment_file_name))? else {
                if fixed_segment_count != 0 {
                    return Err(ErrorTrace::new(format!(
                        "Missing split raw segment file: {}",
                        segment_file_name,
                    )));
                }
                break;
            };

            if last_segment_file_size > 0 && last_segment_file_size != segment_size {
                return Err(ErrorTrace::new(format!(
                    "Mismatch in size of split raw segment file: {}",
                    last_segment_file_name,
                )));
            }

            let current_segment_size = source.size()?;
            if current_segment_size == 0 {
                return Err(ErrorTrace::new(format!(
                    "Unsupported zero-sized split raw segment file: {}",
                    segment_file_name,
                )));
            }
            if segment_size == 0 {
                segment_size = current_segment_size;
            }

            segments.push(SplitRawSegment {
                file_name: segment_file_name.clone(),
                logical_offset,
                size: current_segment_size,
                source,
            });

            logical_offset = logical_offset
                .checked_add(current_segment_size)
                .ok_or_else(|| {
                    ErrorTrace::new(
                        "Split raw media size overflow while opening segments".to_string(),
                    )
                })?;
            media_size = logical_offset;
            last_segment_file_name = segment_file_name;
            last_segment_file_size = current_segment_size;
            segment_number = segment_number
                .checked_add(1)
                .ok_or_else(|| ErrorTrace::new("Split raw segment number overflow".to_string()))?;
        }

        if segments.is_empty() {
            return Err(ErrorTrace::new(
                "No split raw segments were opened".to_string(),
            ));
        }

        Ok(Self {
            segment_size,
            segments,
            media_size,
        })
    }

    /// Retrieves the total logical media size.
    pub fn media_size(&self) -> u64 {
        self.media_size
    }

    /// Retrieves the expected size of full-sized segments.
    pub fn segment_size(&self) -> u64 {
        self.segment_size
    }

    /// Retrieves the number of opened segments.
    pub fn number_of_segments(&self) -> usize {
        self.segments.len()
    }

    /// Opens the logical image source backed by the segment sequence.
    pub fn open_source(&self) -> Result<DataSourceReference, ErrorTrace> {
        let segments = self
            .segments
            .iter()
            .map(|segment| SegmentedSourceSegment {
                logical_offset: segment.logical_offset,
                size: segment.size,
                source: segment.source.clone(),
                source_offset: 0,
            })
            .collect::<Vec<SegmentedSourceSegment>>();

        Ok(Arc::new(SegmentedDataSource::new(segments)?))
    }

    /// Retrieves the opened segment file names.
    pub fn segment_file_names(&self) -> Vec<&str> {
        self.segments
            .iter()
            .map(|segment| segment.file_name.as_str())
            .collect()
    }

    fn parse_segment_naming(
        naming_schema: &SplitRawNamingSchema,
        file_name_string: &str,
    ) -> Result<(String, u16, usize, u16), ErrorTrace> {
        match naming_schema {
            SplitRawNamingSchema::Alphabetic => {
                let mut characters: Rev<Chars<'_>> = file_name_string.chars().rev();
                let name_suffix_size: usize = characters
                    .position(|value| value != 'a')
                    .unwrap_or_default();
                let name_size = file_name_string.len() - name_suffix_size;

                Ok((
                    file_name_string[0..name_size].to_string(),
                    0,
                    name_suffix_size,
                    0,
                ))
            }
            SplitRawNamingSchema::Numeric => {
                let mut characters: Rev<Chars<'_>> = file_name_string.chars().rev();
                let name_first_segment_number = match characters.next() {
                    Some('0') => 0,
                    Some('1') => 1,
                    _ => {
                        return Err(ErrorTrace::new(format!(
                            "Unable to determine split raw first segment number from file: {}",
                            file_name_string,
                        )));
                    }
                };
                let name_suffix_size = match characters.position(|value| value != '0') {
                    Some(value_index) => value_index + 1,
                    None => 1,
                };
                let name_size = file_name_string.len() - name_suffix_size;

                Ok((
                    file_name_string[0..name_size].to_string(),
                    name_first_segment_number,
                    name_suffix_size,
                    0,
                ))
            }
            SplitRawNamingSchema::XOfN => {
                let string_index = file_name_string.rfind("1of").ok_or_else(|| {
                    ErrorTrace::new(format!(
                        "Unable to determine split raw segment count from file: {}",
                        file_name_string,
                    ))
                })?;
                let fixed_segment_count = file_name_string[string_index + 3..]
                    .parse::<u16>()
                    .map_err(|error| {
                        ErrorTrace::new(format!(
                            "Unable to parse split raw segment count from file: {} with error: {}",
                            file_name_string, error,
                        ))
                    })?;

                Ok((
                    file_name_string[0..string_index].to_string(),
                    0,
                    0,
                    fixed_segment_count,
                ))
            }
        }
    }

    fn get_segment_file_name(
        name: &str,
        mut segment_number: u16,
        number_of_segment_files: u16,
        naming_schema: &SplitRawNamingSchema,
        name_first_segment_number: u16,
        name_suffix_size: usize,
    ) -> Result<String, ErrorTrace> {
        if segment_number == 0 {
            return Err(ErrorTrace::new(
                "Unsupported split raw segment number: 0".to_string(),
            ));
        }

        match naming_schema {
            SplitRawNamingSchema::Alphabetic => {
                let mut segment_suffix = Vec::new();

                segment_number = (segment_number - 1) + name_first_segment_number;
                while segment_number > 0 {
                    let remainder = segment_number % 26;
                    segment_number /= 26;

                    let character = char::from_u32((remainder + 0x61) as u32).ok_or_else(|| {
                        ErrorTrace::new(
                            "Unable to encode split raw segment name suffix".to_string(),
                        )
                    })?;
                    segment_suffix.push(character);
                }

                if segment_suffix.len() > name_suffix_size {
                    return Err(ErrorTrace::new(
                        "Split raw segment suffix value exceeds configured size".to_string(),
                    ));
                }
                while segment_suffix.len() < name_suffix_size {
                    segment_suffix.push('a');
                }

                Ok(format!(
                    "{}{}",
                    name,
                    segment_suffix.iter().rev().collect::<String>()
                ))
            }
            SplitRawNamingSchema::Numeric => {
                let mut segment_suffix = Vec::new();

                segment_number = (segment_number - 1) + name_first_segment_number;
                while segment_number > 0 {
                    let remainder = segment_number % 10;
                    segment_number /= 10;

                    let character = char::from_u32((remainder + 0x30) as u32).ok_or_else(|| {
                        ErrorTrace::new(
                            "Unable to encode split raw segment name suffix".to_string(),
                        )
                    })?;
                    segment_suffix.push(character);
                }

                if segment_suffix.len() > name_suffix_size {
                    return Err(ErrorTrace::new(
                        "Split raw segment suffix value exceeds configured size".to_string(),
                    ));
                }
                while segment_suffix.len() < name_suffix_size {
                    segment_suffix.push('0');
                }

                Ok(format!(
                    "{}{}",
                    name,
                    segment_suffix.iter().rev().collect::<String>()
                ))
            }
            SplitRawNamingSchema::XOfN => Ok(format!(
                "{}{}of{}",
                name, segment_number, number_of_segment_files,
            )),
        }
    }

    fn get_segment_file_naming_schema(file_name_string: &str) -> Option<SplitRawNamingSchema> {
        match file_name_string.rfind("1of") {
            Some(string_index) => match file_name_string[string_index + 3..].parse::<u16>() {
                Ok(_) => Some(SplitRawNamingSchema::XOfN),
                Err(_) => None,
            },
            None => {
                if file_name_string.ends_with("aa") {
                    Some(SplitRawNamingSchema::Alphabetic)
                } else if file_name_string.ends_with('0') || file_name_string.ends_with('1') {
                    Some(SplitRawNamingSchema::Numeric)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::resolver::open_local_source_resolver;
    use crate::source::{DataSourceReadConcurrency, DataSourceSeekCost};
    use crate::tests::{get_test_data_path, read_data_source_md5};

    fn get_image() -> Result<SplitRawImage, ErrorTrace> {
        let path = PathBuf::from(get_test_data_path("splitraw"));
        let resolver = open_local_source_resolver(&path)?;

        SplitRawImage::open(&resolver, Path::new("ext2.raw.000"))
    }

    #[test]
    fn test_get_segment_file_name() -> Result<(), ErrorTrace> {
        let name = SplitRawImage::get_segment_file_name(
            "image",
            1,
            99,
            &SplitRawNamingSchema::Alphabetic,
            0,
            2,
        )?;
        assert_eq!(name, "imageaa");

        let name = SplitRawImage::get_segment_file_name(
            "image",
            1,
            99,
            &SplitRawNamingSchema::Numeric,
            1,
            1,
        )?;
        assert_eq!(name, "image1");

        let name = SplitRawImage::get_segment_file_name(
            "image.",
            1,
            99,
            &SplitRawNamingSchema::Numeric,
            1,
            3,
        )?;
        assert_eq!(name, "image.001");

        let name = SplitRawImage::get_segment_file_name(
            "image.",
            1,
            99,
            &SplitRawNamingSchema::XOfN,
            1,
            1,
        )?;
        assert_eq!(name, "image.1of99");

        assert!(
            SplitRawImage::get_segment_file_name(
                "image",
                0,
                99,
                &SplitRawNamingSchema::Numeric,
                1,
                1,
            )
            .is_err()
        );

        Ok(())
    }

    #[test]
    fn test_get_segment_file_naming_schema() {
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("imageaa"),
            Some(SplitRawNamingSchema::Alphabetic)
        );
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("image1"),
            Some(SplitRawNamingSchema::Numeric)
        );
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("image001"),
            Some(SplitRawNamingSchema::Numeric)
        );
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("image.1of5"),
            Some(SplitRawNamingSchema::XOfN)
        );
        assert_eq!(SplitRawImage::get_segment_file_naming_schema("image"), None);
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("imageab"),
            None
        );
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("image2"),
            None
        );
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("image.raw"),
            None
        );
        assert_eq!(
            SplitRawImage::get_segment_file_naming_schema("image.2of5"),
            None
        );
    }

    #[test]
    fn test_open() -> Result<(), ErrorTrace> {
        let image = get_image()?;

        assert_eq!(image.media_size(), 4_194_304);
        assert_eq!(image.segment_size(), 1_048_576);
        assert_eq!(image.number_of_segments(), 4);
        assert_eq!(
            image.segment_file_names(),
            vec![
                "ext2.raw.000",
                "ext2.raw.001",
                "ext2.raw.002",
                "ext2.raw.003"
            ]
        );

        Ok(())
    }

    #[test]
    fn test_open_source() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source()?;
        let mut data = vec![0; 2];

        source.read_exact_at(1024 + 56, &mut data)?;

        assert_eq!(data, [0x53, 0xef]);
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
    fn test_open_source_reads_across_segment_boundary() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source()?;
        let mut actual = vec![0; 4];

        source.read_exact_at(image.segment_size() - 2, &mut actual)?;

        let path = PathBuf::from(get_test_data_path("splitraw"));
        let expected_left = fs::read(path.join("ext2.raw.000")).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to read split raw boundary test segment with error: {}",
                error,
            ))
        })?;
        let expected_right = fs::read(path.join("ext2.raw.001")).map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to read split raw boundary test segment with error: {}",
                error,
            ))
        })?;
        let expected = vec![
            expected_left[expected_left.len() - 2],
            expected_left[expected_left.len() - 1],
            expected_right[0],
            expected_right[1],
        ];

        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn test_read_media() -> Result<(), ErrorTrace> {
        let image = get_image()?;
        let source = image.open_source()?;
        let (media_offset, md5_hash) = read_data_source_md5(source)?;

        assert_eq!(media_offset, image.media_size());
        assert_eq!(md5_hash.as_str(), "b1760d0b35a512ef56970df4e6f8c5d6");
        Ok(())
    }

    #[test]
    fn test_open_with_xofn_layout() -> Result<(), ErrorTrace> {
        let temp = tempfile::tempdir().map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to create temporary directory with error: {}",
                error
            ))
        })?;

        fs::write(temp.path().join("image.1of2"), b"abc").map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to write split raw test segment with error: {}",
                error
            ))
        })?;
        fs::write(temp.path().join("image.2of2"), b"defg").map_err(|error| {
            ErrorTrace::new(format!(
                "Unable to write split raw test segment with error: {}",
                error
            ))
        })?;

        let resolver = open_local_source_resolver(temp.path())?;
        let image = SplitRawImage::open(&resolver, Path::new("image.1of2"))?;
        let source = image.open_source()?;
        let data = source.read_all()?;

        assert_eq!(image.number_of_segments(), 2);
        assert_eq!(data, b"abcdefg");
        Ok(())
    }
}
