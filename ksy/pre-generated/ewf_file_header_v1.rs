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
 * \sa https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#211-ewf-ewf-e01-and-ewf-s01 Source
 */

#[derive(Default, Debug, Clone)]
pub struct EwfFileHeaderV1 {
    pub _root: SharedType<EwfFileHeaderV1>,
    pub _parent: SharedType<EwfFileHeaderV1>,
    pub _self: SharedType<Self>,
    signature: RefCell<Vec<u8>>,
    fields_start: RefCell<Vec<u8>>,
    segment_number: RefCell<u16>,
    fields_end: RefCell<Vec<u8>>,
    _io: RefCell<BytesReader>,
}
impl KStruct for EwfFileHeaderV1 {
    type Root = EwfFileHeaderV1;
    type Parent = EwfFileHeaderV1;

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
        *self_rc.fields_start.borrow_mut() = _io.read_bytes(1 as usize)?.into();
        if !(*self_rc.fields_start() == vec![0x1u8]) {
            return Err(KError::ValidationFailed(ValidationFailedError { kind: ValidationKind::NotEqual, src_path: r#"vec![0x1u8], *self_rc.fields_start(), _io, "/seq/1""#.to_string()}));
        }
        *self_rc.segment_number.borrow_mut() = _io.read_u2le()?.into();
        *self_rc.fields_end.borrow_mut() = _io.read_bytes(2 as usize)?.into();
        if !(*self_rc.fields_end() == vec![0x0u8, 0x0u8]) {
            return Err(KError::ValidationFailed(ValidationFailedError { kind: ValidationKind::NotEqual, src_path: r#"vec![0x0u8, 0x0u8], *self_rc.fields_end(), _io, "/seq/3""#.to_string()}));
        }
        Ok(())
    }
}
impl EwfFileHeaderV1 {
}
impl EwfFileHeaderV1 {
    pub fn signature(&self) -> Ref<Vec<u8>> {
        self.signature.borrow()
    }
}
impl EwfFileHeaderV1 {
    pub fn fields_start(&self) -> Ref<Vec<u8>> {
        self.fields_start.borrow()
    }
}
impl EwfFileHeaderV1 {
    pub fn segment_number(&self) -> Ref<u16> {
        self.segment_number.borrow()
    }
}
impl EwfFileHeaderV1 {
    pub fn fields_end(&self) -> Ref<Vec<u8>> {
        self.fields_end.borrow()
    }
}
impl EwfFileHeaderV1 {
    pub fn _io(&self) -> Ref<BytesReader> {
        self._io.borrow()
    }
}
