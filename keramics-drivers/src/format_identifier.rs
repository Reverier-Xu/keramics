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

use std::fmt;

/// Format identifier.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum FormatIdentifier {
    Apm,
    Ewf,
    Ext,
    Fat,
    Gpt,
    Hfs,
    Mbr,
    Ntfs,
    Pdi,
    Qcow,
    SparseImage,
    SplitRaw,
    Udif,
    Unknown,
    Vhd,
    Vhdx,
    Vmdk,
    Xfs,
}

impl fmt::Display for FormatIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let string = match self {
            Self::Apm => "apm",
            Self::Ewf => "ewf",
            Self::Ext => "ext",
            Self::Fat => "fat",
            Self::Gpt => "gpt",
            Self::Hfs => "hfs",
            Self::Mbr => "mbr",
            Self::Ntfs => "ntfs",
            Self::Pdi => "pdi",
            Self::Qcow => "qcow",
            Self::SparseImage => "sparseimage",
            Self::SplitRaw => "splitraw",
            Self::Udif => "udif",
            Self::Unknown => "unknown",
            Self::Vhd => "vhd",
            Self::Vhdx => "vhdx",
            Self::Vmdk => "vmdk",
            Self::Xfs => "xfs",
        };

        write!(formatter, "{}", string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_identifier_fmt() {
        assert_eq!(FormatIdentifier::Apm.to_string(), "apm");
        assert_eq!(FormatIdentifier::Ewf.to_string(), "ewf");
        assert_eq!(FormatIdentifier::Ext.to_string(), "ext");
        assert_eq!(FormatIdentifier::Fat.to_string(), "fat");
        assert_eq!(FormatIdentifier::Gpt.to_string(), "gpt");
        assert_eq!(FormatIdentifier::Hfs.to_string(), "hfs");
        assert_eq!(FormatIdentifier::Mbr.to_string(), "mbr");
        assert_eq!(FormatIdentifier::Ntfs.to_string(), "ntfs");
        assert_eq!(FormatIdentifier::Pdi.to_string(), "pdi");
        assert_eq!(FormatIdentifier::Qcow.to_string(), "qcow");
        assert_eq!(FormatIdentifier::SparseImage.to_string(), "sparseimage");
        assert_eq!(FormatIdentifier::SplitRaw.to_string(), "splitraw");
        assert_eq!(FormatIdentifier::Udif.to_string(), "udif");
        assert_eq!(FormatIdentifier::Unknown.to_string(), "unknown");
        assert_eq!(FormatIdentifier::Vhd.to_string(), "vhd");
        assert_eq!(FormatIdentifier::Vhdx.to_string(), "vhdx");
        assert_eq!(FormatIdentifier::Vmdk.to_string(), "vmdk");
        assert_eq!(FormatIdentifier::Xfs.to_string(), "xfs");
    }
}
