meta:
  id: ewf_section_descriptor_v1
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#31-section-descriptor'
seq:
  - id: type_string
    size: 16
    type: str
    encoding: ASCII
  - id: next_offset
    type: u8
  - id: size
    type: u8
  - id: padding
    size: 40
  - id: checksum
    type: u4
