// Smoke test for the C API. Build the library with cargo-c first:
//
//   cargo cbuild -p e01-rs --features capi
//
// then, from this directory (adjust the target dir to match cargo's output):
//
//   g++ -Wall -g -I../../target/<triple>/debug/include/e01 test.cpp -o test -L../../target/<triple>/debug -le01
//   LD_LIBRARY_PATH=../../target/<triple>/debug ./test

#include "e01.h"

#include <iostream>
#include <memory>

int main(int argc, char** argv) {
  const char* path = argc > 1 ? argv[1] : "../data/image.E01";

  const E01ReaderOptions opts{CSP_ERROR, CCP_ERROR};

  E01Error* err = nullptr;

  std::unique_ptr<E01Handle, decltype(&e01_close)> reader{
    e01_open_glob(path, &opts, &err),
    e01_close
  };

  if (err) {
    std::cerr << "error: " << err->message << std::endl;
    e01_free_error(err);
    return 1;
  }

  std::cout << "size == " << reader->image_size << std::endl;

  return 0;
}
