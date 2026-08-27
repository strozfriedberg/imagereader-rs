use std::str::FromStr;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AccessMode {
    NoAccess,
    RdOnly,
    Rw,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("unrecognized access mode '{0}'")]
pub struct ParseAccessModeError(String);

impl FromStr for AccessMode {
    type Err = ParseAccessModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "NOACCESS" => Ok(Self::NoAccess),
            "RDONLY" => Ok(Self::RdOnly),
            "RW" => Ok(Self::Rw),
            _ => Err(ParseAccessModeError(s.into())),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ExtentKind {
    Flat,
    SeSparse,
    Sparse,
    Vmfs,
    VmfsSparse,
    VmfsRdm,
    VmfsRaw,
    Zero,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("unrecognized extent kind '{0}'")]
pub struct ParseExtentKindError(String);

impl FromStr for ExtentKind {
    type Err = ParseExtentKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "FLAT" => Ok(Self::Flat),
            "SESPARSE" => Ok(Self::SeSparse),
            "SPARSE" => Ok(Self::Sparse),
            "VMFS" => Ok(Self::Vmfs),
            "VMFSRAW" => Ok(Self::VmfsRaw),
            "VMFSRDM" => Ok(Self::VmfsRdm),
            "VMFSSPARSE" => Ok(Self::VmfsSparse),
            "ZERO" => Ok(Self::Zero),
            _ => Err(ParseExtentKindError(s.into())),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ExtentDescriptionLine {
    sectors: u64,
    kind: ExtentKind,
    filename: Option<String>,
    offset: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("malformed extent description '{line}': {reason}")]
pub struct ParseExtentDescriptionError {
    line: String,
    reason: String,
}

impl ParseExtentDescriptionError {
    fn new(line: &str, reason: impl ToString) -> Self {
        Self {
            line: line.into(),
            reason: reason.to_string(),
        }
    }
}

impl FromStr for ExtentDescriptionLine {
    type Err = ParseExtentDescriptionError;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let err = |reason: &dyn std::fmt::Display| ParseExtentDescriptionError::new(line, reason);
        let s = line.trim();

        // read the access mode. Nothing downstream distinguishes RW from
        // RDONLY -- this reader never writes -- so it is only validated.
        let (tok, s) = s
            .trim_start()
            .split_once(' ')
            .ok_or_else(|| err(&"missing sector count"))?;
        tok.parse::<AccessMode>().map_err(|e| err(&e))?;

        // read the sector count
        let (tok, s) = s
            .trim_start()
            .split_once(' ')
            .ok_or_else(|| err(&"missing extent kind"))?;
        let sectors = tok
            .parse::<u64>()
            .map_err(|e| err(&format!("bad sector count '{tok}': {e}")))?;

        // read the extent kind. It may be the final token on the line: a ZERO
        // extent has no filename, so there is nothing after it.
        let s = s.trim_start();
        let (tok, s) = s.split_once(' ').unwrap_or((s, ""));
        let kind = tok.parse::<ExtentKind>().map_err(|e| err(&e))?;

        // read the optional filename and offset
        let s = s.trim_start();
        let (filename, offset) = if s.is_empty() {
            (None, None)
        } else {
            // read the filename
            let (tok, s) = s
                .strip_prefix('"')
                .and_then(|s| s.rsplit_once('"'))
                .ok_or_else(|| err(&"filename is not double-quoted"))?;
            let filename = Some(tok.to_string());

            // read the offset
            let s = s.trim_start();
            let offset = match s.is_empty() {
                true => None,
                false => Some(
                    s.parse::<u64>()
                        .map_err(|e| err(&format!("bad offset '{s}': {e}")))?,
                ),
            };

            (filename, offset)
        };

