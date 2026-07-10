meta:
  id: ewf_file_header_v2
  endian: le
seq:
  - id: signature
# EVF contents: "EVF2\x0d\x0a\x81\x00"
# LVF contents: "LEF2\x0d\x0a\x81\x00"
    size: 8
  - id: major_version
    type: u1
  - id: minor_version
    type: u1
  - id: compression_method
    type: u2
  - id: segment_number
    type: u2
  - id: set_identifier
    size: 16