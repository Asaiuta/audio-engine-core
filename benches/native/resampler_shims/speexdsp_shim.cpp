#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <limits>
#include <new>
#include <string>
#include <vector>

#include <speex/speex_resampler.h>

namespace {

thread_local std::string global_error;

struct SpeexState {
  SpeexResamplerState* resampler = nullptr;
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint32_t max_input_frames = 0;
  std::uint32_t output_latency = 0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool finished = false;
  std::vector<float> zeros;
  std::string error;
};

void set_error(SpeexState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

std::string speex_error(const char* operation, int code) {
  const char* message = speex_resampler_strerror(code);
  return std::string(operation) + " failed with SpeexDSP error " +
         std::to_string(code) + (message == nullptr ? "" : ": ") +
         (message == nullptr ? "" : message);
}

std::uint64_t target_output(const SpeexState* state) {
  return aeb_round_output_frames(state->total_input, state->input_rate,
                                 state->output_rate) +
         state->output_latency;
}

SpeexResamplerState* create_native_resampler(const SpeexState* state,
                                             int* result) {
  *result = RESAMPLER_ERR_SUCCESS;
  return speex_resampler_init(state->channels, state->input_rate,
                              state->output_rate, 10, result);
}

}  // namespace

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() { return "speexdsp"; }
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "SpeexDSP 1.2.1";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "MSYS2 mingw-w64-x86_64-speexdsp-1.2.1-1";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC 15.2.0 -O3 -DNDEBUG; MSYS2 package sha256=E46B80E43DB1436F9469FB9500FEB1A0D3879E63B35703BABA6F46AF4949A4C8; linked runtime payload is hashed in report";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "native speex_resampler_process_interleaved_float streaming shim; fresh-state reset";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "SpeexDSP quality=10; interleaved float stereo; natural leading latency retained; zero-tail drain to nominal plus output latency";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "SpeexDSP quality-10 sinc; API output latency retained and reported";
}
AEB_EXPORT int aeb_resampler_sample_format() {
  return AEB_SAMPLE_FORMAT_F32;
}
AEB_EXPORT std::uint32_t aeb_resampler_dependency_count() { return 2; }
AEB_EXPORT const char* aeb_resampler_dependency_path(std::uint32_t index) {
  if (index == 0) {
    return "libspeexdsp-1.dll";
  }
  if (index == 1) {
    return "libwinpthread-1.dll";
  }
  return nullptr;
}

