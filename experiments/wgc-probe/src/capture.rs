use super::Options;
use std::{
    error::Error,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{E_POINTER, HMODULE, HWND},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
                D3D11CreateDevice, ID3D11Device, ID3D11Texture2D,
            },
            Dxgi::IDXGIDevice,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
        },
        UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetWindowThreadProcessId, IsWindow},
    },
    core::{Interface, factory},
};

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const BUFFERS: i32 = 2;

struct Runtime;
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: Option<GraphicsCaptureSession>,
}
impl Drop for Capture {
    fn drop(&mut self) {
        // Stop production before returning/releasing the pool's textures, including on errors.
        if let Some(session) = &self.session {
            if let Err(error) = session.Close() {
                eprintln!("session.Close: {error}");
            }
        }
        if let Err(error) = self.pool.Close() {
            eprintln!("pool.Close: {error}");
        }
    }
}

fn device() -> windows::core::Result<IDirect3DDevice> {
    let mut native: Option<ID3D11Device> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut native),
            None,
            None,
        )?;
        let dxgi: IDXGIDevice = native
            .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?
            .cast()?;
        CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()
    }
}

// Texture/surface references are local: none survive frame.Close or pool.Recreate.
fn inspect_frame(frame: &Direct3D11CaptureFrame, count: u64) -> Result<SizeInt32, Box<dyn Error>> {
    let size = frame.ContentSize()?;
    let timestamp = frame.SystemRelativeTime()?.Duration;
    let surface = frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    if desc.Width == 0 || desc.Height == 0 {
        return Err("WGC returned an empty D3D11 texture".into());
    }
    println!(
        "frame={count} content={}x{} texture={}x{} format={} timestamp_100ns={timestamp}",
        size.Width, size.Height, desc.Width, desc.Height, desc.Format.0
    );
    Ok(size)
}

pub(super) fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let hwnd = HWND(options.hwnd as *mut std::ffi::c_void);
    let mut pid = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool()
        || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
        || tid == 0
        || pid == 0
    {
        return Err("HWND must identify a live top-level window".into());
    }

    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
    let _runtime = Runtime; // Declared first: all capture/COM objects drop before RoUninitialize.
    if !GraphicsCaptureSession::IsSupported()? {
        return Err("Windows Graphics Capture is unsupported on this system".into());
    }
    let interop: IGraphicsCaptureItemInterop = factory::<GraphicsCaptureItem, _>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
    let mut pool_size = item.Size()?;
    if pool_size.Width <= 0 || pool_size.Height <= 0 {
        return Err("Capture item has no positive initial size; restore it before probing".into());
    }
    let device = device()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, FORMAT, BUFFERS, pool_size)?;
    let mut capture = Capture {
        pool,
        session: None,
    };
    capture.session = Some(capture.pool.CreateCaptureSession(&item)?);
    println!(
        "hwnd={:#x} pid={pid} item={} initial={}x{} duration_ms={}",
        options.hwnd,
        item.DisplayName()?,
        pool_size.Width,
        pool_size.Height,
        options.duration_ms
    );
    capture.session.as_ref().unwrap().StartCapture()?;
    let started = Instant::now();
    let duration = Duration::from_millis(options.duration_ms);
    let mut count = 0u64;
    while started.elapsed() < duration {
        let mut current_pid = 0;
        let current_tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut current_pid)) };
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || current_pid != pid || current_tid != tid {
            return Err(format!("Source closed/changed after {count} frames").into());
        }
        match capture.pool.TryGetNextFrame() {
            Ok(frame) => {
                let inspected = inspect_frame(&frame, count + 1);
                let closed = frame.Close();
                drop(frame);
                let size = inspected?;
                closed?;
                count += 1;
                if size.Width > 0
                    && size.Height > 0
                    && (size.Width != pool_size.Width || size.Height != pool_size.Height)
                {
                    capture.pool.Recreate(&device, FORMAT, BUFFERS, size)?;
                    pool_size = size;
                }
            }
            // windows 0.61 maps a successful null result (no frame yet) to Error::empty.
            Err(error) if error.code().is_ok() => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(10).min(duration.saturating_sub(started.elapsed())));
    }
    println!(
        "summary frames={count} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    if count == 0 {
        return Err("Timed out without a frame; minimized, protected or unsupported targets may not produce frames".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_null_frame_has_empty_success_code() {
        // Matches TryGetNextFrame's projection without creating a pool/device or calling COM.
        let result: windows::core::Result<Direct3D11CaptureFrame> =
            unsafe { windows::core::Type::from_abi(std::ptr::null_mut()) };
        assert!(result.unwrap_err().code().is_ok());
        assert!(!windows::core::Error::from_hresult(E_POINTER).code().is_ok());
    }
}
