#include "capture_encoder.h"

#include "adaptive_encoder.h"

#include <algorithm>
#include <exception>
#include <iomanip>
#include <iostream>
#include <syncstream>

// Encoder probe, Auto cadence measurement, and hardware candidate selection.
namespace piik::capture::windows {
namespace {

// Only the probe borrows the hardware codec. Destroy its synthetic adaptation
// history before the same codec moves into the ordinary live output pipeline.
class ProbeEncoder final : public VideoEncoder {
 public:
  explicit ProbeEncoder(VideoEncoder& encoder)
      : VideoEncoder(encoder.kind, encoder.name, encoder.identity), encoder_(encoder) {}

  EncodedAccessUnit Encode(ID3D11Texture2D* texture, UINT64 timestamp, bool key,
      EncoderClock::time_point deadline) override {
    return encoder_.Encode(texture, timestamp, key, deadline);
  }
  void SetBitrate(UINT32 bitrate) override { encoder_.SetBitrate(bitrate); }

 private:
  VideoEncoder& encoder_;
};

}  // namespace

// Some drivers emit valid H264 at a fraction of the source rate, while the
// encoder reports no limitation (realtime-quality-adaptation.md). Pace synthetic
// frames through the same WebRTC pipeline as a live output, including its rate
// control and frame dropper, with the Browser probe's warmup, one-second sample
// and 100 ms lag allowance (video-codec-preflight.ts).
UINT32 CadenceLagFrames(UINT32 frame_rate) {
  return std::max<UINT32>(1, (frame_rate + 9) / 10);
}

bool SustainsEncodedCadence(VideoEncoder& hardware, ID3D11Device* device,
                            const VideoProfile& profile, EncoderClock::time_point deadline,
                            const EncoderProbeTextures& textures) {
  RequireEncoderTime(deadline);
  const auto [warmup_frames, sample_frames, interval] = CadenceTiming(profile);
  const UINT32 lag = CadenceLagFrames(profile.frame_rate);
  AdaptiveEncoder encoder(OutputKind::h264, profile, device,
      [&](const VideoProfile& selected) -> std::unique_ptr<VideoEncoder> {
        if (selected.width != profile.width || selected.height != profile.height ||
            selected.frame_rate != profile.frame_rate)
          Fail("codec-probe-output", "encoder changed the probe profile");
        hardware.SetBitrate(selected.bit_rate);
        return std::make_unique<ProbeEncoder>(hardware);
      });
  auto start = EncoderClock::now();
  UINT32 delivered = 0;
  for (UINT32 frame = 0; frame < warmup_frames + sample_frames; ++frame) {
    // Cold activation and the first IDR belong to warmup, not steady cadence.
    // Start the measured schedule afresh; do not carry warmup lag into it.
    if (frame == warmup_frames) start = EncoderClock::now() - interval * frame;
    const auto due = start + interval * frame;
    std::this_thread::sleep_until(std::min(due, deadline));
    RequireEncoderTime(deadline);
    // One encode is in flight, so a slow encoder delays every later source frame.
    if (frame >= warmup_frames && EncoderClock::now() > due + interval * lag) return false;
    const auto& texture = textures[frame % textures.size()];
    auto output = encoder.Encode([&](UINT32 width, UINT32 height) {
      if (width != profile.width || height != profile.height)
        Fail("codec-probe-output", "encoder reduced the probe resolution");
      return texture;
    }, profile.width, profile.height,
        (static_cast<UINT64>(frame) + 1) * profile.frame_duration_100ns(), frame == 0,
        profile.bit_rate, deadline);
    RequireEncoderTime(deadline);
    if (frame >= warmup_frames && EncoderClock::now() > due + interval * lag) return false;
    if (frame >= warmup_frames) {
      if (output && !output->access_unit.bytes.empty()) ++delivered;
      // Even perfect delivery of the remaining frames cannot recover this deficit.
      if (frame - warmup_frames + 1 - delivered > lag) return false;
    }
  }
  return delivered > 0 && delivered + lag >= sample_frames;
}

void LogEncoderRejection(EncoderCandidate candidate, const GateFailure& error) {
  std::osyncstream output(std::cerr);
  output << "result=encoder-candidate-rejected adapter=" << candidate.first
         << " encoder=" << candidate.second << " stage=" << error.stage()
         << " detail=" << std::quoted(error.what());
  if (FAILED(error.result())) output << " hresult=0x" << std::hex << error.result();
  output << '\n';
}

VideoEncoderSelection SelectVideoEncoder(
    ProductArguments& arguments, const std::vector<Adapter>& adapters,
    DeviceContext& device) {
  if (arguments.codec == "vp8") {
    device = CreateDevice(SelectAdapter(adapters, arguments.adapter_index));
    return {};
  }
  const auto deadline = EncoderClock::now() + std::chrono::seconds(4);
  std::unique_ptr<VideoEncoder> hardware;
  try {
    std::vector<EncoderCandidate> candidates;
    for (const auto& adapter : adapters) {
      RequireEncoderTime(deadline);
      try {
        const auto encoders = EnumerateHardwareEncoders(adapter, false);
        for (UINT index = 0; index < encoders.count; ++index)
          candidates.emplace_back(adapter.index, index);
      } catch (const GateFailure& error) {
        LogEncoderRejection({adapter.index, 0}, error);
      }
    }
    const auto chosen = SelectEncoderCandidate(candidates,
        {arguments.adapter_index, arguments.mft_index}, deadline,
        [&](EncoderCandidate candidate) {
          const auto& adapter = SelectAdapter(adapters, candidate.first);
          auto next_device = CreateDevice(adapter);
          try {
            const auto encoders = EnumerateHardwareEncoders(adapter);
            auto selected = ActivateTransform(encoders, candidate.second, next_device.manager.Get());
            auto next = std::make_unique<LiveEncoder>(std::move(selected), arguments.profile);
            if (arguments.codec == "auto") {
              RequireEncoderTime(deadline);
              // Synthesis/conversion belongs to setup, outside the paced measurement.
              const auto textures = PrepareEncoderProbeTextures(next_device.device.Get(), arguments.profile);
              if (!SustainsEncodedCadence(*next, next_device.device.Get(),
                      arguments.profile, deadline, textures))
                Fail("codec-probe-cadence", "hardware H264 did not sustain the target frame rate");
            }
            if (arguments.codec == "h264") {
              // Explicit H264 validates output without Auto's cadence measurement.
              FrameConverter converter(next_device.device.Get(), arguments.profile);
              const auto texture = CreateEncoderProbeTexture(
                  next_device.device.Get(), converter, 0, arguments.profile);
              next->Encode(texture.Get(), 0, true, deadline);
            }
            RequireEncoderTime(deadline);
            device = std::move(next_device);
            // Keep the proven hardware instance; the probe's VSE is already gone.
            hardware = std::move(next);
          } catch (const GateFailure& error) {
            // Query before destroying this candidate's device: the failing API
            // often reports only DEVICE_REMOVED, hiding the actual driver reason.
            const HRESULT reason = next_device.device->GetDeviceRemovedReason();
            if (SUCCEEDED(reason)) throw;
            std::ostringstream detail;
            detail << error.what() << "; device-removed-reason=0x" << std::hex
                   << std::setfill('0') << std::setw(8) << static_cast<UINT32>(reason);
            throw GateFailure(error.stage(), detail.str(), error.result());
          }
        }, arguments.codec == "auto" ? CadenceTiming(arguments.profile).MinimumDuration()
                                    : EncoderClock::duration::zero());
    arguments.adapter_index = chosen.first;
    arguments.mft_index = chosen.second;
    return {OutputKind::h264, std::move(hardware)};
  } catch (const GateFailure&) {
    if (arguments.codec == "h264") throw;
    hardware.reset();
  }
  // Without a sustaining hardware candidate, Auto starts the ordinary VP8 path.
  device = CreateDevice(SelectAdapter(adapters, arguments.adapter_index));
  return {};
}

}  // namespace piik::capture::windows
