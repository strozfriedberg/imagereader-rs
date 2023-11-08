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
 * \sa https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#table-header-3 Source
 */

#[derive(Default, Debug, Clone)]
pub struct EwfTableHeader {
    pub _root: SharedType<EwfTableHeader>,
    pub _parent: SharedType<EwfTableHeader>,
    pub _self: SharedType<Self>,
    entry_count: RefCell<u32>,
    padding1: RefCell<Vec<u8>>,
    table_base_offset: RefCell<u64>,
    padding2: RefCell<Vec<u8>>,
    checksum: RefCell<u32>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfTableHeader {
    type Root = EwfTableHeader;
    type Parent = EwfTableHeader;

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
        *self_rc.entry_count.borrow_mut() = _io.read_u4le()?.into();
        *self_rc.padding1.borrow_mut() = _io.read_bytes(4 as usize)?.into();
        *self_rc.table_base_offset.borrow_mut() = _io.read_u8le()?.into();
        *self_rc.padding2.borrow_mut() = _io.read_bytes(4 as usize)?.into();
        *self_rc.checksum.borrow_mut() = _io.read_u4le()?.into();
        Ok(())
    }
}
impl EwfTableHeader {
}
impl EwfTableHeader {
    pub fn entry_count(&self) -> Ref<u32> {
        self.entry_count.borrow()
    }
}
impl EwfTableHeader {
    pub fn padding1(&self) -> Ref<Vec<u8>> {
        self.padding1.borrow()
    }
}
impl EwfTableHeader {
    pub fn table_base_offset(&self) -> Ref<u64> {
        self.table_base_offset.borrow()
    }
}
impl EwfTableHeader {
    pub fn padding2(&self) -> Ref<Vec<u8>> {
        self.padding2.borrow()
    }
}
impl EwfTableHeader {
    pub fn checksum(&self) -> Ref<u32> {
        self.checksum.borrow()
    }
}
impl EwfTableHeader {
    pub fn _io(&self) -> Ref<BytesReader> {
        self._io.borrow()
    }
}