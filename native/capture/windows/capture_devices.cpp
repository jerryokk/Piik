#include "capture_devices.h"

#include <windows.graphics.capture.interop.h>
#include <windows.graphics.directx.direct3d11.interop.h>
#include <winrt/base.h>

#ifndef ____x_ABI_CWindows_CGraphics_CCapture_CIGraphicsCaptureSession5_INTERFACE_DEFINED__
#define ____x_ABI_CWindows_CGraphics_CCapture_CIGraphicsCaptureSession5_INTERFACE_DEFINED__
namespace ABI::Windows::Graphics::Capture {
MIDL_INTERFACE("67C0EA62-1F85-5061-925A-239BE0AC09CB")
IGraphicsCaptureSession5 : public IInspectable {
 public:
  IFACEMETHOD(get_MinUpdateInterval)(
      ABI::Windows::Foundation::TimeSpan* value) = 0;
  IFACEMETHOD(put_MinUpdateInterval)(
      ABI::Windows::Foundation::TimeSpan value) = 0;
};
}  // namespace ABI::Windows::Graphics::Capture
#endif

// WGC device/session setup for live capture; the fixture build omits it.
namespace piik::capture::windows {
namespace {

using Microsoft::WRL::ComPtr;

constexpr LONGLONG kFastCaptureUpdateInterval100ns = 40'000;

}  // namespace

void EnableFastCaptureUpdates(
    const winrt::Windows::Graphics::Capture::GraphicsCaptureSession& session) {
  ComPtr<ABI::Windows::Graphics::Capture::IGraphicsCaptureSession5> session5;
  auto* inspectable = reinterpret_cast<IInspectable*>(winrt::get_abi(session));
  if (SUCCEEDED(inspectable->QueryInterface(IID_PPV_ARGS(&session5)))) {
    // Zero is not an unlimited rate on current Windows. Keep WGC faster than
    // every supported profile and let the output cadence own the exact cap.
    ABI::Windows::Foundation::TimeSpan interval{kFastCaptureUpdateInterval100ns};
    (void)session5->put_MinUpdateInterval(interval);
  }
}

winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice
CreateCaptureDevice(ID3D11Device* device) {
  ComPtr<IDXGIDevice> dxgi_device;
  Check(device->QueryInterface(IID_PPV_ARGS(&dxgi_device)),
        "capture-dxgi-device");
  winrt::com_ptr<IInspectable> inspectable;
  Check(CreateDirect3D11DeviceFromDXGIDevice(dxgi_device.Get(),
                                             inspectable.put()),
        "capture-winrt-device");
  return inspectable.as<
      winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice>();
}

winrt::Windows::Graphics::Capture::GraphicsCaptureItem CreateCaptureItem(
    HWND window) {
  using winrt::Windows::Graphics::Capture::GraphicsCaptureItem;
  auto interop = winrt::get_activation_factory<GraphicsCaptureItem,
                                                IGraphicsCaptureItemInterop>();
  GraphicsCaptureItem item{nullptr};
  Check(interop->CreateForWindow(window, winrt::guid_of<GraphicsCaptureItem>(),
                                 winrt::put_abi(item)),
        "capture-item-window");
  return item;
}

winrt::Windows::Graphics::Capture::GraphicsCaptureItem CreateCaptureItem(
    HMONITOR monitor) {
  using winrt::Windows::Graphics::Capture::GraphicsCaptureItem;
  auto interop = winrt::get_activation_factory<GraphicsCaptureItem,
                                                IGraphicsCaptureItemInterop>();
  GraphicsCaptureItem item{nullptr};
  Check(interop->CreateForMonitor(monitor, winrt::guid_of<GraphicsCaptureItem>(),
                                  winrt::put_abi(item)),
        "capture-item-display");
  return item;
}

ComPtr<ID3D11Texture2D> CaptureTexture(
    const winrt::Windows::Graphics::Capture::Direct3D11CaptureFrame& frame) {
  auto access = frame.Surface().as<
      ::Windows::Graphics::DirectX::Direct3D11::IDirect3DDxgiInterfaceAccess>();
  ComPtr<ID3D11Texture2D> texture;
  Check(access->GetInterface(IID_PPV_ARGS(&texture)),
        "capture-frame-texture");
  return texture;
}

}  // namespace piik::capture::windows
