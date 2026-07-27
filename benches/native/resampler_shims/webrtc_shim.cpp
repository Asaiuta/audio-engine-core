#include "audio_bench_resampler_shim.h"

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <limits>
#include <memory>
#include <new>
#include <string>
#include <vector>

#include "common_audio/resampler/include/push_resampler.h"
#include "rtc_base/memory/aligned_malloc.h"
#include "system_wrappers/include/cpu_features_wrapper.h"

namespace {

constexpr std::uint32_t kBlocksPerSecond = 100;
constexpr std::uint32_t kKernelHalfFrames = 16;

thread_local std::string global_error;

struct WebRtcState {
  std::unique_ptr<webrtc::PushResampler<float>> resampler;
  std::uint32_t input_rate = 0;
  std::uint32_t output_rate = 0;
  std::uint32_t channels = 0;
  std::uint32_t max_input_frames = 0;
  std::uint32_t source_block_frames = 0;
  std::uint32_t destination_block_frames = 0;
  std::uint32_t latency_frames = 0;
  std::uint32_t staged_frames = 0;
  std::uint32_t pending_offset_frames = 0;
  std::uint32_t pending_frames = 0;
  std::uint64_t total_input = 0;
  std::uint64_t total_output = 0;
  bool ended = false;
  bool drain_submitted = false;
  bool finished = false;
  std::vector<float> input_stage;
  std::vector<float> block_output;
  std::string error;
};

void set_error(WebRtcState* state, const std::string& message) {
  if (state != nullptr) {
    state->error = message;
  } else {
    global_error = message;
  }
}

std::uint64_t target_output(const WebRtcState* state) {
  return aeb_round_output_frames(state->total_input, state->input_rate,
                                 state->output_rate);
}

bool initialize_resampler(WebRtcState* state) {
  auto resampler = std::make_unique<webrtc::PushResampler<float>>();
  const int result = resampler->InitializeIfNeeded(
      static_cast<int>(state->input_rate),
      static_cast<int>(state->output_rate), state->channels);
  if (result != 0) {
    set_error(state, "WebRTC PushResampler rejected canonical geometry");
    return false;
  }
  state->resampler = std::move(resampler);
  return true;
}

void emit_pending(WebRtcState* state,
                  float* output,
                  std::uint32_t output_capacity_frames,
                  std::uint32_t* produced_frames) {
  const std::uint32_t available =
      output_capacity_frames - *produced_frames;
  const std::uint32_t copied = std::min(available, state->pending_frames);
  if (copied == 0) {
    return;
  }
  const std::size_t source_offset =
      static_cast<std::size_t>(state->pending_offset_frames) * state->channels;
  const std::size_t destination_offset =
      static_cast<std::size_t>(*produced_frames) * state->channels;
  std::memcpy(output + destination_offset,
              state->block_output.data() + source_offset,
              static_cast<std::size_t>(copied) * state->channels *
                  sizeof(float));
  state->pending_offset_frames += copied;
  state->pending_frames -= copied;
  *produced_frames += copied;
  state->total_output += copied;
  if (state->pending_frames == 0) {
    state->pending_offset_frames = 0;
  }
}

int resample_staged_block(WebRtcState* state, std::uint32_t valid_frames) {
  if (state->resampler == nullptr ||
      state->staged_frames != state->source_block_frames ||
      valid_frames > state->destination_block_frames ||
      state->pending_frames != 0) {
    set_error(state, "WebRTC shim reached an invalid staged-block state");
    return -1;
  }
  const std::size_t source_samples =
      static_cast<std::size_t>(state->source_block_frames) * state->channels;
  const std::size_t destination_samples =
      static_cast<std::size_t>(state->destination_block_frames) *
      state->channels;
  const int produced_samples = state->resampler->Resample(
      state->input_stage.data(), source_samples, state->block_output.data(),
      destination_samples);
  if (produced_samples < 0 ||
      static_cast<std::size_t>(produced_samples) != destination_samples) {
    set_error(state, "WebRTC PushResampler returned an invalid output count");
    return -2;
  }
  state->staged_frames = 0;
  state->pending_offset_frames = 0;
  state->pending_frames = valid_frames;
  return 0;
}

std::uint64_t required_process_output(const WebRtcState* state,
                                      std::uint32_t input_frames) {
  const std::uint64_t staged_and_new =
      static_cast<std::uint64_t>(state->staged_frames) + input_frames;
  const std::uint64_t complete_blocks =
      staged_and_new / state->source_block_frames;
  return state->pending_frames +
         complete_blocks * state->destination_block_frames;
}

int process_input(WebRtcState* state,
                  const float* input,
                  std::uint32_t input_frames,
                  float* output,
                  std::uint32_t output_capacity_frames,
                  std::uint32_t* consumed_frames,
                  std::uint32_t* produced_frames) {
  emit_pending(state, output, output_capacity_frames, produced_frames);
  while (*consumed_frames < input_frames) {
    if (state->pending_frames != 0) {
      break;
    }
    if (state->staged_frames == state->source_block_frames) {
      const int result =
          resample_staged_block(state, state->destination_block_frames);
      if (result != 0) {
        return result;
      }
      emit_pending(state, output, output_capacity_frames, produced_frames);
      continue;
    }

    const std::uint32_t stage_capacity =
        state->source_block_frames - state->staged_frames;
    const std::uint32_t remaining_input = input_frames - *consumed_frames;
    const std::uint32_t copied = std::min(stage_capacity, remaining_input);
    const std::size_t source_offset =
        static_cast<std::size_t>(*consumed_frames) * state->channels;
    const std::size_t destination_offset =
        static_cast<std::size_t>(state->staged_frames) * state->channels;
    std::memcpy(state->input_stage.data() + destination_offset,
                input + source_offset,
                static_cast<std::size_t>(copied) * state->channels *
                    sizeof(float));
    state->staged_frames += copied;
    *consumed_frames += copied;
    state->total_input += copied;
  }

  if (state->pending_frames == 0 &&
      state->staged_frames == state->source_block_frames) {
    const int result =
        resample_staged_block(state, state->destination_block_frames);
    if (result != 0) {
      return result;
    }
    emit_pending(state, output, output_capacity_frames, produced_frames);
  }
  return 0;
}

}  // namespace