AEB_EXPORT void* aeb_resampler_create(std::uint32_t input_rate,
                                      std::uint32_t output_rate,
                                      std::uint32_t channels,
                                      std::uint32_t max_input_frames,
                                      int* error) {
  if (error != nullptr) {
    *error = 0;
  }
  if (input_rate == 0 || output_rate == 0 || channels == 0 ||
      max_input_frames == 0) {
    set_error(nullptr, "SpeexDSP shim requires non-zero geometry");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new SpeexState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    state->max_input_frames = max_input_frames;
    state->zeros.assign(static_cast<std::size_t>(max_input_frames) * channels,
                        0.0F);
    int result = RESAMPLER_ERR_SUCCESS;
    state->resampler = create_native_resampler(state, &result);
    if (state->resampler == nullptr || result != RESAMPLER_ERR_SUCCESS) {
      global_error = speex_error("speex_resampler_init", result);
      if (state->resampler != nullptr) {
        speex_resampler_destroy(state->resampler);
      }
      delete state;
      if (error != nullptr) {
        *error = result == 0 ? -2 : result;
      }
      return nullptr;
    }
    state->output_latency = static_cast<std::uint32_t>(
        std::max(0, speex_resampler_get_output_latency(state->resampler)));
    return state;
  } catch (const std::exception& exception) {
    set_error(nullptr, exception.what());
  } catch (...) {
    set_error(nullptr, "unknown exception during SpeexDSP shim construction");
  }
  if (error != nullptr) {
    *error = -3;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  auto* state = static_cast<SpeexState*>(opaque);
  if (state != nullptr) {
    if (state->resampler != nullptr) {
      speex_resampler_destroy(state->resampler);
    }
    delete state;
  }
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(void* opaque,
                                                         std::uint32_t input_frames) {
  auto* state = static_cast<SpeexState*>(opaque);
  if (state == nullptr) {
    return 0;
  }
  const std::uint64_t nominal =
      aeb_round_output_frames(input_frames, state->input_rate, state->output_rate);
  return static_cast<std::uint32_t>(std::min<std::uint64_t>(
      nominal + state->output_latency + 1024,
      std::numeric_limits<std::uint32_t>::max()));
}

AEB_EXPORT std::uint32_t aeb_resampler_latency_frames(void* opaque) {
  auto* state = static_cast<SpeexState*>(opaque);
  return state == nullptr ? 0 : state->output_latency;
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<SpeexState*>(opaque);
  return state == nullptr
             ? 0
             : aeb_round_output_frames(input_frames, state->input_rate,
                                       state->output_rate) +
                   state->output_latency;
}

AEB_EXPORT int aeb_resampler_process(void* opaque,
                                     const void* input,
                                     std::uint32_t input_frames,
                                     void* output,
                                     std::uint32_t output_capacity_frames,
                                     int end_of_input,
                                     std::uint32_t* consumed_frames,
                                     std::uint32_t* produced_frames,
                                     int* finished) noexcept {
  auto* state = static_cast<SpeexState*>(opaque);
  try {
  if (state == nullptr || output == nullptr || consumed_frames == nullptr ||
      produced_frames == nullptr || finished == nullptr) {
    return -1;
  }
  *consumed_frames = 0;
  *produced_frames = 0;
  *finished = state->finished ? 1 : 0;
  if (state->finished) {
    return 0;
  }
  if (input_frames > 0) {
    if (input == nullptr || state->ended || input_frames > state->max_input_frames) {
      set_error(state, "SpeexDSP shim received invalid input lifecycle or size");
      return -2;
    }
    spx_uint32_t native_input = input_frames;
    spx_uint32_t native_output = output_capacity_frames;
    const int result = speex_resampler_process_interleaved_float(
        state->resampler, static_cast<const float*>(input), &native_input,
        static_cast<float*>(output), &native_output);
    if (result != RESAMPLER_ERR_SUCCESS) {
      set_error(state, speex_error("speex_resampler_process", result));
      return result;
    }
    *consumed_frames = native_input;
    *produced_frames = native_output;
    state->total_input += native_input;
    state->total_output += native_output;
    state->ended = end_of_input != 0;
    return 0;
  }

  if (!state->ended || end_of_input == 0) {
    set_error(state, "SpeexDSP drain called before end-of-input");
    return -3;
  }
  const std::uint64_t target = target_output(state);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  const std::uint32_t remaining = static_cast<std::uint32_t>(
      std::min<std::uint64_t>(target - state->total_output,
                              output_capacity_frames));
  spx_uint32_t zero_input = state->max_input_frames;
  spx_uint32_t native_output = remaining;
  const int result = speex_resampler_process_interleaved_float(
      state->resampler, state->zeros.data(), &zero_input,
      static_cast<float*>(output), &native_output);
  if (result != RESAMPLER_ERR_SUCCESS) {
    set_error(state, speex_error("speex_resampler_process(drain)", result));
    return result;
  }
  *produced_frames = native_output;
  state->total_output += native_output;
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "SpeexDSP process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<SpeexState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  int result = RESAMPLER_ERR_SUCCESS;
  SpeexResamplerState* replacement = create_native_resampler(state, &result);
  if (replacement == nullptr || result != RESAMPLER_ERR_SUCCESS) {
    if (replacement != nullptr) {
      speex_resampler_destroy(replacement);
    }
    set_error(state, speex_error("speex_resampler_init(reset)", result));
    return result == RESAMPLER_ERR_SUCCESS ? -2 : result;
  }
  const auto replacement_latency = static_cast<std::uint32_t>(
      std::max(0, speex_resampler_get_output_latency(replacement)));
  speex_resampler_destroy(state->resampler);
  state->resampler = replacement;
  state->output_latency = replacement_latency;
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->finished = false;
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "SpeexDSP reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<SpeexState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
