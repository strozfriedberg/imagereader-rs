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

#include "vmdkrs.h"

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
  const char* path = argc > 1 ? argv[1] : "../data/monolithicSparse.vmdk";

  VmdkError* err = NULL;

  /* A good open populates the handle and leaves err alone. */
  VmdkHandle* reader = vmdk_open(path, &err);
  if (!reader) {
    fprintf(stderr, "open %s failed: %s\n", path, err ? err->message : "(none)");
    if (err) {
      vmdk_free_error(err);
    }
    return 1;
  }
  CHECK(err == NULL);
  CHECK(reader->image_size > 0);
  CHECK(reader->image_path != NULL && reader->image_path[0] != '\0');

  /* A read of a whole sector at the start of the image returns all of it. */
  char buf[512];
  size_t n = vmdk_read(reader, 0, buf, sizeof buf, &err);
  CHECK(n == sizeof buf);
  CHECK(err == NULL);
  if (err) {
    fprintf(stderr, "read failed: %s\n", err->message);
    vmdk_free_error(err);
    err = NULL;
  }

  /* A failed open reports through err rather than crossing the boundary as a
   * panic, and the message is ours to free. */
  VmdkHandle* missing = vmdk_open("/nonexistent/not-an-image", &err);
  CHECK(missing == NULL);
  CHECK(err != NULL);
  if (err) {
    CHECK(err->message != NULL);
    vmdk_free_error(err);
    err = NULL;
  }

  /* Null arguments are rejected, not dereferenced. */
  CHECK(vmdk_open(NULL, &err) == NULL);
  CHECK(err != NULL);
  if (err) {
    vmdk_free_error(err);
    err = NULL;
  }
  CHECK(vmdk_read(NULL, 0, buf, sizeof buf, &err) == 0);
  CHECK(err != NULL);
  if (err) {
    vmdk_free_error(err);
    err = NULL;
  }

  vmdk_close(reader);
  vmdk_close(NULL); /* closing null is a no-op */

  if (failures) {
    fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
  }
  return 0;
}
