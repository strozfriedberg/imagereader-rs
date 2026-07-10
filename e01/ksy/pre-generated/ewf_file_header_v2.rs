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

#[derive(Default, Debug, Clone)]
pub struct EwfFileHeaderV2 {
    pub _root: SharedType<EwfFileHeaderV2>,
    pub _parent: SharedType<EwfFileHeaderV2>,
    pub _self: SharedType<Self>,
    signature: RefCell<Vec<u8>>,
    major_version: RefCell<u8>,
    minor_version: RefCell<u8>,
    compression_method: RefCell<u16>,
    segment_number: RefCell<u16>,
    set_identifier: RefCell<Vec<u8>>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfFileHeaderV2 {
    type Root = EwfFileHeaderV2;
    type Parent = EwfFileHeaderV2;

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
        *self_rc.signature.borrow_mut() = _io.read_bytes(8 as usize)?.into();
        *self_rc.major_version.borrow_mut() = _io.read_u1()?.into();
        *self_rc.minor_version.borrow_mut() = _io.read_u1()?.into();
        *self_rc.compression_method.borrow_mut() = _io.read_u2le()?.into();
        *self_rc.segment_number.borrow_mut() = _io.read_u2le()?.into();
        *self_rc.set_identifier.borrow_mut() = _io.read_bytes(16 as usize)?.into();
        Ok(())
    }
}
impl EwfFileHeaderV2 {
}
impl EwfFileHeaderV2 {
    pub fn signature(&self) -> Ref<'_, Vec<u8>> {
        self.signature.borrow()
    }
}
impl EwfFileHeaderV2 {
    pub fn major_version(&self) -> Ref<'_, u8> {
        self.major_version.borrow()
    }
}
impl EwfFileHeaderV2 {
    pub fn minor_version(&self) -> Ref<'_, u8> {
        self.minor_version.borrow()
    }
}
impl EwfFileHeaderV2 {
    pub fn compression_method(&self) -> Ref<'_, u16> {
        self.compression_method.borrow()
    }
}
impl EwfFileHeaderV2 {
    pub fn segment_number(&self) -> Ref<'_, u16> {
        self.segment_number.borrow()
    }
}
impl EwfFileHeaderV2 {
    pub fn set_identifier(&self) -> Ref<'_, Vec<u8>> {
        self.set_identifier.borrow()
    }
}
impl EwfFileHeaderV2 {
    pub fn _io(&self) -> Ref<'_, BytesReader> {
        self._io.borrow()
    }
}
