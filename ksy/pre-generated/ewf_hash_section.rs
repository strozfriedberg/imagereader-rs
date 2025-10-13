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
 * \sa https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#318-hash-section Source
 */

#[derive(Default, Debug, Clone)]
pub struct EwfHashSection {
    pub _root: SharedType<EwfHashSection>,
    pub _parent: SharedType<EwfHashSection>,
    pub _self: SharedType<Self>,
    md5: RefCell<Vec<u8>>,
    unknown: RefCell<Vec<u8>>,
    checksum: RefCell<u32>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfHashSection {
    type Root = EwfHashSection;
    type Parent = EwfHashSection;

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
        *self_rc.md5.borrow_mut() = _io.read_bytes(16 as usize)?.into();
        *self_rc.unknown.borrow_mut() = _io.read_bytes(16 as usize)?.into();
        *self_rc.checksum.borrow_mut() = _io.read_u4le()?.into();
        Ok(())
    }
}
impl EwfHashSection {
}
impl EwfHashSection {
    pub fn md5(&self) -> Ref<'_, Vec<u8>> {
        self.md5.borrow()
    }
}
impl EwfHashSection {
    pub fn unknown(&self) -> Ref<'_, Vec<u8>> {
        self.unknown.borrow()
    }
}
impl EwfHashSection {
    pub fn checksum(&self) -> Ref<'_, u32> {
        self.checksum.borrow()
    }
}
impl EwfHashSection {
    pub fn _io(&self) -> Ref<'_, BytesReader> {
        self._io.borrow()
    }
}
