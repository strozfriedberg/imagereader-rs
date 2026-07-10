meta:
  id: ewf_volume
  endian: le
doc-ref: 'https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc#353-ftk-imager-encase-1-to-7-and-linen-5-to-7-ewf-e01'
seq:
  - id: media_type
    type: u1
    enum: media_types_enum
  - id: unknown1
    size: 3
  - id: number_of_chunks
    type: u4
  - id: sectors_per_chunk
    type: u4
  - id: bytes_per_sector
    type: u4
  - id: number_of_sectors
    type: u8
  - id: chs_cylinders
    type: u4
  - id: chs_heads
    type: u4
  - id: chs_sectors
    type: u4
  - id: media_flags
    type: u1
    enum: media_flags_enum
  - id: unknown2
    size: 3
  - id: palm_volume_start_sector
    type: u4
  - id: unknown3
    size: 4
  - id: smart_logs_start_sector
    type: u4
  - id: compression_level
    type: u1
    enum: compression_level_enum
  - id: unknown4
    size: 3
  - id: error_granularity
    type: u4
  - id: unknown5
    size: 4
  - id: set_identifier
    size: 16
  - id: unknown6
    size: 963
  - id: signature
    size: 5
  - id: checksum
    type: u4

enums:
  media_types_enum:
    0x00: removable
    0x01: fixed
    0x03: optical
    0x0e: single_files
    0x10: memory

  media_flags_enum:
    0x01: image
    0x02: physical
    0x04: fastbloc
    0x08: tableau

  compression_level_enum:
    0x00: no
    0x01: good
    0x02: best
