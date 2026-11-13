#define NOMINMAX
#include <windows.h>

#include <mfapi.h>
#include <wrl/client.h>

#include "capture_frames.h"
#include "capture_geometry.h"
#include "capture_io.h"
#include "capture_workers.h"
#include "video_bounds.h"

#include <algorithm>
#include <chrono>
#include <exception>
#include <functional>
#include <iomanip>
#include <iostream>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
#include <syncstream>
#include <thread>
#include <utility>
#include <vector>

// Serialized output workers and their shared capture-input timing helpers.
namespace piik::capture::windows {

using piik::capture::OutputKind;
using piik::capture::ProtocolWriter;

UINT64 NextPresentationTimestamp(UINT64 candidate, UINT64 previous) {
  constexpr UINT64 rtp_tick_100ns = (10'000'000 + 90'000 - 1) / 90'000;
  return std::max(candidate, previous + rtp_tick_100ns);
}

std::shared_ptr<const CaptureInput> RefreshCaptureInput(
    const CaptureInput& latest, UINT64 previous_presentation) {
  auto input = std::make_shared<CaptureInput>(latest);
  // WGC SystemRelativeTime and MFGetSystemTime share the 100 ns QPC clock.
  // Delayed capture can already have advanced presentation beyond the clock.
  input->timestamp = NextPresentationTimestamp(
      static_cast<UINT64>(MFGetSystemTime()), previous_presentation);
  return input;
}

std::optional<UINT64> CapturePresentationTimestamp(
    UINT64 captured, UINT64 previous_capture, UINT64 previous_presentation,
    UINT64 minimum_capture) {
  if (captured <= previous_capture || captured < minimum_capture) return std::nullopt;
  // A real frame can have been captured before a requested still was presented.
  // Retain its new pixels and keep separate access units at least one RTP tick apart.
  return NextPresentationTimestamp(captured, previous_presentation);
}

OutputWorker::OutputWorker(UINT8 layer, VideoProfile profile, OutputKind kind, ID3D11Device* device,
             ProtocolWriter& writer, Factory create,
             std::unique_ptr<VideoEncoder> initial, bool active,
             std::function<void(const AdaptiveAccessUnit&)> on_output,
             std::function<void(std::exception_ptr)> on_failure,
             std::function<void()> on_refresh)
    : layer_(layer), profile_(profile), kind_(kind), device_(device), writer_(writer),
      create_(std::move(create)), initial_(std::move(initial)),
      on_output_(std::move(on_output)), on_failure_(std::move(on_failure)),
      on_refresh_(std::move(on_refresh)),
      mailbox_(profile.bit_rate, active), thread_([this]() { Run(); }) {}

OutputWorker::~OutputWorker() { Stop(); Join(); }

void OutputWorker::Submit(std::shared_ptr<const CaptureInput> input) {
  mailbox_.Submit(std::move(input));
}

bool OutputWorker::SetActive(bool active) { return mailbox_.SetActive(active); }
void OutputWorker::RequestKeyFrame() { mailbox_.RequestKeyFrame(); }
void OutputWorker::SetBitrate(UINT32 bitrate) { mailbox_.SetBitrate(bitrate); }

void OutputWorker::Stop() {
  mailbox_.Stop();
  if (thread_.joinable()) CancelSynchronousIo(thread_.native_handle());
}

void OutputWorker::Join() {
  if (!thread_.joinable()) return;
  // Keep cancellation effective if a retiring worker was about to enter WriteFile.
  const DWORD interval = std::max<DWORD>(1, 1000 / profile_.frame_rate);
  while (WaitForSingleObject(thread_.native_handle(), interval) == WAIT_TIMEOUT) {
    CancelSynchronousIo(thread_.native_handle());
  }
  thread_.join();
}

void OutputWorker::Run() noexcept {
  bool apartment = false;
  auto initial = std::move(initial_);
  std::unique_ptr<AdaptiveEncoder> encoder;
  std::unique_ptr<FrameConverter> converter;
  try {
    Check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "output-com-apartment");
    apartment = true;
    UINT64 codec_generation = 0;
    std::optional<std::pair<UINT64, UINT32>> delivered_input;
    UINT32 converted_width = 0, converted_height = 0;
    for (;;) {
      const auto work = mailbox_.Take();
      if (work.action == Mailbox::Action::stop) break;
      if (work.action == Mailbox::Action::retire) {
        encoder.reset();
        initial.reset();
        converter.reset();
        delivered_input.reset();
        continue;
      }
      const auto& input = work.input;
      // Quiet pixels still need publication at a newly requested encoder budget.
      const auto requested_input = std::pair{input->capture_timestamp, work.bitrate};
      if (codec_generation != work.generation) {
        encoder.reset();
        converter.reset();
        codec_generation = work.generation;
        delivered_input.reset();
      }
      std::optional<AdaptiveAccessUnit> output;
      try {
        const auto size = piik::capture::BoundedFrameSize(input->presentation,
            {static_cast<LONG>(profile_.width), static_cast<LONG>(profile_.height)});
        if (!encoder) {
          encoder = std::make_unique<AdaptiveEncoder>(
              kind_, profile_, device_.Get(), create_, std::move(initial), layer_);
        }
        output = encoder->Encode([&](UINT32 width, UINT32 height) {
          if (!converter || converted_width != width || converted_height != height) {
            auto selected = profile_;
            selected.width = width;
            selected.height = height;
            converter = std::make_unique<FrameConverter>(device_.Get(), selected);
            converted_width = width;
            converted_height = height;
          }
          return converter->Convert(input->texture.Get(), input->width,
                                    input->height, input->presentation, input->color_space);
        }, static_cast<UINT32>(size.cx), static_cast<UINT32>(size.cy),
           input->timestamp, work.recovery, work.bitrate);
      } catch (...) {
        // Device loss is current shared evidence; an old codec failure only
        // retires its activation. No retry occurs without a new activation.
        if (FAILED(device_->GetDeviceRemovedReason())) throw;
        if (mailbox_.Fail(work.generation)) {
          on_failure_(std::current_exception());
          break;
        }
        encoder.reset();
        initial.reset();
        converter.reset();
        continue;
      }
      auto request_refresh = [&] {
        if (on_refresh_ && mailbox_.Retry(work.generation, work.recovery)) on_refresh_();
      };
      if (!output) {
        if (delivered_input != requested_input || work.recovery) request_refresh();
        continue;
      }
      if (!mailbox_.Accept(work.generation)) continue;
      const auto& access_unit = output->access_unit;
      Check(writer_.Write(kind_, access_unit.key_frame ? 1 : 0,
                           access_unit.timestamp100ns, output->duration100ns,
                           access_unit.bytes.data(), static_cast<DWORD>(access_unit.bytes.size()),
                           layer_, static_cast<UINT16>(output->width),
                           static_cast<UINT16>(output->height)), "capture-video-output");
      delivered_input = requested_input;
      if (on_output_) on_output_(*output);
      if (work.recovery && !access_unit.key_frame) request_refresh();
    }
  } catch (...) {
    if (mailbox_.Fail()) on_failure_(std::current_exception());
  }
  encoder.reset();
  initial.reset();
  converter.reset();
  if (apartment) CoUninitialize();
}

void AppendOutputProfiles(std::ostream& output, const std::vector<VideoProfile>& profiles) {
  output << ",\"outputs\":[";
  for (size_t index = 0; index < profiles.size(); ++index) {
    if (index != 0) output << ',';
    const auto& profile = profiles[index];
    output << "{\"width\":" << profile.width << ",\"height\":" << profile.height
           << ",\"fps\":" << profile.frame_rate << ",\"bitrate\":" << profile.bit_rate << '}';
  }
  output << ']';
}

void WriteVideoActive(ProtocolWriter& writer, const ProductArguments& arguments,
                      bool hardware, const AdaptiveAccessUnit& output) {
  std::ostringstream status;
  status << "{\"state\":\"active\",\"hardwareOnly\":" << (hardware ? "true" : "false")
         << ",\"codec\":" << JSONString(hardware ? "h264" : "vp8");
  if (hardware) {
    const auto profile = InspectAnnexB(output.access_unit.bytes).profile_level_id;
    if (!profile) Fail("capture-active-profile", "first H264 output did not carry an SPS profile");
    status << ",\"profileLevelId\":" << JSONString(*profile);
  }
  status << ",\"width\":" << output.width << ",\"height\":" << output.height
         << ",\"fps\":" << arguments.profile.frame_rate;
  AppendOutputProfiles(status, arguments.outputs);
  status << '}';
  Check(writer.WriteStatus(status.str()), "capture-status-active");
}

std::string OutputFailureDetail(size_t layer, std::exception_ptr error) {
  try {
    std::rethrow_exception(error);
  } catch (const GateFailure& failed) {
    std::osyncstream diagnostic(std::cerr);
    diagnostic << "result=output-encoder-unavailable layer=" << layer
               << " stage=" << failed.stage() << " detail=" << failed.what();
    if (FAILED(failed.result())) {
      diagnostic << " hresult=0x" << std::hex << std::setfill('0')
                 << std::setw(8) << static_cast<UINT32>(failed.result());
    }
    diagnostic << '\n';
    return failed.stage();
  } catch (const std::exception& failed) {
    return failed.what();
  } catch (...) {
    return "Output encoder is unavailable";
  }
}

}  // namespace piik::capture::windows