        Ok(ExtentDescriptionLine {
            sectors,
            kind,
            filename,
            offset,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ExtentDescriptionInner {
    Flat { filename: String, offset: u64 },
    SeSparse { filename: String },
    Sparse { filename: String },
    Vmfs { filename: String },
    VmfsRaw { filename: String },
    VmfsRdm { filename: String },
    VmfsSparse { filename: String },
    Zero,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExtentDescription {
    pub sectors: u64,
    pub kind: ExtentDescriptionInner,
}

impl ExtentDescription {
    /// The backing filename for this extent, or `None` for a ZERO extent,
    /// which has no file (its bytes are all zero).
    pub fn filename(&self) -> Option<&str> {
        match &self.kind {
            ExtentDescriptionInner::Sparse { filename }
            | ExtentDescriptionInner::SeSparse { filename }
            | ExtentDescriptionInner::Flat { filename, .. }
            | ExtentDescriptionInner::Vmfs { filename }
            | ExtentDescriptionInner::VmfsSparse { filename }
            | ExtentDescriptionInner::VmfsRaw { filename }
            | ExtentDescriptionInner::VmfsRdm { filename } => Some(filename),
            ExtentDescriptionInner::Zero => None,
        }
    }
}

impl TryFrom<ExtentDescriptionLine> for ExtentDescription {
    type Error = ParseExtentDescriptionError;

    fn try_from(edl: ExtentDescriptionLine) -> Result<Self, Self::Error> {
        Ok(ExtentDescription {
            sectors: edl.sectors,
            kind: match edl {
                ExtentDescriptionLine {
                    kind: ExtentKind::Zero,
                    filename: None,
                    offset: None,
                    ..
                } => ExtentDescriptionInner::Zero,
                ExtentDescriptionLine {
                    kind: ExtentKind::Flat,
                    filename: Some(filename),
                    offset: Some(offset),
                    ..
                } => ExtentDescriptionInner::Flat { filename, offset },
                ExtentDescriptionLine {
                    kind: ExtentKind::Sparse,
                    filename: Some(filename),
                    // TODO: apparently 0 is possible here?
                    //                   offset: None,
                    offset: None | Some(0),
                    ..
                } => ExtentDescriptionInner::Sparse { filename },
                ExtentDescriptionLine {
                    kind: ExtentKind::SeSparse,
                    filename: Some(filename),
                    // TODO: apparently 0 is possible here?
                    //                   offset: None,
                    offset: None | Some(0),
                    ..
                } => ExtentDescriptionInner::SeSparse { filename },
                ExtentDescriptionLine {
                    kind: ExtentKind::Vmfs,
                    filename: Some(filename),
                    offset: None,
                    ..
                } => ExtentDescriptionInner::Vmfs { filename },
                ExtentDescriptionLine {
                    kind: ExtentKind::VmfsSparse,
                    filename: Some(filename),
                    offset: None,
                    ..
                } => ExtentDescriptionInner::VmfsSparse { filename },
                ExtentDescriptionLine {
                    kind: ExtentKind::VmfsRdm,
                    filename: Some(filename),
                    offset: None,
                    ..
                } => ExtentDescriptionInner::VmfsRdm { filename },
                ExtentDescriptionLine {
                    kind: ExtentKind::VmfsRaw,
                    filename: Some(filename),
                    offset: None,
                    ..
                } => ExtentDescriptionInner::VmfsRaw { filename },
                ExtentDescriptionLine {
                    kind,
                    filename,
                    offset,
                    ..
                } => {
                    return Err(ParseExtentDescriptionError::new(
                        &format!("{kind:?} {filename:?} {offset:?}"),
                        "unexpected filename or offset for this extent kind",
                    ));
                }
            },
        })
    }
}

pub fn extract_extent_descriptions(
    descriptor: &str,
) -> Result<Vec<ExtentDescription>, ParseExtentDescriptionError> {
    let mut eds = vec![];

    for line in descriptor.lines() {
        match line.trim_start().split_once(' ') {
            Some((a, _)) if a.parse::<AccessMode>().is_ok() => {
                eds.push(line.parse::<ExtentDescriptionLine>()?.try_into()?);
            }
            _ => continue,
        }
    }

    Ok(eds)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn read_extent_description_line_sparse_rw() {
        let ed = r#"RW 4192256 SPARSE "test-f001.vmdk""#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 4192256,
                kind: ExtentKind::Sparse,
                filename: Some("test-f001.vmdk".into()),
                offset: None
            }
        );
    }

    #[test]
    fn read_extent_description_line_sparse_ro() {
        let ed = r#"RDONLY 2048 SPARSE "call-me-stream.vmdk""#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 2048,
                kind: ExtentKind::Sparse,
                filename: Some("call-me-stream.vmdk".into()),
                offset: None
            }
        );
    }

    #[test]
    fn read_extent_description_line_flat() {
        let ed = r#"RW 1048576 FLAT "test-f001.vmdk" 0"#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 1048576,
                kind: ExtentKind::Flat,
                filename: Some("test-f001.vmdk".into()),
                offset: Some(0)
            }
        );
    }

    #[test]
    fn read_extent_description_line_vmfs() {
        let ed = r#"RW 209715200 VMFS "vdisk-PhysicalDrive0-flat.vmdk""#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 209715200,
                kind: ExtentKind::Vmfs,
                filename: Some("vdisk-PhysicalDrive0-flat.vmdk".into()),
                offset: None
            }
        );
    }

    #[test]
    fn read_extent_description_line_vmfssparse() {
        let ed = r#"RW 4096 VMFSSPARSE "vmfs_thick-000001-delta.vmdk""#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 4096,
                kind: ExtentKind::VmfsSparse,
                filename: Some("vmfs_thick-000001-delta.vmdk".into()),
                offset: None
            }
        );
    }

    #[test]
    fn read_extent_description_line_sesparse() {
        let ed = r#"RW 314572800 SESPARSE "sesparse.vmdk""#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 314572800,
                kind: ExtentKind::SeSparse,
                filename: Some("sesparse.vmdk".into()),
                offset: None
            }
        );
    }

    #[test]
    fn read_extent_description_line_zero() {
        // A ZERO extent is the final token on the line with no filename.
        let ed = r#"RW 12345 ZERO"#;
        assert_eq!(
            ed.parse::<ExtentDescriptionLine>().unwrap(),
            ExtentDescriptionLine {
                sectors: 12345,
                kind: ExtentKind::Zero,
                filename: None,
                offset: None,
            }
        );
    }

    #[test]
    fn zero_extent_has_no_filename() {
        let eds = extract_extent_descriptions("RW 12345 ZERO\n").unwrap();
        assert_eq!(eds.len(), 1);
        assert_eq!(eds[0].kind, ExtentDescriptionInner::Zero);
        assert_eq!(eds[0].filename(), None);
    }

    #[test]
    fn vmfsraw_and_vmfsrdm_keep_their_filename() {
        let raw = r#"RW 100 VMFSRAW "/vmfs/devices/disks/naa.raw""#
            .parse::<ExtentDescriptionLine>()
            .and_then(ExtentDescription::try_from)
            .unwrap();
        assert_eq!(raw.filename(), Some("/vmfs/devices/disks/naa.raw"));

        let rdm = r#"RW 100 VMFSRDM "rdm-pointer.vmdk""#
            .parse::<ExtentDescriptionLine>()
            .and_then(ExtentDescription::try_from)
            .unwrap();
        assert_eq!(rdm.filename(), Some("rdm-pointer.vmdk"));
    }

    /*
    TODO: What happens if the filename has a double quote in it?
    TODO: What happens if the filename has a space in it?
    TODO: extent description test for filename containing a space
    TODO: extent description test for filename containing a double quote
    TODO: can extent description filenames be single-quote delimited?
    */
}