namespace webrtc {

void* GetRightAlign(const void* pointer, std::size_t alignment) {
  if (pointer == nullptr || alignment == 0 ||
      (alignment & (alignment - 1)) != 0) {
    return nullptr;
  }
  const auto address = reinterpret_cast<std::uintptr_t>(pointer);
  return reinterpret_cast<void*>((address + alignment - 1) & ~(alignment - 1));
}

void* AlignedMalloc(std::size_t size, std::size_t alignment) {
  if (size == 0 || alignment == 0 || (alignment & (alignment - 1)) != 0) {
    return nullptr;
  }
  void* allocation =
      std::malloc(size + sizeof(std::uintptr_t) + alignment - 1);
  if (allocation == nullptr) {
    throw std::bad_alloc();
  }
  const auto base = reinterpret_cast<std::uintptr_t>(allocation);
  const auto aligned =
      (base + sizeof(std::uintptr_t) + alignment - 1) & ~(alignment - 1);
  std::memcpy(reinterpret_cast<void*>(aligned - sizeof(std::uintptr_t)), &base,
              sizeof(base));
  return reinterpret_cast<void*>(aligned);
}

void AlignedFree(void* aligned) {
  if (aligned == nullptr) {
    return;
  }
  std::uintptr_t base = 0;
  std::memcpy(&base,
              reinterpret_cast<const void*>(
                  reinterpret_cast<std::uintptr_t>(aligned) -
                  sizeof(std::uintptr_t)),
              sizeof(base));
  std::free(reinterpret_cast<void*>(base));
}

int GetCPUInfo(CPUFeature feature) {
#if defined(__GNUC__) && (defined(__x86_64__) || defined(__i386__))
  __builtin_cpu_init();
  switch (feature) {
    case kSSE2:
      return __builtin_cpu_supports("sse2") != 0;
    case kSSE3:
      return __builtin_cpu_supports("sse3") != 0;
    case kAVX2:
      return __builtin_cpu_supports("avx2") != 0 &&
             __builtin_cpu_supports("fma") != 0;
  }
#else
  static_cast<void>(feature);
#endif
  return 0;
}

int GetCPUInfoNoASM(CPUFeature feature) {
  static_cast<void>(feature);
  return 0;
}

std::uint64_t GetCPUFeaturesARM() { return 0; }

}  // namespace webrtc

