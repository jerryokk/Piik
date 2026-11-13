#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <d3d11_1.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

#include "capture_color.h"
#include "h264_encoder.h"

#include <array>
#include <cstdint>
#include <memory>

#include <mfidl.h>

namespace piik::capture::windows {

// GPU color/size conversion plus the synthetic frames that feed encoder probes.
class FrameConverter final {
 public:
  explicit FrameConverter(
      ID3D11Device* device,
      VideoProfile profile = kDefaultVideoProfile,
      DXGI_COLOR_SPACE_TYPE output_color = kSdrVideoColor);

  ComPtr<ID3D11Texture2D> Convert(ID3D11Texture2D* source, UINT32 width,
                                  UINT32 height, SIZE presentation,
                                  DXGI_COLOR_SPACE_TYPE input_color);

 private:
  void Configure(UINT32 width, UINT32 height, DXGI_COLOR_SPACE_TYPE input_color);

  ComPtr<ID3D11Device> device_;
  VideoProfile profile_;
  const DXGI_COLOR_SPACE_TYPE output_color_;
  std::unique_ptr<FrameConverter> rgb_converter_;
  ComPtr<ID3D11VideoDevice> video_device_;
  ComPtr<ID3D11VideoContext1> video_context_;
  ComPtr<ID3D11VideoProcessorEnumerator> enumerator_;
  ComPtr<ID3D11VideoProcessor> processor_;
  UINT32 input_width_ = 0;
  UINT32 input_height_ = 0;
  DXGI_COLOR_SPACE_TYPE input_color_ = kDesktopColor;
};

ComPtr<ID3D11Texture2D> CreateEncoderProbeTexture(
    ID3D11Device* device, FrameConverter& converter, UINT32 frame_index,
    const VideoProfile& profile);

using EncoderProbeTextures = std::array<ComPtr<ID3D11Texture2D>, 8>;

EncoderProbeTextures PrepareEncoderProbeTextures(ID3D11Device* device,
                                                const VideoProfile& profile);

// Decoder output ownership: copy the visible aperture into an unpadded NV12
// texture on the capture device, for either decoder path.
ComPtr<ID3D11Texture2D> UploadDecodedNV12(const DeviceContext& device,
                                        const BYTE* data, UINT32 width, UINT32 height);

ComPtr<ID3D11Texture2D> OwnDecodedTexture(const DeviceContext& device, IMFSample* sample,
                                         UINT32 width, UINT32 height, LONG stride,
                                         const RECT& display_area);

}  // namespace piik::capture::windows
