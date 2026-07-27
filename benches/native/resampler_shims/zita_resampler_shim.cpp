#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <limits>
#include <string>
#include <vector>

#include <zita-resampler/resampler.h>

namespace {

thread_local std::string global_error;

struct ZitaState {
  Resampler resampler;
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint32_t max_input_frames = 0;
  std::uint32_t latency_frames = 0;
  std::uint32_t tail_input_remaining = 0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool finished = false;
  std::vector<float> scratch;
  std::string error;
};

void set_error(ZitaState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

std::uint64_t target_output(const ZitaState* state) {
  return aeb_round_output_frames(state->total_input, state->input_rate,
                                 state->output_rate);
}

int prime(ZitaState* state) {
  if (state->resampler.reset() != 0) {
    set_error(state, "zita reset failed during stream priming");
    return -1;
  }
  std::uint32_t remaining = static_cast<std::uint32_t>(
      std::max(0, state->resampler.inpsize() / 2 - 1));
  while (remaining > 0) {
    state->resampler.inp_count = remaining;
    state->resampler.inp_data = nullptr;
    state->resampler.out_count = state->max_input_frames + 1024;
    state->resampler.out_data = state->scratch.data();
    const auto before = state->resampler.inp_count;
    if (state->resampler.process() != 0) {
      set_error(state, "zita process failed during stream priming");
      return -2;
    }
    remaining = state->resampler.inp_count;
    if (remaining == before) {
      set_error(state, "zita stream priming made no progress");
      return -3;
    }
  }
  return 0;
}

}  // namespace

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() { return "zita_resampler"; }
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "zita-resampler 1.11.2";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "official release 1.11.2; archive sha256 AA5C54E696069AF26F3F1FED4A963113CC1237CDDFD57AE5842ABCB1ACD5492C";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC 15.2.0 -O3 -DNDEBUG -DENABLE_SSE2 -msse3; source-integrated shared release shim";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "native Resampler::setup/process/reset interleaved float shim";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "zita Resampler hlen=96 default cutoff; author file-mode half-filter pre/post zero policy; interleaved float stereo";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "linear phase; half-filter input latency reported; file-mode centered stream alignment";
}
AEB_EXPORT int aeb_resampler_sample_format() {
  return AEB_SAMPLE_FORMAT_F32;
}
AEB_EXPORT std::uint32_t aeb_resampler_dependency_count() { return 1; }
AEB_EXPORT const char* aeb_resampler_dependency_path(std::uint32_t index) {
  return index == 0 ? "libwinpthread-1.dll" : nullptr;
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
    set_error(nullptr, "zita shim requires non-zero geometry");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new ZitaState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    state->max_input_frames = max_input_frames;
    state->scratch.assign(
        static_cast<std::size_t>(max_input_frames + 1024) * channels, 0.0F);
    if (state->resampler.setup(input_rate, output_rate, channels, 96) != 0) {
      global_error = "zita Resampler::setup rejected canonical geometry";
      delete state;
      if (error != nullptr) {
        *error = -2;
      }
      return nullptr;
    }
    const std::uint32_t half_filter =
        static_cast<std::uint32_t>(state->resampler.inpsize() / 2);
    state->latency_frames = static_cast<std::uint32_t>(
        aeb_round_output_frames(half_filter, input_rate, output_rate));
    const int result = prime(state);
    if (result != 0) {
      global_error = state->error;
      delete state;
      if (error != nullptr) {
        *error = result;
      }
      return nullptr;
    }
    return state;
  } catch (const std::exception& exception) {
    set_error(nullptr, exception.what());
  } catch (...) {
    set_error(nullptr, "unknown exception during zita shim construction");
  }
  if (error != nullptr) {
    *error = -3;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  delete static_cast<ZitaState*>(opaque);
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(void* opaque,
                                                         std::uint32_t input_frames) {
  auto* state = static_cast<ZitaState*>(opaque);
  if (state == nullptr) {
    return 0;
  }
  const std::uint64_t nominal =
      aeb_round_output_frames(input_frames, state->input_rate, state->output_rate);
  return static_cast<std::uint32_t>(std::min<std::uint64_t>(
      nominal + state->latency_frames + 1024,
      std::numeric_limits<std::uint32_t>::max()));
}

AEB_EXPORT std::uint32_t aeb_resampler_latency_frames(void* opaque) {
  auto* state = static_cast<ZitaState*>(opaque);
  return state == nullptr ? 0 : state->latency_frames;
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<ZitaState*>(opaque);
  return state == nullptr
             ? 0
             : aeb_round_output_frames(input_frames, state->input_rate,
                                       state->output_rate);
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
  auto* state = static_cast<ZitaState*>(opaque);
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
      set_error(state, "zita shim received invalid input lifecycle or size");
      return -2;
    }
    state->resampler.inp_count = input_frames;
    state->resampler.inp_data = const_cast<float*>(static_cast<const float*>(input));
    state->resampler.out_count = output_capacity_frames;
    state->resampler.out_data = static_cast<float*>(output);
    if (state->resampler.process() != 0) {
      set_error(state, "zita Resampler::process failed");
      return -3;
    }
    *consumed_frames = input_frames - state->resampler.inp_count;
    *produced_frames = output_capacity_frames - state->resampler.out_count;
    state->total_input += *consumed_frames;
    state->total_output += *produced_frames;
    state->ended = end_of_input != 0;
    if (state->ended) {
      state->tail_input_remaining =
          static_cast<std::uint32_t>(state->resampler.inpsize() / 2);
    }
    return 0;
  }
  if (!state->ended || end_of_input == 0) {
    set_error(state, "zita drain called before end-of-input");
    return -4;
  }
  const std::uint64_t target = target_output(state);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  state->resampler.inp_count = state->tail_input_remaining;
  state->resampler.inp_data = nullptr;
  state->resampler.out_count = static_cast<std::uint32_t>(
      std::min<std::uint64_t>(output_capacity_frames,
                              target - state->total_output));
  const std::uint32_t offered_output = state->resampler.out_count;
  state->resampler.out_data = static_cast<float*>(output);
  if (state->resampler.process() != 0) {
    set_error(state, "zita Resampler::process failed during drain");
    return -5;
  }
  state->tail_input_remaining = state->resampler.inp_count;
  *produced_frames = offered_output - state->resampler.out_count;
  state->total_output += *produced_frames;
  if (state->total_output >= target ||
      (state->tail_input_remaining == 0 && *produced_frames == 0)) {
    state->finished = true;
    *finished = 1;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "zita process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<ZitaState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  const int result = prime(state);
  if (result != 0) {
    return result;
  }
  state->tail_input_remaining = 0;
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->finished = false;
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "zita reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<ZitaState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
