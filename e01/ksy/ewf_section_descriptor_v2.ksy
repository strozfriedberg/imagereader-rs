meta:
  id: ewf_section_descriptor_v2
  endian: le
seq:
  - id: type_num
    type: u4
  - id: data_flags
    type: u4
  - id: previous_offset
    type: u8
  - id: data_size
    type: u8
  - id: descriptor_size
    type: u4
  - id: padding_size
    type: u4
  - id: data_integrity_hash
    size: 16
  - id: padding
    size: 12
  - id: checksum
    type: u4

