#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <limits>
#include <new>
#include <string>

extern "C" {
#include <libavutil/channel_layout.h>
#include <libavutil/error.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>
}

namespace {

thread_local std::string global_error;

struct FfmpegState {
  SwrContext* context = nullptr;
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool finished = false;
  std::string error;
};

void set_error(FfmpegState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

std::string ffmpeg_error(const char* operation, int code) {
  char buffer[AV_ERROR_MAX_STRING_SIZE] = {};
  if (av_strerror(code, buffer, sizeof(buffer)) == 0) {
    return std::string(operation) + " failed: " + buffer;
  }
  return std::string(operation) + " failed with FFmpeg error " +
         std::to_string(code);
}

int initialize(FfmpegState* state) {
  AVChannelLayout layout = AV_CHANNEL_LAYOUT_STEREO;
  int result = swr_alloc_set_opts2(
      &state->context, &layout, AV_SAMPLE_FMT_DBL,
      static_cast<int>(state->output_rate), &layout, AV_SAMPLE_FMT_DBL,
      static_cast<int>(state->input_rate), 0, nullptr);
  av_channel_layout_uninit(&layout);
  if (result < 0) {
    set_error(state, ffmpeg_error("swr_alloc_set_opts2", result));
    return result;
  }
  result = swr_init(state->context);
  if (result < 0) {
    set_error(state, ffmpeg_error("swr_init", result));
    swr_free(&state->context);
  }
  return result;
}

}  // namespace

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() {
  return "ffmpeg_libswresample";
}
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "FFmpeg n8.0.1 libswresample 6";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "894da5ca7d742e4429ffb2af534fcda0103ef593";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC 15.2.0 -O3 -DNDEBUG; FFmpeg n8.0.1 clean source revision; installed include-tree manifest sha256=9C7EF81AF2DA1EEA17A5C5EAA3A678BD72F5C0F70C99C4A22D4C064D43666AFA (relative-path/SP/SHA-256 records, UTF-8/LF); pinned import/runtime library hashes";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "native swr_alloc_set_opts2/swr_convert streaming shim";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "libswresample SWR engine defaults; packed AV_SAMPLE_FMT_DBL stereo; filter_size=32; phase_shift=10; exact_rational=1";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "linear-phase windowed sinc; null-input flush";
}
AEB_EXPORT int aeb_resampler_sample_format() {
  return AEB_SAMPLE_FORMAT_F64;
}
AEB_EXPORT std::uint32_t aeb_resampler_dependency_count() { return 3; }
AEB_EXPORT const char* aeb_resampler_dependency_path(std::uint32_t index) {
  if (index == 0) {
    return "swresample-6.dll";
  }
  if (index == 1) {
    return "avutil-60.dll";
  }
  if (index == 2) {
    return "libwinpthread-1.dll";
  }
  return nullptr;
}

AEB_EXPORT void* aeb_resampler_create(std::uint32_t input_rate,
                                      std::uint32_t output_rate,
                                      std::uint32_t channels,
                                      std::uint32_t,
                                      int* error) {
  if (error != nullptr) {
    *error = 0;
  }
  if (input_rate == 0 || output_rate == 0 || channels != 2) {
    set_error(nullptr, "FFmpeg shim requires non-zero rates and stereo");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new FfmpegState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    const int result = initialize(state);
    if (result < 0) {
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
    set_error(nullptr, "unknown exception during FFmpeg shim construction");
  }
  if (error != nullptr) {
    *error = -2;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  auto* state = static_cast<FfmpegState*>(opaque);
  if (state != nullptr) {
    swr_free(&state->context);
    delete state;
  }
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(void* opaque,
                                                         std::uint32_t input_frames) {
  auto* state = static_cast<FfmpegState*>(opaque);
  if (state == nullptr) {
    return 0;
  }
  const std::uint64_t nominal =
      aeb_round_output_frames(input_frames, state->input_rate, state->output_rate);
  return static_cast<std::uint32_t>(
      std::min<std::uint64_t>(nominal + 8192, std::numeric_limits<std::uint32_t>::max()));
}

AEB_EXPORT std::uint32_t aeb_resampler_latency_frames(void*) {
  return AEB_RESAMPLER_UNKNOWN_LATENCY;
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<FfmpegState*>(opaque);
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
  auto* state = static_cast<FfmpegState*>(opaque);
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
  if (input_frames > 0 && (input == nullptr || state->ended)) {
    set_error(state, "FFmpeg shim received invalid input lifecycle");
    return -2;
  }

  auto* output_bytes = static_cast<std::uint8_t*>(output);
  std::uint8_t* output_planes[1] = {output_bytes};
  const auto* input_bytes = static_cast<const std::uint8_t*>(input);
  const std::uint8_t* input_planes[1] = {input_bytes};
  const std::uint8_t* const* native_input = input_frames == 0 ? nullptr : input_planes;
  const std::uint64_t target = aeb_round_output_frames(
      state->total_input + input_frames, state->input_rate, state->output_rate);
  if (input_frames == 0 && state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  const std::uint64_t remaining = target - state->total_output;
  const std::uint32_t offered_output =
      end_of_input == 0
          ? output_capacity_frames
          : static_cast<std::uint32_t>(std::min<std::uint64_t>(
                output_capacity_frames, remaining));
  const int result = swr_convert(
      state->context, output_planes, static_cast<int>(offered_output),
      native_input, static_cast<int>(input_frames));
  if (result < 0) {
    set_error(state, ffmpeg_error("swr_convert", result));
    return result;
  }
  *consumed_frames = input_frames;
  *produced_frames = static_cast<std::uint32_t>(result);
  state->total_output += *produced_frames;
  if (input_frames > 0) {
    state->total_input += input_frames;
    state->ended = end_of_input != 0;
  } else if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
  } else if (result == 0) {
    set_error(state, "FFmpeg drain ended before the exact target duration");
    return -3;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "FFmpeg process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<FfmpegState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  swr_close(state->context);
  const int result = swr_init(state->context);
  if (result < 0) {
    set_error(state, ffmpeg_error("swr_init(reset)", result));
    return result;
  }
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->finished = false;
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "FFmpeg reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<FfmpegState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
