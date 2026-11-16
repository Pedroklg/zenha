use windows::Foundation::TimeSpan;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{BOOL, Interface, PCWSTR, factory};

use super::{GpuError, Step};

const POOL_BUFFERS: i32 = 2;

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the address of the `Vec` owned by `monitor_for_device_index`, which
    // outlives the synchronous `EnumDisplayMonitors` call that invokes this callback.
    let monitors = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    monitors.push(monitor);
    BOOL(1)
}

/// Maps the libwebrtc screen id (an `EnumDisplayDevices` index) to its monitor.
pub(super) fn monitor_for_device_index(index: u32) -> Result<HMONITOR, GpuError> {
    let mut device = DISPLAY_DEVICEW {
        cb: u32::try_from(size_of::<DISPLAY_DEVICEW>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `device` is a live struct with `cb` set to its size.
    let found = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) };
    if !found.as_bool() {
        return Err(GpuError::Unsupported("the screen has no display device"));
    }
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `monitors`, which lives until the call returns.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<HMONITOR> as isize),
        )
    };
    for monitor in monitors {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = u32::try_from(size_of::<MONITORINFOEXW>()).unwrap_or(0);
        // SAFETY: `info` is a live MONITORINFOEXW with `cbSize` set, as the API requires.
        let ok = unsafe {
            GetMonitorInfoW(
                monitor,
                &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
            )
        };
        if ok.as_bool() && info.szDevice == device.DeviceName {
            return Ok(monitor);
        }
    }
    Err(GpuError::Unsupported("no monitor matches the screen"))
}

pub(super) struct ScreenCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    size: (u32, u32),
    _item: GraphicsCaptureItem,
}

impl ScreenCapture {
    pub(super) fn new(
        device: &ID3D11Device,
        monitor: HMONITOR,
        fps: u32,
    ) -> Result<Self, GpuError> {
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .step("capture item factory")?;
        // SAFETY: `monitor` was just returned by `EnumDisplayMonitors`.
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(monitor) }.step("capture item for the monitor")?;
        let size = item.Size().step("capture item size")?;
        let dxgi: IDXGIDevice = device.cast().step("IDXGIDevice")?;
        // SAFETY: `dxgi` is a live DXGI device; the call only adds a reference to it.
        let inspectable =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.step("WinRT D3D device")?;
        let winrt_device: IDirect3DDevice = inspectable.cast().step("IDirect3DDevice")?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            POOL_BUFFERS,
            size,
        )
        .step("capture frame pool")?;
        let session = pool.CreateCaptureSession(&item).step("capture session")?;
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetMinUpdateInterval(TimeSpan {
            Duration: 7_500_000 / i64::from(fps.max(1)),
        });
        session.StartCapture().step("start capture")?;
        let width = u32::try_from(size.Width).unwrap_or(0);
        let height = u32::try_from(size.Height).unwrap_or(0);
        if width < 2 || height < 2 {
            return Err(GpuError::Unsupported("the monitor reported no size"));
        }
        Ok(Self {
            pool,
            session,
            size: (width, height),
            _item: item,
        })
    }

    pub(super) fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Copies the newest frame into `target`; returns `false` when the screen did not change.
    pub(super) fn copy_latest(
        &self,
        context: &ID3D11DeviceContext,
        target: &ID3D11Texture2D,
    ) -> Result<bool, GpuError> {
        let mut latest = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(previous) = latest.replace(frame) {
                let _ = previous.Close();
            }
        }
        let Some(frame) = latest else {
            return Ok(false);
        };
        let content = frame.ContentSize().step("frame size")?;
        if (content.Width, content.Height) != (self.size.0 as i32, self.size.1 as i32) {
            let _ = frame.Close();
            return Err(GpuError::Unsupported("the monitor resolution changed"));
        }
        let access: IDirect3DDxgiInterfaceAccess = frame
            .Surface()
            .step("frame surface")?
            .cast()
            .step("IDirect3DDxgiInterfaceAccess")?;
        // SAFETY: the surface of a D3D11 frame pool is backed by an ID3D11Texture2D.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.step("frame texture")?;
        // SAFETY: both textures are BGRA8 of the monitor size on the same device.
        unsafe { context.CopyResource(target, &texture) };
        let _ = frame.Close();
        Ok(true)
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
