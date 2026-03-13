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

use std::cmp::min;

use keramics_core::{DataStreamReference, ErrorTrace, open_fake_data_stream};
use keramics_formats::Path;
use keramics_formats::xfs::{XfsFileEntry, XfsFileSystem};
use keramics_types::ByteString;

const XFS_FILE_MODE_TYPE_DIRECTORY: u16 = 0x4000;
const XFS_FILE_MODE_TYPE_REGULAR_FILE: u16 = 0x8000;
const XFS_FILE_MODE_TYPE_SYMBOLIC_LINK: u16 = 0xa000;

const XFS_INODE_FORMAT_LOCAL: u8 = 1;
const XFS_INODE_FORMAT_EXTENTS: u8 = 2;

const XFS_SUPERBLOCK_SIGNATURE: &[u8; 4] = b"XFSB";
const XFS_INODE_BTREE_SIGNATURE_V5: &[u8; 4] = b"IAB3";
const XFS_SUPERBLOCK_FEATURE2_FILE_TYPE: u32 = 0x0000_0200;

fn read_data_stream(data_stream: &DataStreamReference) -> Result<Vec<u8>, ErrorTrace> {
    let mut buffer: Vec<u8> = vec![0; 4096];
    let mut data: Vec<u8> = Vec::new();

    match data_stream.write() {
        Ok(mut data_stream) => loop {
            let read_count: usize = data_stream.read(&mut buffer)?;
            if read_count == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read_count]);
        },
        Err(error) => {
            return Err(keramics_core::error_trace_new_with_error!(
                "Unable to obtain write lock on data stream",
                error
            ));
        }
    };
    Ok(data)
}

fn open_file_system(image: &[u8]) -> Result<XfsFileSystem, ErrorTrace> {
    let data_stream: DataStreamReference = open_fake_data_stream(image);
    let mut file_system: XfsFileSystem = XfsFileSystem::new();

    file_system.read_data_stream(&data_stream)?;

    Ok(file_system)
}

fn build_superblock(root_inode_number: u64) -> Vec<u8> {
    build_superblock_with_geometry(root_inode_number, 8, 1, 3)
}

fn build_superblock_with_geometry(
    root_inode_number: u64,
    allocation_group_block_size: u32,
    number_of_allocation_groups: u32,
    relative_block_bits: u8,
) -> Vec<u8> {
    let mut data: Vec<u8> = vec![0; 512];

    data[0..4].copy_from_slice(XFS_SUPERBLOCK_SIGNATURE);
    data[4..8].copy_from_slice(&512u32.to_be_bytes());
    data[56..64].copy_from_slice(&root_inode_number.to_be_bytes());
    data[84..88].copy_from_slice(&allocation_group_block_size.to_be_bytes());
    data[88..92].copy_from_slice(&number_of_allocation_groups.to_be_bytes());
    data[100..102].copy_from_slice(&5u16.to_be_bytes());
    data[102..104].copy_from_slice(&512u16.to_be_bytes());
    data[104..106].copy_from_slice(&256u16.to_be_bytes());
    data[106..108].copy_from_slice(&2u16.to_be_bytes());
    data[108..116].copy_from_slice(b"xfs_test");
    data[123] = 1;
    data[124] = relative_block_bits;
    data[192] = 0;
    data[200..204].copy_from_slice(&XFS_SUPERBLOCK_FEATURE2_FILE_TYPE.to_be_bytes());

    data
}

fn put_agi_and_inobt_leaf(image: &mut [u8], root_block_number: u32, first_inode_number: u32) {
    let agi: &mut [u8] = &mut image[1024..1536];
    agi[0..4].copy_from_slice(b"XAGI");
    agi[4..8].copy_from_slice(&1u32.to_be_bytes());
    agi[12..16].copy_from_slice(&8u32.to_be_bytes());
    agi[20..24].copy_from_slice(&root_block_number.to_be_bytes());
    agi[24..28].copy_from_slice(&1u32.to_be_bytes());

    let root_start_offset: usize = (root_block_number as usize) * 512;
    let root_end_offset: usize = root_start_offset + 512;
    let root: &mut [u8] = &mut image[root_start_offset..root_end_offset];
    root[0..4].copy_from_slice(XFS_INODE_BTREE_SIGNATURE_V5);
    root[4..6].copy_from_slice(&0u16.to_be_bytes());
    root[6..8].copy_from_slice(&1u16.to_be_bytes());
    root[56..60].copy_from_slice(&first_inode_number.to_be_bytes());
}

