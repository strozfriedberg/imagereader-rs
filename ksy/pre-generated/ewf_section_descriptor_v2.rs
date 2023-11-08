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
pub struct EwfSectionDescriptorV2 {
    pub _root: SharedType<EwfSectionDescriptorV2>,
    pub _parent: SharedType<EwfSectionDescriptorV2>,
    pub _self: SharedType<Self>,
    type_num: RefCell<u32>,
    data_flags: RefCell<u32>,
    previous_offset: RefCell<u64>,
    data_size: RefCell<u64>,
    descriptor_size: RefCell<u32>,
    padding_size: RefCell<u32>,
    data_integrity_hash: RefCell<Vec<u8>>,
    padding: RefCell<Vec<u8>>,
    checksum: RefCell<u32>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfSectionDescriptorV2 {
    type Root = EwfSectionDescriptorV2;
    type Parent = EwfSectionDescriptorV2;

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
        *self_rc.type_num.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.data_flags.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.previous_offset.borrow_mut() = _io.read_u8le()?.into();
        *self_rc.data_size.borrow_mut() = _io.read_u8le()?.into();
        *self_rc.descriptor_size.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.padding_size.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.data_integrity_hash.borrow_mut() = _io.read_bytes(16 as usize)?.into();
        *self_rc.padding.borrow_mut() = _io.read_bytes(12 as usize)?.into();
        *self_rc.checksum.borrow_mut() = _io.read_u4le()?.into();
        Ok(())
    }
}
impl EwfSectionDescriptorV2 {
}
impl EwfSectionDescriptorV2 {
    pub fn type_num(&self) -> Ref<u32> {
        self.type_num.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn data_flags(&self) -> Ref<u32> {
        self.data_flags.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn previous_offset(&self) -> Ref<u64> {
        self.previous_offset.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn data_size(&self) -> Ref<u64> {
        self.data_size.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn descriptor_size(&self) -> Ref<u32> {
        self.descriptor_size.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn padding_size(&self) -> Ref<u32> {
        self.padding_size.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn data_integrity_hash(&self) -> Ref<Vec<u8>> {
        self.data_integrity_hash.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn padding(&self) -> Ref<Vec<u8>> {
        self.padding.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn checksum(&self) -> Ref<u32> {
        self.checksum.borrow()
    }
}
impl EwfSectionDescriptorV2 {
    pub fn _io(&self) -> Ref<BytesReader> {
        self._io.borrow()
    }
}