AEB_EXPORT std::uint32_t aeb_resampler_abi_version() {
  return AEB_RESAMPLER_ABI_VERSION;
}
AEB_EXPORT const char* aeb_resampler_engine_id() { return "webrtc"; }
AEB_EXPORT const char* aeb_resampler_upstream_version() {
  return "webrtc-audio-processing 1.3";
}
AEB_EXPORT const char* aeb_resampler_source_revision() {
  return "8e258a1933d405073c9e6465628a69ac7d2a1f13";
}
AEB_EXPORT const char* aeb_resampler_build_provenance() {
  return "MinGW-w64 GCC 15.2.0 -O3 -DNDEBUG -march=x86-64-v2; source-integrated PushResampler with runtime SSE2/AVX2 dispatch";
}
AEB_EXPORT const char* aeb_resampler_implementation() {
  return "WebRTC PushResampler<float> with preallocated arbitrary-chunk 10 ms staging";
}
AEB_EXPORT const char* aeb_resampler_quality_recipe() {
  return "WebRTC SincResampler kernel=32, offset_count=32, Blackman window, 0.9 cutoff scale; runtime AVX2/SSE2; interleaved float stereo";
}
AEB_EXPORT const char* aeb_resampler_phase_response() {
  return "linear phase; 16-input-frame algorithmic delay; final 10 ms block zero padded and trimmed to exact duration";
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
  if (input_rate == 0 || output_rate == 0 || channels == 0 || channels > 2 ||
      max_input_frames == 0 || input_rate % kBlocksPerSecond != 0 ||
      output_rate % kBlocksPerSecond != 0 ||
      input_rate > static_cast<std::uint32_t>(std::numeric_limits<int>::max()) ||
      output_rate > static_cast<std::uint32_t>(std::numeric_limits<int>::max())) {
    set_error(nullptr,
              "WebRTC shim requires bounded mono/stereo rates divisible into 10 ms blocks");
    if (error != nullptr) {
      *error = -1;
    }
    return nullptr;
  }
  try {
    auto* state = new WebRtcState();
    state->input_rate = input_rate;
    state->output_rate = output_rate;
    state->channels = channels;
    state->max_input_frames = max_input_frames;
    state->source_block_frames = input_rate / kBlocksPerSecond;
    state->destination_block_frames = output_rate / kBlocksPerSecond;
    if (state->source_block_frames <= 32 ||
        state->destination_block_frames == 0) {
      global_error = "WebRTC shim block geometry is outside PushResampler bounds";
      delete state;
      if (error != nullptr) {
        *error = -2;
      }
      return nullptr;
    }
    state->latency_frames = static_cast<std::uint32_t>(
        aeb_round_output_frames(kKernelHalfFrames, input_rate, output_rate));
    state->input_stage.assign(
        static_cast<std::size_t>(state->source_block_frames) * channels, 0.0F);
    state->block_output.assign(
        static_cast<std::size_t>(state->destination_block_frames) * channels,
        0.0F);
    if (!initialize_resampler(state)) {
      global_error = state->error;
      delete state;
      if (error != nullptr) {
        *error = -3;
      }
      return nullptr;
    }
    return state;
  } catch (const std::exception& exception) {
    set_error(nullptr, exception.what());
  } catch (...) {
    set_error(nullptr, "unknown exception during WebRTC shim construction");
  }
  if (error != nullptr) {
    *error = -4;
  }
  return nullptr;
}

AEB_EXPORT void aeb_resampler_destroy(void* opaque) {
  delete static_cast<WebRtcState*>(opaque);
}

AEB_EXPORT std::uint32_t aeb_resampler_max_output_frames(
    void* opaque, std::uint32_t input_frames) {
  auto* state = static_cast<WebRtcState*>(opaque);
  if (state == nullptr) {
    return 0;
  }
  const std::uint64_t blocks =
      (static_cast<std::uint64_t>(input_frames) +
       state->source_block_frames - 1) /
      state->source_block_frames;
  const std::uint64_t frames =
      std::max<std::uint64_t>(1, blocks) * state->destination_block_frames;
  return static_cast<std::uint32_t>(std::min<std::uint64_t>(
      frames, std::numeric_limits<std::uint32_t>::max()));
}

