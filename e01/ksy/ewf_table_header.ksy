meta:
  id: ewf_table_header
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#table-header-3'
seq:
  - id: entry_count
    type: u4
  - id: padding1
    size: 4
  - id: table_base_offset
    type: u8
  - id: padding2
    size: 4
  - id: checksum
    type: u4
