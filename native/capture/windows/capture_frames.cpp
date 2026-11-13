#include "capture_frames.h"

#include "capture_geometry.h"
#include "video_bounds.h"

#include <algorithm>
#include <cstdint>
#include <vector>

// D3D11 video-processor conversion and synthetic probe frame creation.
namespace piik::capture::windows {

FrameConverter::FrameConverter(ID3D11Device* device, VideoProfile profile,
                               DXGI_COLOR_SPACE_TYPE output_color)
    : device_(device), profile_(profile), output_color_(output_color) {
  Check(device_->QueryInterface(IID_PPV_ARGS(&video_device_)),
        "video-processor-device");
  ComPtr<ID3D11DeviceContext> context;
  device_->GetImmediateContext(&context);
  Check(context.As(&video_context_), "video-processor-context");
}

ComPtr<ID3D11Texture2D> FrameConverter::Convert(ID3D11Texture2D* source,
                                                UINT32 width, UINT32 height,
                                                SIZE presentation,
                                                DXGI_COLOR_SPACE_TYPE input_color) {
  if (source == nullptr || width == 0 || height == 0 ||
      width > 16'384 || height > 16'384) {
    Fail("capture-size", "captured window dimensions are invalid");
  }
  ComPtr<ID3D11Texture2D> normalized;
  if (output_color_ == kSdrVideoColor && input_color != kDesktopColor &&
      input_color != kSdrVideoColor) {
    // YUV-to-YUV matrix/range changes are not reliable across tested drivers,
    // even when reported as supported. Normalize through RGB on the GPU;
    // ordinary RGB capture and canonical NV12 scaling stay single-pass.
    if (!rgb_converter_)
      rgb_converter_ = std::make_unique<FrameConverter>(device_.Get(), profile_, kDesktopColor);
    normalized = rgb_converter_->Convert(source, width, height, presentation, input_color);
    source = normalized.Get();
    width = profile_.width;
    height = profile_.height;
    presentation = {static_cast<LONG>(width), static_cast<LONG>(height)};
    input_color = kDesktopColor;
  }
  if (!enumerator_ || width != input_width_ || height != input_height_ ||
      input_color != input_color_) {
    Configure(width, height, input_color);
  }

  D3D11_TEXTURE2D_DESC output_description = {};
  output_description.Width = profile_.width;
  output_description.Height = profile_.height;
  output_description.MipLevels = 1;
  output_description.ArraySize = 1;
  output_description.Format = output_color_ == kDesktopColor
      ? DXGI_FORMAT_B8G8R8A8_UNORM : DXGI_FORMAT_NV12;
  output_description.SampleDesc.Count = 1;
  output_description.Usage = D3D11_USAGE_DEFAULT;
  output_description.BindFlags = D3D11_BIND_RENDER_TARGET;
  ComPtr<ID3D11Texture2D> output;
  Check(device_->CreateTexture2D(&output_description, nullptr, &output),
        "video-processor-output-texture");

  D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC input_description = {};
  input_description.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
  input_description.Texture2D.ArraySlice = 0;
  input_description.Texture2D.MipSlice = 0;
  ComPtr<ID3D11VideoProcessorInputView> input_view;
  Check(video_device_->CreateVideoProcessorInputView(
            source, enumerator_.Get(), &input_description, &input_view),
        "video-processor-input-view");

  D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC output_view_description = {};
  output_view_description.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
  output_view_description.Texture2D.MipSlice = 0;
  ComPtr<ID3D11VideoProcessorOutputView> output_view;
  Check(video_device_->CreateVideoProcessorOutputView(
            output.Get(), enumerator_.Get(), &output_view_description,
            &output_view),
        "video-processor-output-view");

  RECT source_rect = {0, 0, static_cast<LONG>(width),
                      static_cast<LONG>(height)};
  RECT target_rect = piik::capture::FitFrameRect(
      presentation, {static_cast<LONG>(profile_.width),
                     static_cast<LONG>(profile_.height)});
  RECT output_rect = {0, 0, static_cast<LONG>(profile_.width),
                      static_cast<LONG>(profile_.height)};

  D3D11_VIDEO_COLOR background = {};
  background.RGBA.A = 1.0f;
  video_context_->VideoProcessorSetOutputBackgroundColor(processor_.Get(),
                                                          FALSE, &background);
  video_context_->VideoProcessorSetOutputTargetRect(processor_.Get(), TRUE,
                                                     &output_rect);
  video_context_->VideoProcessorSetStreamFrameFormat(
      processor_.Get(), 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
  video_context_->VideoProcessorSetStreamSourceRect(processor_.Get(), 0,
                                                     TRUE, &source_rect);
  video_context_->VideoProcessorSetStreamDestRect(processor_.Get(), 0, TRUE,
                                                   &target_rect);
  video_context_->VideoProcessorSetStreamAutoProcessingMode(processor_.Get(),
                                                            0, FALSE);
  D3D11_VIDEO_PROCESSOR_STREAM stream = {};
  stream.Enable = TRUE;
  stream.pInputSurface = input_view.Get();
  Check(video_context_->VideoProcessorBlt(processor_.Get(), output_view.Get(),
                                           0, 1, &stream),
        "video-processor-blit");
  return output;
}

void FrameConverter::Configure(UINT32 width, UINT32 height,
                               DXGI_COLOR_SPACE_TYPE input_color) {
  D3D11_VIDEO_PROCESSOR_CONTENT_DESC description = {};
  description.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
  description.InputFrameRate = {profile_.frame_rate, 1};
  description.InputWidth = width;
  description.InputHeight = height;
  description.OutputFrameRate = {profile_.frame_rate, 1};
  description.OutputWidth = profile_.width;
  description.OutputHeight = profile_.height;
  description.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
  ComPtr<ID3D11VideoProcessorEnumerator> enumerator;
  Check(video_device_->CreateVideoProcessorEnumerator(&description,
                                                       &enumerator),
        "video-processor-enumerator");
  ComPtr<ID3D11VideoProcessor> processor;
  Check(video_device_->CreateVideoProcessor(enumerator.Get(), 0, &processor),
        "video-processor-create");
  // The incoming frame owns its range/matrix. Never infer these from size.
  video_context_->VideoProcessorSetStreamColorSpace1(processor.Get(), 0, input_color);
  video_context_->VideoProcessorSetOutputColorSpace1(processor.Get(), output_color_);
  enumerator_ = std::move(enumerator);
  processor_ = std::move(processor);
  input_width_ = width;
  input_height_ = height;
  input_color_ = input_color;
}

ComPtr<ID3D11Texture2D> CreateEncoderProbeTexture(
    ID3D11Device* device, FrameConverter& converter, UINT32 frame_index,
    const VideoProfile& profile) {
  std::vector<UINT32> pixels(static_cast<size_t>(profile.width) * profile.height);
  const UINT32 bar_start = (frame_index * 11) % profile.width;
  for (UINT32 y = 0; y < profile.height; ++y) {
    for (UINT32 x = 0; x < profile.width; ++x) {
      const UINT32 distance = (x + profile.width - bar_start) % profile.width;
      const UINT32 base = 16 + ((x / 8 + y / 8 + frame_index) % 180);
      pixels[static_cast<size_t>(y) * profile.width + x] = distance < 96
          ? 0xffebebeb : 0xff000000 | base |
              ((96 + frame_index % 64) << 8) | ((160 - frame_index % 64) << 16);
    }
  }
  D3D11_TEXTURE2D_DESC description{};
  description.Width = profile.width;
  description.Height = profile.height;
  description.MipLevels = description.ArraySize = description.SampleDesc.Count = 1;
  description.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  description.Usage = D3D11_USAGE_DEFAULT;
  description.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
  D3D11_SUBRESOURCE_DATA initial{};
  initial.pSysMem = pixels.data();
  initial.SysMemPitch = profile.width * 4;
  ComPtr<ID3D11Texture2D> source;
  Check(device->CreateTexture2D(&description, &initial, &source), "codec-probe-source");
  // Match live capture's GPU-produced NV12 input instead of probing a separate
  // initialized NV12 upload path. Each conversion owns its output surface.
  return converter.Convert(source.Get(), profile.width, profile.height,
      {static_cast<LONG>(profile.width), static_cast<LONG>(profile.height)}, kDesktopColor);
}

EncoderProbeTextures PrepareEncoderProbeTextures(ID3D11Device* device,
                                                const VideoProfile& profile) {
  FrameConverter converter(device, profile);
  EncoderProbeTextures textures;
  for (UINT32 index = 0; index < textures.size(); ++index)
    textures[index] = CreateEncoderProbeTexture(device, converter, index * 7, profile);
  return textures;
}

ComPtr<ID3D11Texture2D> UploadDecodedNV12(const DeviceContext& device,
                                        const BYTE* data, UINT32 width, UINT32 height) {
  D3D11_TEXTURE2D_DESC description{};
  description.Width = width;
  description.Height = height;
  description.MipLevels = description.ArraySize = 1;
  description.Format = DXGI_FORMAT_NV12;
  description.SampleDesc.Count = 1;
  description.Usage = D3D11_USAGE_DEFAULT;
  description.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
  D3D11_SUBRESOURCE_DATA input{};
  input.pSysMem = data;
  input.SysMemPitch = width;
  input.SysMemSlicePitch = width * height * 3 / 2;
  ComPtr<ID3D11Texture2D> texture;
  Check(device.device->CreateTexture2D(&description, &input, &texture), "decoded-input-upload");
  return texture;
}

ComPtr<ID3D11Texture2D> OwnDecodedTexture(const DeviceContext& device, IMFSample* sample,
                                         UINT32 width, UINT32 height, LONG stride,
                                         const RECT& display_area) {
  // Copy the visible aperture while taking ownership of the decoder's storage.
  // Downstream converters receive ordinary unpadded NV12, on either decoder path.
  auto copy_visible = [&](ID3D11Texture2D* original, UINT subresource) {
    D3D11_TEXTURE2D_DESC description{};
    original->GetDesc(&description);
    if (description.Format != DXGI_FORMAT_NV12 || description.Width < width || description.Height < height ||
        !piik::capture::ValidCodedVideoSize(description.Width, description.Height))
      Fail("decoded-texture-size", "decoder texture exceeds source bounds");
    description.Width = display_area.right - display_area.left;
    description.Height = display_area.bottom - display_area.top;
    description.ArraySize = description.MipLevels = 1;
    description.Usage = D3D11_USAGE_DEFAULT;
    description.CPUAccessFlags = description.MiscFlags = 0;
    description.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    ComPtr<ID3D11Texture2D> owned;
    Check(device.device->CreateTexture2D(&description, nullptr, &owned), "decoded-owned-texture");
    const D3D11_BOX source{static_cast<UINT>(display_area.left), static_cast<UINT>(display_area.top), 0,
        static_cast<UINT>(display_area.right), static_cast<UINT>(display_area.bottom), 1};
    device.context->CopySubresourceRegion(owned.Get(), 0, 0, 0, 0, original, subresource, &source);
    return owned;
  };
  ComPtr<IMFMediaBuffer> buffer;
  Check(sample->GetBufferByIndex(0, &buffer), "decoded-buffer");
  ComPtr<IMFDXGIBuffer> dxgi;
  if (SUCCEEDED(buffer.As(&dxgi))) {
    ComPtr<ID3D11Texture2D> original;
    UINT subresource = 0;
    Check(dxgi->GetResource(IID_PPV_ARGS(&original)), "decoded-dxgi-resource");
    Check(dxgi->GetSubresourceIndex(&subresource), "decoded-dxgi-subresource");
    return copy_visible(original.Get(), subresource);
  }
  const DWORD size = width * height * 3 / 2;
  std::vector<BYTE> pixels(size);
  ComPtr<IMF2DBuffer> planar;
  if (SUCCEEDED(buffer.As(&planar))) {
    DWORD length = 0;
    Check(planar->GetContiguousLength(&length), "decoded-contiguous-length");
    if (length != size) Fail("decoded-contiguous-size", "NV12 output does not match its dimensions");
    Check(planar->ContiguousCopyTo(pixels.data(), size), "decoded-contiguous-copy");
  } else {
    BYTE* data = nullptr;
    DWORD length = 0;
    Check(buffer->Lock(&data, nullptr, &length), "decoded-memory-lock");
    const bool valid = stride >= static_cast<LONG>(width) &&
        static_cast<UINT64>(stride) * height * 3 / 2 <= length;
    if (valid) {
      for (UINT32 row = 0; row < height * 3 / 2; ++row) {
        std::copy_n(data + static_cast<size_t>(row) * stride, width, pixels.data() + static_cast<size_t>(row) * width);
      }
    }
    Check(buffer->Unlock(), "decoded-memory-unlock");
    if (!valid) Fail("decoded-memory-size", "NV12 output buffer is incomplete");
  }
  auto uploaded = UploadDecodedNV12(device, pixels.data(), width, height);
  if (display_area.left == 0 && display_area.top == 0 &&
      display_area.right == static_cast<LONG>(width) && display_area.bottom == static_cast<LONG>(height))
    return uploaded;
  return copy_visible(uploaded.Get(), 0);
}

}  // namespace piik::capture::windows
