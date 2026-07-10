meta:
  id: ewf_volume_smart
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#351-ewf-specification'
seq:
  - id: unknown1
    type: u4
  - id: number_of_chunks
    type: u4
  - id: sectors_per_chunk
    type: u4
  - id: bytes_per_sector
    type: u4
  - id: number_of_sectors
    type: u4
  - id: unknown2
    size: 20
  - id: unknown3
    size: 45
  - id: signature
    size: 5
  - id: checksum
    type: u4