fn put_inode(
    image: &mut [u8],
    inode_offset: usize,
    file_mode: u16,
    data_fork_format: u8,
    data_size: u64,
    number_of_extents: u32,
    data_fork: &[u8],
) {
    let inode: &mut [u8] = &mut image[inode_offset..inode_offset + 256];

    inode[0..2].copy_from_slice(b"IN");
    inode[2..4].copy_from_slice(&file_mode.to_be_bytes());
    inode[4] = 3;
    inode[5] = data_fork_format;
    inode[8..12].copy_from_slice(&1000u32.to_be_bytes());
    inode[12..16].copy_from_slice(&1000u32.to_be_bytes());
    inode[16..20].copy_from_slice(&1u32.to_be_bytes());
    inode[56..64].copy_from_slice(&data_size.to_be_bytes());
    inode[76..80].copy_from_slice(&number_of_extents.to_be_bytes());
    inode[82] = 0;

    let copy_size: usize = min(data_fork.len(), 256 - 176);
    inode[176..176 + copy_size].copy_from_slice(&data_fork[..copy_size]);
}

fn encode_extent(
    logical_block_number: u64,
    physical_block_number: u64,
    number_of_blocks: u64,
) -> [u8; 16] {
    let upper: u64 = (logical_block_number << 9) | (physical_block_number & 0x01ff);
    let lower: u64 = ((physical_block_number >> 9) << 21) | (number_of_blocks & 0x001f_ffff);

    let mut data: [u8; 16] = [0; 16];
    data[0..8].copy_from_slice(&upper.to_be_bytes());
    data[8..16].copy_from_slice(&lower.to_be_bytes());

    data
}

fn build_shortform_directory(entries: &[(&str, u32)]) -> Vec<u8> {
    let mut directory_data: Vec<u8> = Vec::new();

    directory_data.push(entries.len() as u8);
    directory_data.push(0u8);
    directory_data.extend_from_slice(&2u32.to_be_bytes());

    for (name, inode_number) in entries.iter() {
        directory_data.push(name.len() as u8);
        directory_data.extend_from_slice(&0u16.to_be_bytes());
        directory_data.extend_from_slice(name.as_bytes());
        directory_data.push(1u8);
        directory_data.extend_from_slice(&inode_number.to_be_bytes());
    }
    directory_data
}

#[test]
fn read_xfs_inline_file_and_shortform_directory() -> Result<(), ErrorTrace> {
    let mut image: Vec<u8> = vec![0; 4096];
    let superblock: Vec<u8> = build_superblock(2);
    image[0..512].copy_from_slice(&superblock);
    put_agi_and_inobt_leaf(&mut image, 4, 0);

    let directory_data: Vec<u8> = build_shortform_directory(&[("hello.txt", 3)]);
    put_inode(
        &mut image,
        512,
        XFS_FILE_MODE_TYPE_DIRECTORY,
        XFS_INODE_FORMAT_LOCAL,
        directory_data.len() as u64,
        0,
        &directory_data,
    );
    put_inode(
        &mut image,
        768,
        XFS_FILE_MODE_TYPE_REGULAR_FILE,
        XFS_INODE_FORMAT_LOCAL,
        3,
        0,
        b"abc",
    );

    let file_system: XfsFileSystem = open_file_system(&image)?;

    assert_eq!(file_system.get_format_version(), 5);
    assert_eq!(
        file_system.get_volume_label(),
        Some(ByteString::from("xfs_test")).as_ref()
    );

    let mut root_directory: XfsFileEntry = file_system.get_root_directory()?;
    assert_eq!(root_directory.get_inode_number(), 2);
    assert_eq!(root_directory.get_number_of_sub_file_entries()?, 1);

    let path: Path = Path::from("/hello.txt");
    let mut file_entry: XfsFileEntry = file_system.get_file_entry_by_path(&path)?.unwrap();
    assert_eq!(file_entry.get_size(), 3);
    assert_eq!(
        file_entry.get_name(),
        Some(ByteString::from("hello.txt")).as_ref()
    );

    let data_stream: DataStreamReference = file_entry.get_data_stream()?.unwrap();
    let data: Vec<u8> = read_data_stream(&data_stream)?;
    assert_eq!(data, b"abc");

    Ok(())
}

#[test]
fn read_xfs_extent_file_and_block_directory() -> Result<(), ErrorTrace> {
    let mut image: Vec<u8> = vec![0; 4096];
    let superblock: Vec<u8> = build_superblock(2);
    image[0..512].copy_from_slice(&superblock);
    put_agi_and_inobt_leaf(&mut image, 4, 0);

    let directory_extent: [u8; 16] = encode_extent(0, 5, 1);
    put_inode(
        &mut image,
        512,
        XFS_FILE_MODE_TYPE_DIRECTORY,
        XFS_INODE_FORMAT_EXTENTS,
        0,
        1,
        &directory_extent,
    );

    let file_extent: [u8; 16] = encode_extent(0, 3, 1);
    put_inode(
        &mut image,
        768,
        XFS_FILE_MODE_TYPE_REGULAR_FILE,
        XFS_INODE_FORMAT_EXTENTS,
        3,
        1,
        &file_extent,
    );

    let directory_block: &mut [u8] = &mut image[2560..3072];
    directory_block[0..4].copy_from_slice(b"XD2D");
    let mut data_offset: usize = 16;
    directory_block[data_offset..data_offset + 8].copy_from_slice(&3u64.to_be_bytes());
    directory_block[data_offset + 8] = 4;
    directory_block[data_offset + 9..data_offset + 13].copy_from_slice(b"file");
    directory_block[data_offset + 13] = 1;
    directory_block[data_offset + 14..data_offset + 16].copy_from_slice(&0u16.to_be_bytes());
    data_offset += 16;
    directory_block[data_offset..data_offset + 2].copy_from_slice(&0xffffu16.to_be_bytes());
    directory_block[data_offset + 2..data_offset + 4]
        .copy_from_slice(&((512 - data_offset) as u16).to_be_bytes());

    image[1536..1539].copy_from_slice(b"XYZ");

    let file_system: XfsFileSystem = open_file_system(&image)?;
    let path: Path = Path::from("/file");
    let mut file_entry: XfsFileEntry = file_system.get_file_entry_by_path(&path)?.unwrap();
    let data_stream: DataStreamReference = file_entry.get_data_stream()?.unwrap();
    let data: Vec<u8> = read_data_stream(&data_stream)?;

    assert_eq!(data, b"XYZ");

    Ok(())
}

