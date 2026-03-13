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

use keramics_core::{DataStreamReference, ErrorTrace};

/// Retrieves a data slice at a specific offset.
pub(super) fn get_data_slice(
    data: &[u8],
    data_offset: usize,
    data_size: usize,
) -> Result<&[u8], ErrorTrace> {
    let data_end_offset: usize = match data_offset.checked_add(data_size) {
        Some(value) => value,
        None => {
            return Err(keramics_core::error_trace_new!(
                "Invalid data offset value out of bounds"
            ));
        }
    };
    match data.get(data_offset..data_end_offset) {
        Some(slice) => Ok(slice),
        None => Err(keramics_core::error_trace_new!(format!(
            "Invalid data offset: {} value out of bounds",
            data_offset
        ))),
    }
}

/// Reads data from a data stream at a specific offset.
pub(super) fn read_data_at_offset(
    data_stream: &DataStreamReference,
    offset: u64,
    data_size: usize,
) -> Result<Vec<u8>, ErrorTrace> {
    if data_size == 0 {
        return Ok(Vec::new());
    }
    let mut data: Vec<u8> = vec![0; data_size];

    keramics_core::data_stream_read_exact_at_position!(
        data_stream,
        &mut data,
        SeekFrom::Start(offset)
    );

    Ok(data)
}