AEB_EXPORT std::uint32_t aeb_resampler_latency_frames(void* opaque) {
  auto* state = static_cast<WebRtcState*>(opaque);
  return state == nullptr ? 0 : state->latency_frames;
}

AEB_EXPORT std::uint64_t aeb_resampler_expected_output_frames(
    void* opaque, std::uint64_t input_frames) {
  auto* state = static_cast<WebRtcState*>(opaque);
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
  auto* state = static_cast<WebRtcState*>(opaque);
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

  auto* output_samples = static_cast<float*>(output);
  if (input_frames > 0) {
    if (input == nullptr || state->ended ||
        input_frames > state->max_input_frames) {
      set_error(state, "WebRTC shim received invalid input lifecycle or size");
      return -2;
    }
    if (end_of_input != 0 &&
        required_process_output(state, input_frames) >
            output_capacity_frames) {
      set_error(state,
                "WebRTC final input requires more output capacity than supplied");
      return -3;
    }
    const int result = process_input(
        state, static_cast<const float*>(input), input_frames, output_samples,
        output_capacity_frames, consumed_frames, produced_frames);
    if (result != 0) {
      return -4;
    }
    if (end_of_input != 0) {
      if (*consumed_frames != input_frames) {
        set_error(state, "WebRTC final input was not completely consumed");
        return -5;
      }
      state->ended = true;
    }
    return 0;
  }

  if (!state->ended || end_of_input == 0) {
    set_error(state, "WebRTC drain called before end-of-input");
    return -6;
  }
  emit_pending(state, output_samples, output_capacity_frames, produced_frames);
  if (state->pending_frames != 0) {
    return 0;
  }
  const std::uint64_t target = target_output(state);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
    return 0;
  }
  if (!state->drain_submitted) {
    if (state->staged_frames == 0 ||
        state->staged_frames >= state->source_block_frames) {
      set_error(state, "WebRTC drain has no valid partial block to submit");
      return -7;
    }
    const std::size_t zero_offset =
        static_cast<std::size_t>(state->staged_frames) * state->channels;
    std::fill(state->input_stage.begin() + zero_offset,
              state->input_stage.end(), 0.0F);
    state->staged_frames = state->source_block_frames;
    const std::uint32_t valid_frames = static_cast<std::uint32_t>(
        std::min<std::uint64_t>(target - state->total_output,
                                state->destination_block_frames));
    const int result = resample_staged_block(state, valid_frames);
    if (result != 0) {
      return -8;
    }
    state->drain_submitted = true;
  }
  emit_pending(state, output_samples, output_capacity_frames, produced_frames);
  if (state->total_output >= target) {
    state->finished = true;
    *finished = 1;
  } else if (state->pending_frames == 0) {
    set_error(state, "WebRTC padded drain ended before the target duration");
    return -9;
  }
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -100, "WebRTC process")
}

AEB_EXPORT int aeb_resampler_reset(void* opaque) noexcept {
  auto* state = static_cast<WebRtcState*>(opaque);
  try {
  if (state == nullptr) {
    return -1;
  }
  if (!initialize_resampler(state)) {
    return -2;
  }
  state->staged_frames = 0;
  state->pending_offset_frames = 0;
  state->pending_frames = 0;
  state->total_input = 0;
  state->total_output = 0;
  state->ended = false;
  state->drain_submitted = false;
  state->finished = false;
  std::fill(state->input_stage.begin(), state->input_stage.end(), 0.0F);
  std::fill(state->block_output.begin(), state->block_output.end(), 0.0F);
  state->error.clear();
  return 0;
  }
  AEB_CATCH_STATE_EXCEPTIONS(state, -101, "WebRTC reset")
}

AEB_EXPORT const char* aeb_resampler_last_error(void* opaque) {
  auto* state = static_cast<WebRtcState*>(opaque);
  return state == nullptr ? global_error.c_str() : state->error.c_str();
}
