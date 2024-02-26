meta:
  id: ewf_hash_section
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#318-hash-section'
seq:
  - id: md5
    size: 16
  - id: unknown
    size: 16
  - id: checksum
    type: u4
