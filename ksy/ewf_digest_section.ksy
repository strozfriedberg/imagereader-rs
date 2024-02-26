meta:
  id: ewf_digest_section
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#317-digest-section'
seq:
  - id: md5
    size: 16
  - id: sha1
    size: 20
  - id: padding
    size: 40
  - id: checksum
    type: u4
