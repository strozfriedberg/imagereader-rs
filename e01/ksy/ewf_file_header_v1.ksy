meta:
  id: ewf_file_header_v1
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#211-ewf-ewf-e01-and-ewf-s01'
seq:
  - id: signature
# EVF contents: "EVF\x09\x0d\x0a\xff\x00"
# LVF contents: "LVF\x09\x0d\x0a\xff\x00"
    size: 8
  - id: fields_start
    contents: [ 0x1 ]
  - id: segment_number
    type: u2
  - id: fields_end
    contents: [ 0x0, 0x0 ]
