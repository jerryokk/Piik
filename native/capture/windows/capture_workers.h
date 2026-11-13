#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <d3d11.h>
#include <mfapi.h>
#include <wrl/client.h>

#include "adaptive_encoder.h"
#include "capture_arguments.h"
#include "capture_color.h"
#include "capture_error.h"
#include "capture_output.h"
#include "h264_encoder.h"
#include "output_mailbox.h"

#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <thread>
#include <vector>

namespace piik::capture::windows {

struct CaptureInput final {
  ComPtr<ID3D11Texture2D> texture;
  DXGI_COLOR_SPACE_TYPE color_space = kDesktopColor;
  UINT32 width = 0;
  UINT32 height = 0;
  SIZE presentation{};
  // Presentation advances on still refresh; content identity does not.
  UINT64 timestamp = 0;
  UINT64 capture_timestamp = 0;
};

std::shared_ptr<const CaptureInput> RefreshCaptureInput(
    const CaptureInput& latest, UINT64 previous_presentation);

std::optional<UINT64> CapturePresentationTimestamp(
    UINT64 captured, UINT64 previous_capture, UINT64 previous_presentation,
    UINT64 minimum_capture);

// One serialized encode/write thread per derived output level.
class OutputWorker final {
 public:
  using Factory = AdaptiveEncoder::Factory;
  OutputWorker(UINT8 layer, VideoProfile profile, OutputKind kind, ID3D11Device* device,
               ProtocolWriter& writer, Factory create,
               std::unique_ptr<VideoEncoder> initial, bool active,
               std::function<void(const AdaptiveAccessUnit&)> on_output,
               std::function<void(std::exception_ptr)> on_failure,
               std::function<void()> on_refresh = {});
  ~OutputWorker();

  void Submit(std::shared_ptr<const CaptureInput> input);
  bool SetActive(bool active);
  void RequestKeyFrame();
  void SetBitrate(UINT32 bitrate);
  void Stop();
  void Join();

 private:
  void Run() noexcept;

  const UINT8 layer_;
  const VideoProfile profile_;
  const OutputKind kind_;
  ComPtr<ID3D11Device> device_;
  ProtocolWriter& writer_;
  Factory create_;
  std::unique_ptr<VideoEncoder> initial_;
  std::function<void(const AdaptiveAccessUnit&)> on_output_;
  std::function<void(std::exception_ptr)> on_failure_;
  std::function<void()> on_refresh_;
  using Mailbox = piik::capture::OutputMailbox<CaptureInput>;
  Mailbox mailbox_;
  std::thread thread_;
};

void AppendOutputProfiles(std::ostream& output, const std::vector<VideoProfile>& profiles);

void WriteVideoActive(ProtocolWriter& writer, const ProductArguments& arguments,
                      bool hardware, const AdaptiveAccessUnit& output);

std::string OutputFailureDetail(size_t layer, std::exception_ptr error);

}  // namespace piik::capture::windows
