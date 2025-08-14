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
 * \sa https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#31-section-descriptor Source
 */

#[derive(Default, Debug, Clone)]
pub struct EwfSectionDescriptorV1 {
    pub _root: SharedType<EwfSectionDescriptorV1>,
    pub _parent: SharedType<EwfSectionDescriptorV1>,
    pub _self: SharedType<Self>,
    type_string: RefCell<String>,
    next_offset: RefCell<u64>,
    size: RefCell<u64>,
    padding: RefCell<Vec<u8>>,
    checksum: RefCell<u32>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfSectionDescriptorV1 {
    type Root = EwfSectionDescriptorV1;
    type Parent = EwfSectionDescriptorV1;

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
        *self_rc.type_string.borrow_mut() = bytes_to_str(&_io.read_bytes(16 as usize)?.into(), &"ASCII")?;
        *self_rc.next_offset.borrow_mut() = _io.read_u8le()?.into();
        *self_rc.size.borrow_mut() = _io.read_u8le()?.into();
        *self_rc.padding.borrow_mut() = _io.read_bytes(40 as usize)?.into();
        *self_rc.checksum.borrow_mut() = _io.read_u4le()?.into();
        Ok(())
    }
}
impl EwfSectionDescriptorV1 {
}
impl EwfSectionDescriptorV1 {
    pub fn type_string(&self) -> Ref<String> {
        self.type_string.borrow()
    }
}
impl EwfSectionDescriptorV1 {
    pub fn next_offset(&self) -> Ref<u64> {
        self.next_offset.borrow()
    }
}
impl EwfSectionDescriptorV1 {
    pub fn size(&self) -> Ref<u64> {
        self.size.borrow()
    }
}
impl EwfSectionDescriptorV1 {
    pub fn padding(&self) -> Ref<Vec<u8>> {
        self.padding.borrow()
    }
}
impl EwfSectionDescriptorV1 {
    pub fn checksum(&self) -> Ref<u32> {
        self.checksum.borrow()
    }
}
impl EwfSectionDescriptorV1 {
    pub fn _io(&self) -> Ref<BytesReader> {
        self._io.borrow()
    }
}
