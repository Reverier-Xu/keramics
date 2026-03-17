/* Copyright 2024-2026 Joachim Metz <joachim.metz@gmail.com>
 * Copyright 2026 Reverier Xu <reverier.xu@woooo.tech>
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

use std::collections::HashSet;

use keramics_core::ErrorTrace;
use keramics_sigscan::{PatternType, ScanContext, Scanner, Signature};

use crate::format_identifier::FormatIdentifier;
use crate::source::DataSourceReference;

const EWF_FILE_HEADER_SIGNATURE: &[u8] = b"EVF\x09\x0d\x0a\xff\x00";
const EXT_SUPERBLOCK_SIGNATURE: &[u8] = &[0x53, 0xef];
const GPT_PARTITION_TABLE_SIGNATURE: &[u8] = b"EFI PART";
const HFS_MASTER_DIRECTORY_BLOCK_SIGNATURE: &[u8] = b"BD";
const HFSPLUS_VOLUME_HEADER_SIGNATURE: &[u8] = b"H+";
const HFSX_VOLUME_HEADER_SIGNATURE: &[u8] = b"HX";
const MBR_BOOT_SIGNATURE: &[u8] = &[0x55, 0xaa];
const NTFS_FILE_SYSTEM_SIGNATURE: &[u8] = b"NTFS    ";
const SPARSEIMAGE_FILE_HEADER_SIGNATURE: &[u8] = b"sprs";
const UDIF_FILE_FOOTER_SIGNATURE: &[u8] = b"koly";
const VHD_FILE_FOOTER_SIGNATURE: &[u8] = b"conectix";
const VHDX_FILE_HEADER_SIGNATURE: &[u8] = b"vhdxfile";
const VMDK_DESCRIPTOR_FILE_HEADER_SIGNATURE: &[u8] = b"# Disk DescriptorFile";
const VMDK_SPARSE_FILE_HEADER_SIGNATURE: &[u8] = b"KDMV";
const XFS_SUPERBLOCK_SIGNATURE: &[u8] = b"XFSB";

/// Format scanner.
pub struct FormatScanner {
    signature_scanner: Scanner,
}

impl FormatScanner {
    /// Creates a new format scanner.
    pub fn new() -> Self {
        Self {
            signature_scanner: Scanner::new(),
        }
    }

    /// Adds Apple Partition Map signatures.
    pub fn add_apm_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "apm1",
            PatternType::BoundToStart,
            560,
            &[
                0x41, 0x70, 0x70, 0x6c, 0x65, 0x5f, 0x70, 0x61, 0x72, 0x74, 0x69, 0x74, 0x69, 0x6f,
                0x6e, 0x5f, 0x6d, 0x61, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ],
        ));
    }

    /// Adds Expert Witness Compression Format signatures.
    pub fn add_ewf_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "ewf1",
            PatternType::BoundToStart,
            0,
            EWF_FILE_HEADER_SIGNATURE,
        ));
    }

    /// Adds ext signatures.
    pub fn add_ext_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "ext1",
            PatternType::BoundToStart,
            1080,
            EXT_SUPERBLOCK_SIGNATURE,
        ));
    }

    /// Adds FAT signatures.
    pub fn add_fat_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "fat1",
            PatternType::BoundToStart,
            54,
            b"FAT12   ",
        ));
        self.signature_scanner.add_signature(Signature::new(
            "fat2",
            PatternType::BoundToStart,
            54,
            b"FAT16   ",
        ));
        self.signature_scanner.add_signature(Signature::new(
            "fat3",
            PatternType::BoundToStart,
            82,
            b"FAT32   ",
        ));
    }

    /// Adds GPT signatures.
    pub fn add_gpt_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "gpt1",
            PatternType::BoundToStart,
            512,
            GPT_PARTITION_TABLE_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "gpt2",
            PatternType::BoundToStart,
            1024,
            GPT_PARTITION_TABLE_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "gpt3",
            PatternType::BoundToStart,
            2048,
            GPT_PARTITION_TABLE_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "gpt4",
            PatternType::BoundToStart,
            4096,
            GPT_PARTITION_TABLE_SIGNATURE,
        ));
    }

    /// Adds HFS signatures.
    pub fn add_hfs_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "hfs1",
            PatternType::BoundToStart,
            1024,
            HFS_MASTER_DIRECTORY_BLOCK_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "hfs2",
            PatternType::BoundToStart,
            1024,
            HFSPLUS_VOLUME_HEADER_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "hfs3",
            PatternType::BoundToStart,
            1024,
            HFSX_VOLUME_HEADER_SIGNATURE,
        ));
    }

    /// Adds MBR signatures.
    pub fn add_mbr_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "mbr1",
            PatternType::BoundToStart,
            510,
            MBR_BOOT_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "mbr2",
            PatternType::BoundToStart,
            1022,
            MBR_BOOT_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "mbr3",
            PatternType::BoundToStart,
            2046,
            MBR_BOOT_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "mbr4",
            PatternType::BoundToStart,
            4094,
            MBR_BOOT_SIGNATURE,
        ));
    }

    /// Adds NTFS signatures.
    pub fn add_ntfs_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "ntfs1",
            PatternType::BoundToStart,
            3,
            NTFS_FILE_SYSTEM_SIGNATURE,
        ));
    }

    /// Adds XFS signatures.
    pub fn add_xfs_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "xfs1",
            PatternType::BoundToStart,
            0,
            XFS_SUPERBLOCK_SIGNATURE,
        ));
    }

    /// Adds PDI signatures.
    pub fn add_pdi_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "pdi1",
            PatternType::BoundToStart,
            39,
            b"<Parallels_disk_image ",
        ));
    }

    /// Adds QCOW signatures.
    pub fn add_qcow_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "qcow1",
            PatternType::BoundToStart,
            0,
            &[0x51, 0x46, 0x49, 0xfb, 0x00, 0x00, 0x00, 0x01],
        ));
        self.signature_scanner.add_signature(Signature::new(
            "qcow2",
            PatternType::BoundToStart,
            0,
            &[0x51, 0x46, 0x49, 0xfb, 0x00, 0x00, 0x00, 0x02],
        ));
        self.signature_scanner.add_signature(Signature::new(
            "qcow3",
            PatternType::BoundToStart,
            0,
            &[0x51, 0x46, 0x49, 0xfb, 0x00, 0x00, 0x00, 0x03],
        ));
    }

    /// Adds sparseimage signatures.
    pub fn add_sparseimage_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "sparseimage1",
            PatternType::BoundToStart,
            0,
            SPARSEIMAGE_FILE_HEADER_SIGNATURE,
        ));
    }

    /// Adds UDIF signatures.
    pub fn add_udif_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "udif1",
            PatternType::BoundToEnd,
            512,
            UDIF_FILE_FOOTER_SIGNATURE,
        ));
    }

    /// Adds VHD signatures.
    pub fn add_vhd_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "vhd1",
            PatternType::BoundToEnd,
            512,
            VHD_FILE_FOOTER_SIGNATURE,
        ));
    }

    /// Adds VHDX signatures.
    pub fn add_vhdx_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "vhdx1",
            PatternType::BoundToStart,
            0,
            VHDX_FILE_HEADER_SIGNATURE,
        ));
    }

    /// Adds VMDK signatures.
    pub fn add_vmdk_signatures(&mut self) {
        self.signature_scanner.add_signature(Signature::new(
            "vmdk1",
            PatternType::BoundToStart,
            0,
            VMDK_DESCRIPTOR_FILE_HEADER_SIGNATURE,
        ));
        self.signature_scanner.add_signature(Signature::new(
            "vmdk2",
            PatternType::BoundToStart,
            0,
            VMDK_SPARSE_FILE_HEADER_SIGNATURE,
        ));
    }

    /// Adds all currently supported signatures.
    pub fn add_default_signatures(&mut self) {
        self.add_apm_signatures();
        self.add_ewf_signatures();
        self.add_ext_signatures();
        self.add_fat_signatures();
        self.add_gpt_signatures();
        self.add_hfs_signatures();
        self.add_mbr_signatures();
        self.add_ntfs_signatures();
        self.add_pdi_signatures();
        self.add_qcow_signatures();
        self.add_sparseimage_signatures();
        self.add_udif_signatures();
        self.add_vhd_signatures();
        self.add_vhdx_signatures();
        self.add_vmdk_signatures();
        self.add_xfs_signatures();
    }

    /// Builds the signature scanner.
    pub fn build(&mut self) -> Result<(), ErrorTrace> {
        self.signature_scanner.build().map_err(|mut error| {
            keramics_core::error_trace_add_frame!(error, "Unable to build signature scanner");
            error
        })
    }

    /// Scans a data source for format signatures.
    pub fn scan_data_source(
        &self,
        data_source: &DataSourceReference,
    ) -> Result<HashSet<FormatIdentifier>, ErrorTrace> {
        let data_size = data_source.size()?;
        let mut scan_context = ScanContext::new(&self.signature_scanner, data_size);
        let mut header_data = vec![0; scan_context.header_range_size as usize];
        let mut footer_data = vec![0; scan_context.footer_range_size as usize];

        let header_read_count = data_source.read_at(0, &mut header_data)?;
        header_data.truncate(header_read_count);
        scan_context.data_offset = 0;
        scan_context.scan_buffer(&header_data);

        let footer_data_offset = data_size.saturating_sub(scan_context.footer_range_size);
        let footer_buffer_offset = if scan_context.footer_range_size < data_size {
            0
        } else {
            (scan_context.footer_range_size - data_size) as usize
        };
        let footer_read_count =
            data_source.read_at(footer_data_offset, &mut footer_data[footer_buffer_offset..])?;

        footer_data.truncate(footer_buffer_offset + footer_read_count);
        scan_context.data_offset = footer_data_offset;
        scan_context.scan_buffer(&footer_data);

        let mut results = HashSet::new();
        for signature in scan_context.results.values() {
            let format_identifier = match signature.identifier.as_str() {
                "apm1" => FormatIdentifier::Apm,
                "ewf1" => FormatIdentifier::Ewf,
                "ext1" => FormatIdentifier::Ext,
                "fat1" | "fat2" | "fat3" => FormatIdentifier::Fat,
                "gpt1" | "gpt2" | "gpt3" | "gpt4" => FormatIdentifier::Gpt,
                "hfs1" | "hfs2" | "hfs3" => FormatIdentifier::Hfs,
                "mbr1" | "mbr2" | "mbr3" | "mbr4" => FormatIdentifier::Mbr,
                "ntfs1" => FormatIdentifier::Ntfs,
                "pdi1" => FormatIdentifier::Pdi,
                "qcow1" | "qcow2" | "qcow3" => FormatIdentifier::Qcow,
                "sparseimage1" => FormatIdentifier::SparseImage,
                "udif1" => FormatIdentifier::Udif,
                "vhd1" => FormatIdentifier::Vhd,
                "vhdx1" => FormatIdentifier::Vhdx,
                "vmdk1" | "vmdk2" => FormatIdentifier::Vmdk,
                "xfs1" => FormatIdentifier::Xfs,
                _ => FormatIdentifier::Unknown,
            };

            results.insert(format_identifier);
        }

        Ok(results)
    }
}

impl Default for FormatScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use keramics_core::ErrorTrace;

    use super::*;
    use crate::source::{MemoryDataSource, open_local_data_source};
    use crate::tests::get_test_data_path;

    #[test]
    fn test_build() -> Result<(), ErrorTrace> {
        let mut format_scanner = FormatScanner::new();
        format_scanner.add_default_signatures();
        format_scanner.build()
    }

    #[test]
    fn test_scan_data_source() -> Result<(), ErrorTrace> {
        let mut format_scanner = FormatScanner::new();
        format_scanner.add_default_signatures();
        format_scanner.build()?;

        let path_buf = PathBuf::from(get_test_data_path("qcow/ext2.qcow2"));
        let data_source = open_local_data_source(&path_buf)?;
        let scan_results = format_scanner.scan_data_source(&data_source)?;

        assert_eq!(scan_results.len(), 1);
        assert_eq!(scan_results.iter().next(), Some(&FormatIdentifier::Qcow));

        Ok(())
    }

    #[test]
    fn test_scan_data_source_with_xfs() -> Result<(), ErrorTrace> {
        let mut format_scanner = FormatScanner::new();
        format_scanner.add_xfs_signatures();
        format_scanner.build()?;

        let data_source: DataSourceReference = Arc::new(MemoryDataSource::new(b"XFSB".to_vec()));
        let scan_results = format_scanner.scan_data_source(&data_source)?;

        assert_eq!(scan_results.len(), 1);
        assert_eq!(scan_results.iter().next(), Some(&FormatIdentifier::Xfs));

        Ok(())
    }
}
