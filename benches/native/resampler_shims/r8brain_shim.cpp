#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <limits>
#include <memory>
#include <string>
#include <vector>

#include <CDSPResampler.h>

namespace {

thread_local std::string global_error;

struct R8brainState {
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint32_t max_input_frames = 0;
  std::uint32_t latency_frames = 0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool finished = false;
  std::vector<std::unique_ptr<r8b::CDSPResampler>> resamplers;
  std::vector<std::vector<double>> channel_input;
  std::vector<double*> native_output;
  std::vector<double> zeros;
  std::string error;
};

void set_error(R8brainState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

std::uint64_t target_output(const R8brainState* state) {
  return aeb_round_output_frames(state->total_input, state->input_rate,
                                 state->output_rate);
}

int process_channels(R8brainState* state,
                     const double* input,
                     std::uint32_t input_frames,
                     double* output,
                     std::uint32_t output_capacity_frames,
                     std::uint32_t* produced_frames) {
  for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
    auto& channel_buffer = state->channel_input[channel];
    for (std::uint32_t frame = 0; frame < input_frames; ++frame) {
      channel_buffer[frame] = input[static_cast<std::size_t>(frame) *
                                      state->channels +
                                  channel];
    }
  }

  int common_output = -1;
  std::fill(state->native_output.begin(), state->native_output.end(), nullptr);
  for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
    const int count = state->resamplers[channel]->process(
        state->channel_input[channel].data(), static_cast<int>(input_frames),
        state->native_output[channel]);
    if (count < 0 || static_cast<std::uint32_t>(count) > output_capacity_frames) {
      set_error(state, "r8brain returned output beyond shim capacity");
      return -3;
    }
    if (common_output < 0) {
      common_output = count;
    } else if (common_output != count) {
      set_error(state, "r8brain channel states returned different output lengths");
      return -4;
    }
  }
  const std::uint32_t count = static_cast<std::uint32_t>(std::max(0, common_output));
  for (std::uint32_t frame = 0; frame < count; ++frame) {
    for (std::uint32_t channel = 0; channel < state->channels; ++channel) {
      output[static_cast<std::size_t>(frame) * state->channels + channel] =
          state->native_output[channel][frame];
    }
  }
  *produced_frames = count;
  return 0;
}

}  // namespace

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() { return "r8brain"; }
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "r8brain-free-src 7.1";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "e71c31bf320f84210bb4bdcb57e296c39ce940f9";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC 15.2.0 -O3 -DNDEBUG -march=x86-64-v2; source-integrated header implementation; FFT4G double backend";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "two independent r8b::CDSPResampler24 channel states with interleaved shim";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "CDSPResampler24; 2 percent transition band; 180.15 dB requested attenuation; linear phase; double precision";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "linear phase; upstream automatic initial-latency consumption; bounded zero-tail drain to duration-aligned output";
}
AEB_EXPORT int aeb_resampler_sample_format() {
  return AEB_SAMPLE_FORMAT_F64;
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
      max_input_frames > static_cast<std::uint32_t>(std::numeric_limits<int>::max())) {
    set_error(nullptr, "r8brain shim requires bounded non-zero geometry");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new R8brainState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    state->max_input_frames = max_input_frames;
    state->zeros.assign(static_cast<std::size_t>(max_input_frames) * channels,
                        0.0);
    state->channel_input.assign(channels,
                                std::vector<double>(max_input_frames, 0.0));
    state->native_output.assign(channels, nullptr);
    for (std::uint32_t channel = 0; channel < channels; ++channel) {
      state->resamplers.push_back(std::make_unique<r8b::CDSPResampler24>(
          static_cast<double>(input_rate), static_cast<double>(output_rate),
          static_cast<int>(max_input_frames), 2.0));
    }
    const int input_latency =
        state->resamplers.front()->getInputRequiredForOutput(1);
    state->latency_frames = static_cast<std::uint32_t>(
        aeb_round_output_frames(static_cast<std::uint64_t>(std::max(0, input_latency)),
                                input_rate, output_rate));
    return state;
  } catch (const std::exception& exception) {
    set_error(nullptr, exception.what());
  } catch (...) {
    set_error(nullptr, "unknown exception during r8brain shim construction");
  }
  if (error != nullptr) {
    *error = -2;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  delete static_cast<R8brainState*>(opaque);
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(void* opaque,
                                                         std::uint32_t input_frames) {
  auto* state = static_cast<R8brainState*>(opaque);
  if (state == nullptr || state->resamplers.empty()) {
    return 0;
  }
  const int upstream =
      state->resamplers.front()->getMaxOutLen(static_cast<int>(input_frames));
  return static_cast<std::uint32_t>(
      std::max<std::uint64_t>(static_cast<std::uint64_t>(std::max(0, upstream)) + 64,
                              1024));
}

AEB_EXPORT std::uint32_t aeb_resampler_latency_frames(void* opaque) {
  auto* state = static_cast<R8brainState*>(opaque);
  return state == nullptr ? 0 : state->latency_frames;
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<R8brainState*>(opaque);
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
  auto* state = static_cast<R8brainState*>(opaque);
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
      set_error(state, "r8brain shim received invalid input lifecycle or size");
      return -2;
    }
    const int result = process_channels(
        state, static_cast<const double*>(input), input_frames,
        static_cast<double*>(output), output_capacity_frames, produced_frames);
    if (result != 0) {
      return result;
    }
    *consumed_frames = input_frames;
    state->total_input += input_frames;
    state->total_output += *produced_frames;
    state->ended = end_of_input != 0;
    return 0;
  }
  if (!state->ended || end_of_input == 0) {
    set_error(state, "r8brain drain called before end-of-input");
    return -5;
  }
  const std::uint64_t target = target_output(state);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  for (int attempt = 0; attempt < 64; ++attempt) {
    std::uint32_t native_produced = 0;
    const int result = process_channels(
        state, state->zeros.data(), state->max_input_frames,
        static_cast<double*>(output), output_capacity_frames, &native_produced);
    if (result != 0) {
      return result;
    }
    if (native_produced == 0) {
      continue;
    }
    const std::uint32_t remaining = static_cast<std::uint32_t>(
        std::min<std::uint64_t>(target - state->total_output,
                                output_capacity_frames));
    *produced_frames = std::min(native_produced, remaining);
    state->total_output += *produced_frames;
    break;
  }
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "r8brain process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<R8brainState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  for (auto& resampler : state->resamplers) {
    resampler->clear();
  }
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->finished = false;
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "r8brain reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<R8brainState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
