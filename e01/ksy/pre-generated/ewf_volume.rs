// This is a generated file! Please edit source .ksy file and use kaitai-struct-compiler to rebuild

#[allow(unused_imports)]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
#[allow(irrefutable_let_patterns)]
#[allow(unused_comparisons)]
#[allow(arithmetic_overflow)]
#[allow(overflowing_literals)]

extern crate kaitai;
use kaitai::*;
use std::convert::{TryFrom, TryInto};
use std::cell::{Ref, Cell, RefCell};
use std::rc::{Rc, Weak};

/**
 * \sa https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#353-ftk-imager-encase-1-to-7-and-linen-5-to-7-ewf-e01 Source
 */

#[derive(Default, Debug, Clone)]
pub struct EwfVolume {
    pub _root: SharedType<EwfVolume>,
    pub _parent: SharedType<EwfVolume>,
    pub _self: SharedType<Self>,
    media_type: RefCell<EwfVolume_MediaTypesEnum>,
    unknown1: RefCell<Vec<u8>>,
    number_of_chunks: RefCell<u32>,
    sectors_per_chunk: RefCell<u32>,
    bytes_per_sector: RefCell<u32>,
    number_of_sectors: RefCell<u64>,
    chs_cylinders: RefCell<u32>,
    chs_heads: RefCell<u32>,
    chs_sectors: RefCell<u32>,
    media_flags: RefCell<EwfVolume_MediaFlagsEnum>,
    unknown2: RefCell<Vec<u8>>,
    palm_volume_start_sector: RefCell<u32>,
    unknown3: RefCell<Vec<u8>>,
    smart_logs_start_sector: RefCell<u32>,
    compression_level: RefCell<EwfVolume_CompressionLevelEnum>,
    unknown4: RefCell<Vec<u8>>,
    error_granularity: RefCell<u32>,
    unknown5: RefCell<Vec<u8>>,
    set_identifier: RefCell<Vec<u8>>,
    unknown6: RefCell<Vec<u8>>,
    signature: RefCell<Vec<u8>>,
    checksum: RefCell<u32>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfVolume {
    type Root = EwfVolume;
    type Parent = EwfVolume;

    fn read<S: KStream>(
        self_rc: &OptRc<Self>,
        _io: &S,
        _root: SharedType<Self::Root>,
        _parent: SharedType<Self::Parent>,
    ) -> KResult<()> {
        *self_rc._io.borrow_mut() = _io.clone();
        self_rc._root.set(_root.get());
        self_rc._parent.set(_parent.get());
        self_rc._self.set(Ok(self_rc.clone()));
        let _rrc = self_rc._root.get_value().borrow().upgrade();
        let _prc = self_rc._parent.get_value().borrow().upgrade();
        let _r = _rrc.as_ref().unwrap();
        *self_rc.media_type.borrow_mut() = (_io.read_u1()? as i64).try_into()?;
        *self_rc.unknown1.borrow_mut() = _io.read_bytes(3 as usize)?.into();
        *self_rc.number_of_chunks.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.sectors_per_chunk.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.bytes_per_sector.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.number_of_sectors.borrow_mut() = _io.read_u8le()?.into();
        *self_rc.chs_cylinders.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.chs_heads.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.chs_sectors.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.media_flags.borrow_mut() = (_io.read_u1()? as i64).try_into()?;
        *self_rc.unknown2.borrow_mut() = _io.read_bytes(3 as usize)?.into();
        *self_rc.palm_volume_start_sector.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.unknown3.borrow_mut() = _io.read_bytes(4 as usize)?.into();
        *self_rc.smart_logs_start_sector.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.compression_level.borrow_mut() = (_io.read_u1()? as i64).try_into()?;
        *self_rc.unknown4.borrow_mut() = _io.read_bytes(3 as usize)?.into();
        *self_rc.error_granularity.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.unknown5.borrow_mut() = _io.read_bytes(4 as usize)?.into();
        *self_rc.set_identifier.borrow_mut() = _io.read_bytes(16 as usize)?.into();
        *self_rc.unknown6.borrow_mut() = _io.read_bytes(963 as usize)?.into();
        *self_rc.signature.borrow_mut() = _io.read_bytes(5 as usize)?.into();
        *self_rc.checksum.borrow_mut() = _io.read_u4le()?.into();
        Ok(())
    }
}
impl EwfVolume {
}
impl EwfVolume {
    pub fn media_type(&self) -> Ref<'_, EwfVolume_MediaTypesEnum> {
        self.media_type.borrow()
    }
}
impl EwfVolume {
    pub fn unknown1(&self) -> Ref<'_, Vec<u8>> {
        self.unknown1.borrow()
    }
}
impl EwfVolume {
    pub fn number_of_chunks(&self) -> Ref<'_, u32> {
        self.number_of_chunks.borrow()
    }
}
impl EwfVolume {
    pub fn sectors_per_chunk(&self) -> Ref<'_, u32> {
        self.sectors_per_chunk.borrow()
    }
}
impl EwfVolume {
    pub fn bytes_per_sector(&self) -> Ref<'_, u32> {
        self.bytes_per_sector.borrow()
    }
}
impl EwfVolume {
    pub fn number_of_sectors(&self) -> Ref<'_, u64> {
        self.number_of_sectors.borrow()
    }
}
impl EwfVolume {
    pub fn chs_cylinders(&self) -> Ref<'_, u32> {
        self.chs_cylinders.borrow()
    }
}
impl EwfVolume {
    pub fn chs_heads(&self) -> Ref<'_, u32> {
        self.chs_heads.borrow()
    }
}
impl EwfVolume {
    pub fn chs_sectors(&self) -> Ref<'_, u32> {
        self.chs_sectors.borrow()
    }
}
impl EwfVolume {
    pub fn media_flags(&self) -> Ref<'_, EwfVolume_MediaFlagsEnum> {
        self.media_flags.borrow()
    }
}
impl EwfVolume {
    pub fn unknown2(&self) -> Ref<'_, Vec<u8>> {
        self.unknown2.borrow()
    }
}
impl EwfVolume {
    pub fn palm_volume_start_sector(&self) -> Ref<'_, u32> {
        self.palm_volume_start_sector.borrow()
    }
}
impl EwfVolume {
    pub fn unknown3(&self) -> Ref<'_, Vec<u8>> {
        self.unknown3.borrow()
    }
}
impl EwfVolume {
    pub fn smart_logs_start_sector(&self) -> Ref<'_, u32> {
        self.smart_logs_start_sector.borrow()
    }
}
impl EwfVolume {
    pub fn compression_level(&self) -> Ref<'_, EwfVolume_CompressionLevelEnum> {
        self.compression_level.borrow()
    }
}
impl EwfVolume {
    pub fn unknown4(&self) -> Ref<'_, Vec<u8>> {
        self.unknown4.borrow()
    }
}
impl EwfVolume {
    pub fn error_granularity(&self) -> Ref<'_, u32> {
        self.error_granularity.borrow()
    }
}
impl EwfVolume {
    pub fn unknown5(&self) -> Ref<'_, Vec<u8>> {
        self.unknown5.borrow()
    }
}
impl EwfVolume {
    pub fn set_identifier(&self) -> Ref<'_, Vec<u8>> {
        self.set_identifier.borrow()
    }
}
impl EwfVolume {
    pub fn unknown6(&self) -> Ref<'_, Vec<u8>> {
        self.unknown6.borrow()
    }
}
impl EwfVolume {
    pub fn signature(&self) -> Ref<'_, Vec<u8>> {
        self.signature.borrow()
    }
}
impl EwfVolume {
    pub fn checksum(&self) -> Ref<'_, u32> {
        self.checksum.borrow()
    }
}
impl EwfVolume {
    pub fn _io(&self) -> Ref<'_, BytesReader> {
        self._io.borrow()
    }
}
#[derive(Debug, PartialEq, Clone)]
pub enum EwfVolume_MediaTypesEnum {
    Removable,
    Fixed,
    Optical,
    SingleFiles,
    Memory,
    Unknown(i64),
}

