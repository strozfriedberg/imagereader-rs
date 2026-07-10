// g++ -Wall -g -I. -c -o test.o test.cpp
// g++ -Wall -g -o test test.o -lstdc++ -L../target/debug -le01

#include "c_api.h"

#include <iostream>
#include <memory>

int main(int argc, char** argv) {
  const E01ReaderOptions opts{
    CorruptSectionPolicy_ERROR,
    CorruptChunkPolicy_ERROR
  };

  E01Error* err = nullptr;

  std::unique_ptr<E01Reader, decltype(&e01_close)> reader{
    e01_open_glob("../data/image.E01", &opts, &err),
    e01_close
  };

  if (err) {
    std::cerr << "error: " << err->message << std::endl;
    return 1;
  }

  std::cout << "size == " << e01_total_size(reader.get()) << std::endl;

  return 0;
}
