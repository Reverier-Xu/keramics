/* Copyright 2026 Reverier Xu <reverier.xu@woooo.tech>
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

use keramics_core::{DataStream, DataStreamReference, ErrorTrace};

use crate::block_tree::BlockTree;

use super::block_range::{XfsBlockRange, XfsBlockRangeType};

/// XFS block stream.
pub struct XfsBlockStream {
    /// The data stream.
    data_stream: Option<DataStreamReference>,

    /// Block size.
    block_size: u32,

    /// Block tree.
    block_tree: BlockTree<XfsBlockRange>,

    /// The current offset.
    current_offset: u64,

    /// The size.
    size: u64,
}

impl XfsBlockStream {
    /// Creates a new block stream.
    pub(super) fn new(block_size: u32, size: u64) -> Self {
        Self {
            data_stream: None,
            block_size,
            block_tree: BlockTree::<XfsBlockRange>::new(0, 0, 0),
            current_offset: 0,
            size,
        }
    }

    /// Opens a block stream.
    pub(super) fn open(
        &mut self,
        data_stream: &DataStreamReference,
        number_of_blocks: u64,
        block_ranges: &[XfsBlockRange],
    ) -> Result<(), ErrorTrace> {
        let block_tree_data_size: u64 = number_of_blocks * (self.block_size as u64);
        self.block_tree =
            BlockTree::<XfsBlockRange>::new(block_tree_data_size, 0, self.block_size as u64);

        for block_range in block_ranges.iter() {
            let range_logical_offset: u64 =
                block_range.logical_block_number * (self.block_size as u64);
            let range_size: u64 = block_range.number_of_blocks * (self.block_size as u64);

            if range_size == 0 {
                continue;
            }
            match self.block_tree.insert_value(
                range_logical_offset,
                range_size,
                block_range.clone(),
            ) {
                Ok(_) => {}
                Err(error) => {
                    return Err(keramics_core::error_trace_new_with_error!(
                        "Unable to insert block range into block tree",
                        error
                    ));
                }
            }
        }
        self.data_stream = Some(data_stream.clone());

        Ok(())
    }

    /// Reads data based on the block ranges.
    fn read_data_from_blocks(&mut self, data: &mut [u8]) -> Result<usize, ErrorTrace> {
        let read_size: usize = data.len();
        let mut data_offset: usize = 0;
        let mut current_offset: u64 = self.current_offset;

        while data_offset < read_size {
            if current_offset >= self.size {
                break;
            }
            let block_range: &XfsBlockRange = match self.block_tree.get_value(current_offset) {
                Ok(Some(value)) => value,
                Ok(None) => {
                    return Err(keramics_core::error_trace_new!(format!(
                        "Missing block range for offset: {} (0x{:08x})",
                        current_offset, current_offset
                    )));
                }
                Err(mut error) => {
                    keramics_core::error_trace_add_frame!(
                        error,
                        format!(
                            "Unable to retrieve block range for offset: {} (0x{:08x})",
                            current_offset, current_offset
                        )
                    );
                    return Err(error);
                }
            };
            let range_logical_offset: u64 =
                block_range.logical_block_number * (self.block_size as u64);
            let range_size: u64 = block_range.number_of_blocks * (self.block_size as u64);

            let range_relative_offset: u64 = current_offset - range_logical_offset;
            let range_remainder_size: u64 = range_size - range_relative_offset;
            let mut range_read_size: usize = read_size - data_offset;

            if (range_read_size as u64) > range_remainder_size {
                range_read_size = range_remainder_size as usize;
            }
            let data_end_offset: usize = data_offset + range_read_size;
            let range_read_count: usize = match block_range.range_type {
                XfsBlockRangeType::InFile => {
                    let data_stream: &DataStreamReference = match self.data_stream.as_ref() {
                        Some(data_stream) => data_stream,
                        None => {
                            return Err(keramics_core::error_trace_new!("Missing data stream"));
                        }
                    };
                    let range_physical_offset: u64 =
                        block_range.physical_block_number * (self.block_size as u64);

                    let read_count: usize = keramics_core::data_stream_read_at_position!(
                        data_stream,
                        &mut data[data_offset..data_end_offset],
                        SeekFrom::Start(range_physical_offset + range_relative_offset)
                    );

                    read_count
                }
                XfsBlockRangeType::Sparse => {
                    data[data_offset..data_end_offset].fill(0);
                    range_read_size
                }
            };
            if range_read_count == 0 {
                break;
            }
            data_offset += range_read_count;
            current_offset += range_read_count as u64;
        }
        Ok(data_offset)
    }
}

impl DataStream for XfsBlockStream {
    /// Retrieves the current position.
    fn get_offset(&mut self) -> Result<u64, ErrorTrace> {
        Ok(self.current_offset)
    }

    /// Retrieves the size of the data.
    fn get_size(&mut self) -> Result<u64, ErrorTrace> {
        Ok(self.size)
    }

    /// Reads data at the current position.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorTrace> {
        if self.current_offset >= self.size {
            return Ok(0);
        }
        let remaining_size: u64 = self.size - self.current_offset;
        let mut read_size: usize = buf.len();

        if (read_size as u64) > remaining_size {
            read_size = remaining_size as usize;
        }
        let read_count: usize = match self.read_data_from_blocks(&mut buf[..read_size]) {
            Ok(read_count) => read_count,
            Err(mut error) => {
                keramics_core::error_trace_add_frame!(error, "Unable to read data from blocks");
                return Err(error);
            }
        };
        self.current_offset += read_count as u64;

        Ok(read_count)
    }

    /// Sets the current position of the data.
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, ErrorTrace> {
        self.current_offset = match pos {
            SeekFrom::Current(relative_offset) => {
                match self.current_offset.checked_add_signed(relative_offset) {
                    Some(offset) => offset,
                    None => {
                        return Err(keramics_core::error_trace_new!(
                            "Invalid offset value out of bounds"
                        ));
                    }
                }
            }
            SeekFrom::End(relative_offset) => match self.size.checked_add_signed(relative_offset) {
                Some(offset) => offset,
                None => {
                    return Err(keramics_core::error_trace_new!(
                        "Invalid offset value out of bounds"
                    ));
                }
            },
            SeekFrom::Start(offset) => offset,
        };
        Ok(self.current_offset)
    }
}
