#ifndef C_API_H_
#define C_API_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdint.h>

struct E01Reader;

struct E01Error {
  char* message;
};

void e01_free_error(E01Error* err);

enum CorruptSectionPolicy {
  CorruptSectionPolicy_ERROR,
  CorruptSectionPolicy_DAMN_THE_TORPEDOES
};

enum CorruptChunkPolicy {
  CorruptChunkPolicy_ERROR,
  CorruptChunkPolicy_ZERO,
  CorruptChunkPolicy_RAW_IF_POSSIBLE
};

struct E01ReaderOptions {
  enum CorruptSectionPolicy corrupt_section_policy;
  enum CorruptChunkPolicy corrupt_chunk_policy;
};

E01Reader* e01_open(
  const char* const* segment_paths,
  size_t segment_paths_len,
  const E01ReaderOptions* options,
  E01Error** err
);

E01Reader* e01_open_glob(
  const char* example_segment_path,
  const E01ReaderOptions* options,
  E01Error** err
);

void e01_close(E01Reader* reader); 

size_t e01_read(
  E01Reader* reader,
  uint64_t offset,
  char* buf,
  size_t buflen,
  E01Error** err
);

size_t e01_chunk_size(const E01Reader* reader);

size_t e01_total_size(const E01Reader* reader);

const uint8_t* e01_stored_md5(const E01Reader* reader);

const uint8_t* e01_stored_sha1(const E01Reader* reader);

#ifdef __cplusplus
}
#endif

#endif /* C_API_H_ */
