#ifndef AUDIO_BENCH_RESAMPLER_SHIM_H
#define AUDIO_BENCH_RESAMPLER_SHIM_H

#include <cstdint>
#include <exception>
#include <string>

#if defined(_WIN32)
#define AEB_EXPORT extern "C" __declspec(dllexport)
#else
#define AEB_EXPORT extern "C" __attribute__((visibility("default")))
#endif

constexpr std::uint32_t AEB_RESAMPLER_ABI_VERSION = 2;
constexpr std::uint32_t AEB_RESAMPLER_UNKNOWN_LATENCY = UINT32_MAX;
constexpr int AEB_SAMPLE_FORMAT_F32 = 1;
constexpr int AEB_SAMPLE_FORMAT_F64 = 2;

inline std::uint64_t aeb_round_output_frames(std::uint64_t input_frames,
                                             std::uint32_t input_rate,
                                             std::uint32_t output_rate) {
  if (input_rate == 0) {
    return 0;
  }
  const std::uint64_t whole = input_frames / input_rate;
  const std::uint64_t remainder = input_frames % input_rate;
  return whole * output_rate +
         (remainder * output_rate + input_rate / 2) / input_rate;
}

#define AEB_CATCH_STATE_EXCEPTIONS(STATE, CODE, OPERATION)                   \
  catch (const std::exception& exception) {                                 \
    try {                                                                    \
      set_error((STATE), std::string(OPERATION) + ": " + exception.what()); \
    } catch (...) {                                                          \
    }                                                                        \
    return (CODE);                                                           \
  }                                                                          \
  catch (...) {                                                              \
    try {                                                                    \
      set_error((STATE), std::string(OPERATION) + ": unknown exception");   \
    } catch (...) {                                                          \
    }                                                                        \
    return (CODE);                                                           \
  }

#endif