impl TryFrom<i64> for EwfVolume_MediaTypesEnum {
    type Error = KError;
    fn try_from(flag: i64) -> KResult<EwfVolume_MediaTypesEnum> {
        match flag {
            0 => Ok(EwfVolume_MediaTypesEnum::Removable),
            1 => Ok(EwfVolume_MediaTypesEnum::Fixed),
            3 => Ok(EwfVolume_MediaTypesEnum::Optical),
            14 => Ok(EwfVolume_MediaTypesEnum::SingleFiles),
            16 => Ok(EwfVolume_MediaTypesEnum::Memory),
            _ => Ok(EwfVolume_MediaTypesEnum::Unknown(flag)),
        }
    }
}

impl From<&EwfVolume_MediaTypesEnum> for i64 {
    fn from(v: &EwfVolume_MediaTypesEnum) -> Self {
        match *v {
            EwfVolume_MediaTypesEnum::Removable => 0,
            EwfVolume_MediaTypesEnum::Fixed => 1,
            EwfVolume_MediaTypesEnum::Optical => 3,
            EwfVolume_MediaTypesEnum::SingleFiles => 14,
            EwfVolume_MediaTypesEnum::Memory => 16,
            EwfVolume_MediaTypesEnum::Unknown(v) => v
        }
    }
}

