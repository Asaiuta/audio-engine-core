#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <limits>
#include <string>
#include <vector>

#include <libresample.h>

namespace {

thread_local std::string global_error;

struct LibresampleState {
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint32_t max_input_frames = 0;
  std::uint32_t filter_width = 0;
  double ratio = 0.0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool finished = false;
  std::vector<void*> resamplers;
  std::vector<std::vector<float>> channel_input;
  std::vector<std::vector<float>> channel_output;
  std::vector<float> zeros;
  std::string error;
};

void set_error(LibresampleState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

void close_resamplers(LibresampleState* state) {
  for (void* resampler : state->resamplers) {
    if (resampler != nullptr) {
      resample_close(resampler);
    }
  }
  state->resamplers.clear();
}

bool open_resamplers(LibresampleState* state) {
  close_resamplers(state);
  for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
    void* resampler = resample_open(1, state->ratio, state->ratio);
    if (resampler == nullptr) {
      set_error(state, "libresample resample_open failed");
      close_resamplers(state);
      return false;
    }
    state->resamplers.push_back(resampler);
  }
  state->filter_width = static_cast<std::uint32_t>(
      std::max(0, resample_get_filter_width(state->resamplers.front())));
  return true;
}

std::uint64_t target_output(const LibresampleState* state) {
  return aeb_round_output_frames(state->total_input, state->input_rate,
                                 state->output_rate);
}

int process_channels(LibresampleState* state,
                     const float* input,
                     std::uint32_t input_frames,
                     float* output,
                     std::uint32_t output_capacity_frames,
                     bool final,
                     std::uint32_t* consumed_frames,
                     std::uint32_t* produced_frames) {
  for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
    auto& channel_input = state->channel_input[channel];
    for (std::uint32_t frame = 0; frame < input_frames; ++frame) {
      channel_input[frame] =
          input[static_cast<std::size_t>(frame) * state->channels + channel];
    }
  }

  int common_consumed = -1;
  int common_produced = -1;
  for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
    int used = 0;
    const int produced = resample_process(
        state->resamplers[channel], state->ratio,
        state->channel_input[channel].data(), static_cast<int>(input_frames),
        final ? 1 : 0, &used, state->channel_output[channel].data(),
        static_cast<int>(output_capacity_frames));
    if (produced < 0 || used < 0 ||
        static_cast<std::uint32_t>(used) > input_frames ||
        static_cast<std::uint32_t>(produced) > output_capacity_frames) {
      set_error(state, "libresample returned invalid native progress");
      return -3;
    }
    if (common_consumed < 0) {
      common_consumed = used;
      common_produced = produced;
    } else if (common_consumed != used || common_produced != produced) {
      set_error(state, "libresample channel states returned different progress");
      return -4;
    }
  }
  *consumed_frames = static_cast<std::uint32_t>(std::max(0, common_consumed));
  *produced_frames = static_cast<std::uint32_t>(std::max(0, common_produced));
  for (std::uint32_t frame = 0; frame < *produced_frames; ++frame) {
    for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
      output[static_cast<std::size_t>(frame) * state->channels + channel] =
          state->channel_output[channel][frame];
    }
  }
  return 0;
}

}  // namespace

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() { return "libresample"; }
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "libresample 0.1.5 development head";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "7cb7f9c3f72d4e6774d964dc324af827192df7c3";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC/G++ 15.2.0 -O3 -DNDEBUG -march=x86-64-v2; resample.c/filterkit.c/resamplesubs.c source-integrated release shim";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "two independent high-quality libresample C states with interleaved float shim";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "resample_open highQuality=1 at fixed exact ratio; one state per channel; final zero padding trimmed to exact duration";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "linear-phase Kaiser-windowed sinc; filter-width input latency reported; documented padding used for exact output duration";
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
      max_input_frames == 0 ||
      max_input_frames > static_cast<std::uint32_t>(std::numeric_limits<int>::max() / 4)) {
    set_error(nullptr, "libresample shim requires bounded non-zero geometry");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new LibresampleState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    state->max_input_frames = max_input_frames;
    state->ratio = static_cast<double>(output_rate) / input_rate;
    const std::size_t output_capacity =
        static_cast<std::size_t>(max_input_frames) * 4 + 4096;
    state->channel_input.assign(
        channels, std::vector<float>(max_input_frames + 4096, 0.0F));
    state->channel_output.assign(channels,
                                 std::vector<float>(output_capacity, 0.0F));
    state->zeros.assign(max_input_frames + 4096, 0.0F);
    if (!open_resamplers(state)) {
      global_error = state->error;
      delete state;
      if (error != nullptr) {
        *error = -2;
      }
      return nullptr;
    }
    return state;
  } catch (const std::exception& exception) {
    set_error(nullptr, exception.what());
  } catch (...) {
    set_error(nullptr, "unknown exception during libresample shim construction");
  }
  if (error != nullptr) {
    *error = -3;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  auto* state = static_cast<LibresampleState*>(opaque);
  if (state != nullptr) {
    close_resamplers(state);
    delete state;
  }
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(void* opaque,
                                                         std::uint32_t input_frames) {
  auto* state = static_cast<LibresampleState*>(opaque);
  if (state == nullptr) {
    return 0;
  }
  const std::uint64_t nominal =
      aeb_round_output_frames(input_frames, state->input_rate, state->output_rate);
  return static_cast<std::uint32_t>(std::min<std::uint64_t>(
      nominal + state->filter_width + 4096,
      std::numeric_limits<std::uint32_t>::max()));
}

AEB_EXPORT std::uint32_t aeb_resampler_latency_frames(void* opaque) {
  auto* state = static_cast<LibresampleState*>(opaque);
  if (state == nullptr) {
    return 0;
  }
  return static_cast<std::uint32_t>(aeb_round_output_frames(
      state->filter_width, state->input_rate, state->output_rate));
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<LibresampleState*>(opaque);
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
  auto* state = static_cast<LibresampleState*>(opaque);
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
      set_error(state, "libresample shim received invalid input lifecycle or size");
      return -2;
    }
    const int result = process_channels(
        state, static_cast<const float*>(input), input_frames,
        static_cast<float*>(output), output_capacity_frames, false,
        consumed_frames, produced_frames);
    if (result != 0) {
      return result;
    }
    state->total_input += *consumed_frames;
    state->total_output += *produced_frames;
    state->ended = end_of_input != 0;
    return 0;
  }
  if (!state->ended || end_of_input == 0) {
    set_error(state, "libresample drain called before end-of-input");
    return -5;
  }
  const std::uint64_t target = target_output(state);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  const std::uint32_t padding =
      std::min<std::uint32_t>(state->max_input_frames + state->filter_width,
                              static_cast<std::uint32_t>(state->zeros.size()));
  std::uint32_t padding_used = 0;
  std::uint32_t native_produced = 0;
  const std::uint32_t remaining = static_cast<std::uint32_t>(
      std::min<std::uint64_t>(target - state->total_output,
                              output_capacity_frames));
  const int result = process_channels(
      state, state->zeros.data(), padding, static_cast<float*>(output),
      remaining, true, &padding_used, &native_produced);
  if (result != 0) {
    return result;
  }
  *produced_frames = native_produced;
  state->total_output += native_produced;
  if (state->total_output >= target || native_produced == 0) {
    state->finished = true;
    *finished = 1;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "libresample process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<LibresampleState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  if (!open_resamplers(state)) {
    return -2;
  }
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->finished = false;
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "libresample reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<LibresampleState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
