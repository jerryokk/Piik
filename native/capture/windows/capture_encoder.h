#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include "capture_arguments.h"
#include "capture_devices.h"
#include "capture_error.h"
#include "capture_frames.h"
#include "capture_output.h"
#include "h264_encoder.h"

#include <algorithm>
#include <chrono>
#include <exception>
#include <memory>
#include <utility>
#include <vector>

namespace piik::capture::windows {

// Encoder probing/selection for live capture and Auto's cadence measurement.
UINT32 CadenceLagFrames(UINT32 frame_rate);

struct CadenceTiming final {
  explicit CadenceTiming(const VideoProfile& profile)
      : warmup_frames((profile.frame_rate + 1) / 2),
        sample_frames(profile.frame_rate),
        interval(profile.frame_duration_100ns() * 100) {}

  std::chrono::nanoseconds MinimumDuration() const {
    // Each phase submits its first frame immediately; the measured phase
    // starts a fresh schedule after warmup.
    return interval * (warmup_frames + sample_frames - 2);
  }

  UINT32 warmup_frames;
  UINT32 sample_frames;
  std::chrono::nanoseconds interval;
};

bool SustainsEncodedCadence(VideoEncoder& hardware, ID3D11Device* device,
                            const VideoProfile& profile, EncoderClock::time_point deadline,
                            const EncoderProbeTextures& textures);

struct VideoEncoderSelection final {
  OutputKind kind = OutputKind::vp8;
  std::unique_ptr<VideoEncoder> initial;
};

using EncoderCandidate = std::pair<UINT, UINT>;

void LogEncoderRejection(EncoderCandidate candidate, const GateFailure& error);

// Only pre-publication setup tries another hardware candidate. A live worker
// keeps its selected device/encoder; source replacement owns any later change.
template <typename Prepare>
EncoderCandidate SelectEncoderCandidate(std::vector<EncoderCandidate> candidates,
    EncoderCandidate preferred, EncoderClock::time_point deadline, Prepare prepare,
    EncoderClock::duration minimum_time = EncoderClock::duration::zero()) {
  const auto first = std::find(candidates.begin(), candidates.end(), preferred);
  if (first != candidates.end()) std::rotate(candidates.begin(), first, first + 1);
  std::exception_ptr failure;
  for (const auto& candidate : candidates) {
    // Do not enter synchronous device setup for a probe that cannot finish
    // its required measurement. Preserve the remaining startup time for VP8.
    RequireEncoderTime(deadline - minimum_time);
    try {
      prepare(candidate);
      RequireEncoderTime(deadline);
      return candidate;
    } catch (const GateFailure& error) {
      LogEncoderRejection(candidate, error);
      failure = std::current_exception();
    }
  }
  if (failure) std::rethrow_exception(failure);
  Fail("mft-hardware-enumeration", "no usable hardware H264 encoder was found");
}

VideoEncoderSelection SelectVideoEncoder(
    ProductArguments& arguments, const std::vector<Adapter>& adapters,
    DeviceContext& device);

}  // namespace piik::capture::windows