impl Default for EwfVolume_MediaTypesEnum {
    fn default() -> Self { EwfVolume_MediaTypesEnum::Unknown(0) }
}

#[derive(Debug, PartialEq, Clone)]
pub enum EwfVolume_MediaFlagsEnum {
    Image,
    Physical,
    Fastbloc,
    Tableau,
    Unknown(i64),
}

impl TryFrom<i64> for EwfVolume_MediaFlagsEnum {
    type Error = KError;
    fn try_from(flag: i64) -> KResult<EwfVolume_MediaFlagsEnum> {
        match flag {
            1 => Ok(EwfVolume_MediaFlagsEnum::Image),
            2 => Ok(EwfVolume_MediaFlagsEnum::Physical),
            4 => Ok(EwfVolume_MediaFlagsEnum::Fastbloc),
            8 => Ok(EwfVolume_MediaFlagsEnum::Tableau),
            _ => Ok(EwfVolume_MediaFlagsEnum::Unknown(flag)),
        }
    }
}

impl From<&EwfVolume_MediaFlagsEnum> for i64 {
    fn from(v: &EwfVolume_MediaFlagsEnum) -> Self {
        match *v {
            EwfVolume_MediaFlagsEnum::Image => 1,
            EwfVolume_MediaFlagsEnum::Physical => 2,
            EwfVolume_MediaFlagsEnum::Fastbloc => 4,
            EwfVolume_MediaFlagsEnum::Tableau => 8,
            EwfVolume_MediaFlagsEnum::Unknown(v) => v
        }
    }
}

impl Default for EwfVolume_MediaFlagsEnum {
    fn default() -> Self { EwfVolume_MediaFlagsEnum::Unknown(0) }
}

#[derive(Debug, PartialEq, Clone)]
pub enum EwfVolume_CompressionLevelEnum {
    False,
    Good,
    Best,
    Unknown(i64),
}

impl TryFrom<i64> for EwfVolume_CompressionLevelEnum {
    type Error = KError;
    fn try_from(flag: i64) -> KResult<EwfVolume_CompressionLevelEnum> {
        match flag {
            0 => Ok(EwfVolume_CompressionLevelEnum::False),
            1 => Ok(EwfVolume_CompressionLevelEnum::Good),
            2 => Ok(EwfVolume_CompressionLevelEnum::Best),
            _ => Ok(EwfVolume_CompressionLevelEnum::Unknown(flag)),
        }
    }
}

impl From<&EwfVolume_CompressionLevelEnum> for i64 {
    fn from(v: &EwfVolume_CompressionLevelEnum) -> Self {
        match *v {
            EwfVolume_CompressionLevelEnum::False => 0,
            EwfVolume_CompressionLevelEnum::Good => 1,
            EwfVolume_CompressionLevelEnum::Best => 2,
            EwfVolume_CompressionLevelEnum::Unknown(v) => v
        }
    }
}

impl Default for EwfVolume_CompressionLevelEnum {
    fn default() -> Self { EwfVolume_CompressionLevelEnum::Unknown(0) }
}
