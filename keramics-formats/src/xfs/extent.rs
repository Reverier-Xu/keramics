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

use keramics_core::ErrorTrace;
use keramics_types::bytes_to_u64_be;

use super::block_range::{XfsBlockRange, XfsBlockRangeType};
use super::util::get_data_slice;

/// Parses XFS extent records into block ranges.
pub(super) fn parse_block_ranges(
    data: &[u8],
    number_of_records: usize,
) -> Result<Vec<XfsBlockRange>, ErrorTrace> {
    let mut block_ranges: Vec<XfsBlockRange> = Vec::with_capacity(number_of_records);

    for record_index in 0..number_of_records {
        let record_data: &[u8] = get_data_slice(data, record_index * 16, 16)?;
        block_ranges.push(parse_block_range(record_data));
    }
    Ok(block_ranges)
}

/// Parses a single XFS extent record into a block range.
pub(super) fn parse_block_range(data: &[u8]) -> XfsBlockRange {
    let mut upper: u64 = bytes_to_u64_be!(data, 0);
    let mut lower: u64 = bytes_to_u64_be!(data, 8);

    let number_of_blocks: u64 = lower & 0x001f_ffff;
    lower >>= 21;

    let physical_block_number: u64 = lower | (upper & 0x01ff);
    upper >>= 9;

    let logical_block_number: u64 = upper & 0x003f_ffff_ffff_ffff;
    upper >>= 54;

    let range_type: XfsBlockRangeType = if upper != 0 {
        XfsBlockRangeType::Sparse
    } else {
        XfsBlockRangeType::InFile
    };
    XfsBlockRange::new(
        logical_block_number,
        physical_block_number,
        number_of_blocks,
        range_type,
    )
}

/// Normalizes XFS block ranges and fills sparse gaps.
pub(super) fn normalize_sparse_block_ranges(
    mut block_ranges: Vec<XfsBlockRange>,
    block_size: u64,
    data_size: u64,
) -> Result<Vec<XfsBlockRange>, ErrorTrace> {
    block_ranges.sort_by_key(|block_range| block_range.logical_block_number);

    let mut normalized_block_ranges: Vec<XfsBlockRange> = Vec::new();
    let mut logical_block_number: u64 = 0;

    let mut number_of_blocks: u64 = data_size / block_size;
    if !data_size.is_multiple_of(block_size) {
        number_of_blocks += 1;
    }
    for block_range in block_ranges {
        if block_range.number_of_blocks == 0 {
            continue;
        }
        if block_range.logical_block_number < logical_block_number {
            return Err(keramics_core::error_trace_new!(
                "Overlapping block ranges are unsupported"
            ));
        }
        if block_range.logical_block_number > logical_block_number {
            normalized_block_ranges.push(XfsBlockRange::new(
                logical_block_number,
                0,
                block_range.logical_block_number - logical_block_number,
                XfsBlockRangeType::Sparse,
            ));
        }
        logical_block_number = match block_range
            .logical_block_number
            .checked_add(block_range.number_of_blocks)
        {
            Some(value) => value,
            None => {
                return Err(keramics_core::error_trace_new!(
                    "Block range logical offset overflow"
                ));
            }
        };
        normalized_block_ranges.push(block_range);
    }
    if logical_block_number < number_of_blocks {
        normalized_block_ranges.push(XfsBlockRange::new(
            logical_block_number,
            0,
            number_of_blocks - logical_block_number,
            XfsBlockRangeType::Sparse,
        ));
    }
    Ok(normalized_block_ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_block_range() {
        let logical_block_number: u64 = 1;
        let physical_block_number: u64 = 9;
        let number_of_blocks: u64 = 2;

        let upper: u64 = (logical_block_number << 9) | (physical_block_number & 0x01ff);
        let lower: u64 = ((physical_block_number >> 9) << 21) | number_of_blocks;

        let mut data: [u8; 16] = [0; 16];
        data[0..8].copy_from_slice(&upper.to_be_bytes());
        data[8..16].copy_from_slice(&lower.to_be_bytes());

        let block_range: XfsBlockRange = parse_block_range(&data);

        assert_eq!(block_range.logical_block_number, logical_block_number);
        assert_eq!(block_range.physical_block_number, physical_block_number);
        assert_eq!(block_range.number_of_blocks, number_of_blocks);
        assert_eq!(block_range.range_type, XfsBlockRangeType::InFile);
    }

    #[test]
    fn test_normalize_sparse_block_ranges() -> Result<(), ErrorTrace> {
        let block_ranges: Vec<XfsBlockRange> =
            vec![XfsBlockRange::new(1, 7, 1, XfsBlockRangeType::InFile)];

        let normalized_block_ranges: Vec<XfsBlockRange> =
            normalize_sparse_block_ranges(block_ranges, 512, 1024)?;

        assert_eq!(normalized_block_ranges.len(), 2);
        assert_eq!(normalized_block_ranges[0].logical_block_number, 0);
        assert_eq!(normalized_block_ranges[0].number_of_blocks, 1);
        assert_eq!(
            normalized_block_ranges[0].range_type,
            XfsBlockRangeType::Sparse
        );
        assert_eq!(normalized_block_ranges[1].logical_block_number, 1);
        assert_eq!(normalized_block_ranges[1].physical_block_number, 7);

        Ok(())
    }
}
