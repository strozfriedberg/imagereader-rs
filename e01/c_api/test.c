/*
 * Smoke test for the C API: proves the generated header compiles as both C99
 * and C++17 and that the ABI works end to end from a real C caller. The Rust
 * `#[cfg(test)]` tests in src/capi.rs cover the same functions in depth, but
 * they call them from Rust and so never link the library the way a consumer
 * does.
 *
 * Built and run for every capi crate by scripts/ctest-capi.sh; see that script
 * for the compiler invocations.
 */

#include "e01.h"

#include <stdio.h>

static int failures = 0;

#define CHECK(cond)                                                           \
  do {                                                                        \
    if (!(cond)) {                                                            \
      fprintf(stderr, "%s:%d: FAILED: %s\n", __FILE__, __LINE__, #cond);      \
      failures += 1;                                                          \
    }                                                                         \
  } while (0)

int main(int argc, char** argv) {
  const char* path = argc > 1 ? argv[1] : "../data/image.E01";

  const E01ReaderOptions opts = {CSP_ERROR, CCP_ERROR};

  E01Error* err = NULL;

  /* A good open populates the handle and leaves err alone. */
  E01Handle* reader = e01_open_glob(path, &opts, &err);
  if (!reader) {
    fprintf(stderr, "open %s failed: %s\n", path, err ? err->message : "(none)");
    if (err) {
      e01_free_error(err);
    }
    return 1;
  }
  CHECK(err == NULL);
  CHECK(reader->image_size > 0);
  CHECK(reader->sector_size > 0);
  CHECK(reader->sector_count > 0);
  CHECK(reader->chunk_size > 0);
  CHECK(reader->chunk_count > 0);
  CHECK(reader->image_size == (uint64_t)reader->sector_size * reader->sector_count);

  /* Segment discovery found at least the segment we named. */
  CHECK(reader->segment_paths_count > 0);
  CHECK(reader->segment_paths != NULL);
  if (reader->segment_paths && reader->segment_paths_count) {
    CHECK(reader->segment_paths[0] != NULL);
    CHECK(reader->segment_paths[0][0] != '\0');
  }

  /* A read of a whole sector at the start of the image returns all of it. */
  char buf[512];
  size_t n = e01_read(reader, 0, buf, sizeof buf, &err);
  CHECK(n == sizeof buf);
  CHECK(err == NULL);
  if (err) {
    fprintf(stderr, "read failed: %s\n", err->message);
    e01_free_error(err);
    err = NULL;
  }

  /* A failed open reports through err rather than crossing the boundary as a
   * panic, and the message is ours to free. */
  E01Handle* missing = e01_open_glob("/nonexistent/not-an-image.E01", &opts, &err);
  CHECK(missing == NULL);
  CHECK(err != NULL);
  if (err) {
    CHECK(err->message != NULL);
    e01_free_error(err);
    err = NULL;
  }

  /* Null arguments are rejected, not dereferenced. */
  CHECK(e01_open_glob(NULL, &opts, &err) == NULL);
  CHECK(err != NULL);
  if (err) {
    e01_free_error(err);
    err = NULL;
  }
  CHECK(e01_open_glob(path, NULL, &err) == NULL); /* options is required */
  CHECK(err != NULL);
  if (err) {
    e01_free_error(err);
    err = NULL;
  }
  CHECK(e01_read(NULL, 0, buf, sizeof buf, &err) == 0);
  CHECK(err != NULL);
  if (err) {
    e01_free_error(err);
    err = NULL;
  }

  e01_close(reader);
  e01_close(NULL); /* closing null is a no-op */

  if (failures) {
    fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
  }
  return 0;
}
