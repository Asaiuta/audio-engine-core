#ifndef AUDIO_BENCH_ZITA_MINGW_COMPAT_H
#define AUDIO_BENCH_ZITA_MINGW_COMPAT_H

#if defined(_WIN32)
#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdlib>

inline int aeb_zita_posix_memalign(void** pointer,
                                   std::size_t alignment,
                                   std::size_t size) {
  if (pointer == nullptr || alignment < sizeof(void*) ||
      (alignment & (alignment - 1)) != 0) {
    return EINVAL;
  }
  void* allocation = std::malloc(size);
  if (allocation == nullptr) {
    return ENOMEM;
  }
  if ((reinterpret_cast<std::uintptr_t>(allocation) & (alignment - 1)) != 0) {
    std::free(allocation);
    return ENOMEM;
  }
  *pointer = allocation;
  return 0;
}

#define posix_memalign aeb_zita_posix_memalign
#endif

#ifndef M_PI
#define M_PI 3.14159265358979323846264338327950288
#endif

#endif