#[test]
fn read_xfs_symbolic_link_directory_path() -> Result<(), ErrorTrace> {
    let mut image: Vec<u8> = vec![0; 8192];
    let superblock: Vec<u8> = build_superblock(2);
    image[0..512].copy_from_slice(&superblock);
    put_agi_and_inobt_leaf(&mut image, 4, 0);

    let root_directory_data: Vec<u8> = build_shortform_directory(&[("linkdir", 3), ("real", 10)]);
    put_inode(
        &mut image,
        512,
        XFS_FILE_MODE_TYPE_DIRECTORY,
        XFS_INODE_FORMAT_LOCAL,
        root_directory_data.len() as u64,
        0,
        &root_directory_data,
    );
    put_inode(
        &mut image,
        768,
        XFS_FILE_MODE_TYPE_SYMBOLIC_LINK,
        XFS_INODE_FORMAT_LOCAL,
        4,
        0,
        b"real",
    );

    let real_directory_data: Vec<u8> = build_shortform_directory(&[("x", 11)]);
    put_inode(
        &mut image,
        2560,
        XFS_FILE_MODE_TYPE_DIRECTORY,
        XFS_INODE_FORMAT_LOCAL,
        real_directory_data.len() as u64,
        0,
        &real_directory_data,
    );
    put_inode(
        &mut image,
        2816,
        XFS_FILE_MODE_TYPE_REGULAR_FILE,
        XFS_INODE_FORMAT_LOCAL,
        2,
        0,
        b"ok",
    );

    let file_system: XfsFileSystem = open_file_system(&image)?;

    let path: Path = Path::from("/linkdir");
    let mut symbolic_link_entry: XfsFileEntry = file_system.get_file_entry_by_path(&path)?.unwrap();
    assert!(symbolic_link_entry.is_symbolic_link());
    assert_eq!(
        symbolic_link_entry.get_symbolic_link_target()?,
        Some(ByteString::from("real")).as_ref()
    );

    let path: Path = Path::from("/linkdir/x");
    let mut file_entry: XfsFileEntry = file_system.get_file_entry_by_path(&path)?.unwrap();
    let data_stream: DataStreamReference = file_entry.get_data_stream()?.unwrap();
    let data: Vec<u8> = read_data_stream(&data_stream)?;
    assert_eq!(data, b"ok");

    Ok(())
}

#[test]
fn read_xfs_extent_with_allocation_group_geometry() -> Result<(), ErrorTrace> {
    let mut image: Vec<u8> = vec![0; 16384];
    let superblock: Vec<u8> = build_superblock_with_geometry(2, 6, 2, 3);
    image[0..512].copy_from_slice(&superblock);
    put_agi_and_inobt_leaf(&mut image, 4, 0);

    let directory_data: Vec<u8> = build_shortform_directory(&[("file", 3)]);
    put_inode(
        &mut image,
        512,
        XFS_FILE_MODE_TYPE_DIRECTORY,
        XFS_INODE_FORMAT_LOCAL,
        directory_data.len() as u64,
        0,
        &directory_data,
    );

    let file_extent: [u8; 16] = encode_extent(0, 9, 1);
    put_inode(
        &mut image,
        768,
        XFS_FILE_MODE_TYPE_REGULAR_FILE,
        XFS_INODE_FORMAT_EXTENTS,
        4,
        1,
        &file_extent,
    );
    image[7 * 512..7 * 512 + 4].copy_from_slice(b"DATA");

    let file_system: XfsFileSystem = open_file_system(&image)?;
    let path: Path = Path::from("/file");
    let mut file_entry: XfsFileEntry = file_system.get_file_entry_by_path(&path)?.unwrap();
    let data_stream: DataStreamReference = file_entry.get_data_stream()?.unwrap();
    let data: Vec<u8> = read_data_stream(&data_stream)?;

    assert_eq!(data, b"DATA");

    Ok(())
}
