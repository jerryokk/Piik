#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>

#include <wrl/client.h>

#include "capture_color.h"
#include "capture_error.h"
#include "h264_encoder.h"

namespace piik::capture::windows {

// Capture-device and WGC item creation shared by live capture and previews.
void EnableFastCaptureUpdates(
    const winrt::Windows::Graphics::Capture::GraphicsCaptureSession& session);
winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice
CreateCaptureDevice(ID3D11Device* device);
winrt::Windows::Graphics::Capture::GraphicsCaptureItem CreateCaptureItem(
    HWND window);
winrt::Windows::Graphics::Capture::GraphicsCaptureItem CreateCaptureItem(
    HMONITOR monitor);
ComPtr<ID3D11Texture2D> CaptureTexture(
    const winrt::Windows::Graphics::Capture::Direct3D11CaptureFrame& frame);

}  // namespace piik::capture::windows
