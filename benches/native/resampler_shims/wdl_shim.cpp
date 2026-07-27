#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <exception>
#include <limits>
#include <string>

#include <resample.h>

namespace {

thread_local std::string global_error;

struct WdlState {
  WDL_Resampler resampler;
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint32_t max_input_frames = 0;
  std::uint32_t latency_frames = 0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool drain_submitted = false;
  bool finished = false;
  std::string error;
};

void set_error(WdlState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

std::uint64_t target_output(const WdlState* state) {
  return aeb_round_output_frames(state->total_input, state->input_rate,
                                 state->output_rate);
}

void configure(WdlState* state) {
  state->resampler.SetFeedMode(true);
  state->resampler.SetMode(false, 0, true, 64, 32);
  state->resampler.SetRates(static_cast<double>(state->input_rate),
                            static_cast<double>(state->output_rate));
  state->resampler.Prealloc(
      static_cast<int>(state->channels),
      static_cast<int>(state->max_input_frames + 128),
      static_cast<int>(state->max_input_frames * 2 + 1024));
  state->resampler.Reset();
}

}  // namespace

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() { return "wdl"; }
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "WDL commit 96b770f (2026-07-15)";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "96b770f7368f75b53756e0c8941ce3ecc8b6c29b";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC 15.2.0 -O3 -DNDEBUG -march=x86-64-v2; WDL resample.cpp source-integrated release shim";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "native WDL_Resampler input-driven interleaved double shim";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "WDL sinc mode size=64, interpolation table=32, input-driven feed mode, double samples, stereo";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "linear-phase sinc; 31-input-frame startup latency reported; documented short-input flush trimmed to duration alignment";
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
      channels > static_cast<std::uint32_t>(std::numeric_limits<int>::max()) ||
      max_input_frames > static_cast<std::uint32_t>(std::numeric_limits<int>::max() / 4)) {
    set_error(nullptr, "WDL shim requires bounded non-zero geometry");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new WdlState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    state->max_input_frames = max_input_frames;
    state->latency_frames = static_cast<std::uint32_t>(
        aeb_round_output_frames(31, input_rate, output_rate));
    configure(state);
    return state;
  } catch (const std::exception& exception) {
    set_error(nullptr, exception.what());
  } catch (...) {
    set_error(nullptr, "unknown exception during WDL shim construction");
  }
  if (error != nullptr) {
    *error = -2;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  delete static_cast<WdlState*>(opaque);
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(void* opaque,
                                                         std::uint32_t input_frames) {
  auto* state = static_cast<WdlState*>(opaque);
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
  auto* state = static_cast<WdlState*>(opaque);
  return state == nullptr ? 0 : state->latency_frames;
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<WdlState*>(opaque);
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
  auto* state = static_cast<WdlState*>(opaque);
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
      set_error(state, "WDL shim received invalid input lifecycle or size");
      return -2;
    }
    WDL_ResampleSample* native_input = nullptr;
    const int requested = state->resampler.ResamplePrepare(
        static_cast<int>(input_frames), static_cast<int>(state->channels),
        &native_input);
    if (requested != static_cast<int>(input_frames) || native_input == nullptr) {
      set_error(state, "WDL input-driven prepare did not accept the complete block");
      return -3;
    }
    std::memcpy(native_input, input,
                static_cast<std::size_t>(input_frames) * state->channels *
                    sizeof(double));
    const int produced = state->resampler.ResampleOut(
        static_cast<double*>(output), static_cast<int>(input_frames),
        static_cast<int>(output_capacity_frames),
        static_cast<int>(state->channels));
    if (produced < 0 ||
        static_cast<std::uint32_t>(produced) > output_capacity_frames) {
      set_error(state, "WDL ResampleOut returned an invalid output count");
      return -4;
    }
    *consumed_frames = input_frames;
    *produced_frames = static_cast<std::uint32_t>(produced);
    state->total_input += input_frames;
    state->total_output += *produced_frames;
    state->ended = end_of_input != 0;
    return 0;
  }
  if (!state->ended || end_of_input == 0) {
    set_error(state, "WDL drain called before end-of-input");
    return -5;
  }
  const std::uint64_t target = target_output(state);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  WDL_ResampleSample* unused = nullptr;
  const int requested = state->resampler.ResamplePrepare(
      static_cast<int>(state->max_input_frames),
      static_cast<int>(state->channels), &unused);
  const std::uint32_t remaining = static_cast<std::uint32_t>(
      std::min<std::uint64_t>(target - state->total_output,
                              output_capacity_frames));
  const int produced = state->resampler.ResampleOut(
      static_cast<double*>(output), 0, static_cast<int>(remaining),
      static_cast<int>(state->channels));
  if (requested <= 0 || produced < 0 ||
      static_cast<std::uint32_t>(produced) > remaining) {
    set_error(state, "WDL flush returned invalid progress");
    return -6;
  }
  *produced_frames = static_cast<std::uint32_t>(produced);
  state->total_output += *produced_frames;
  state->drain_submitted = true;
  if (state->total_output >= target || *produced_frames == 0) {
    state->finished = true;
    *finished = 1;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "WDL process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<WdlState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  state->resampler.Reset();
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->drain_submitted = false;
  state->finished = false;
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "WDL reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<WdlState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
