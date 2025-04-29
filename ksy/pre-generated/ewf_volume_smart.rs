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
 * \sa https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#351-ewf-specification Source
 */

#[derive(Default, Debug, Clone)]
pub struct EwfVolumeSmart {
    pub _root: SharedType<EwfVolumeSmart>,
    pub _parent: SharedType<EwfVolumeSmart>,
    pub _self: SharedType<Self>,
    unknown1: RefCell<u32>,
    number_of_chunks: RefCell<u32>,
    sector_per_chunk: RefCell<u32>,
    bytes_per_sector: RefCell<u32>,
    number_of_sectors: RefCell<u32>,
    unknown2: RefCell<Vec<u8>>,
    unknown3: RefCell<Vec<u8>>,
    signature: RefCell<Vec<u8>>,
    checksum: RefCell<u32>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfVolumeSmart {
    type Root = EwfVolumeSmart;
    type Parent = EwfVolumeSmart;

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
        *self_rc.unknown1.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.number_of_chunks.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.sector_per_chunk.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.bytes_per_sector.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.number_of_sectors.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.unknown2.borrow_mut() = _io.read_bytes(20 as usize)?.into();
        *self_rc.unknown3.borrow_mut() = _io.read_bytes(45 as usize)?.into();
        *self_rc.signature.borrow_mut() = _io.read_bytes(5 as usize)?.into();
        *self_rc.checksum.borrow_mut() = _io.read_u4le()?.into();
        Ok(())
    }
}
impl EwfVolumeSmart {
}
impl EwfVolumeSmart {
    pub fn unknown1(&self) -> Ref<u32> {
        self.unknown1.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn number_of_chunks(&self) -> Ref<u32> {
        self.number_of_chunks.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn sector_per_chunk(&self) -> Ref<u32> {
        self.sector_per_chunk.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn bytes_per_sector(&self) -> Ref<u32> {
        self.bytes_per_sector.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn number_of_sectors(&self) -> Ref<u32> {
        self.number_of_sectors.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn unknown2(&self) -> Ref<Vec<u8>> {
        self.unknown2.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn unknown3(&self) -> Ref<Vec<u8>> {
        self.unknown3.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn signature(&self) -> Ref<Vec<u8>> {
        self.signature.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn checksum(&self) -> Ref<u32> {
        self.checksum.borrow()
    }
}
impl EwfVolumeSmart {
    pub fn _io(&self) -> Ref<BytesReader> {
        self._io.borrow()
    }
}