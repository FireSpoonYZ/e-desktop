use std::{path::PathBuf, ptr::null_mut};
use windows_sys::Win32::System::{
    Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
    SystemInformation::GetSystemDirectoryW,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn registry_value(name: &str, flags: u32, data: &mut [u8]) -> Result<(), String> {
    let key = wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let name_wide = wide(name);
    let mut length = data.len() as u32;
    // Public read-only API; buffers and NUL-terminated strings remain alive for the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name_wide.as_ptr(),
            flags,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(format!("RegGetValueW({name}): Win32 error {status}"));
    }
    Ok(())
}

fn registry_string(name: &str) -> Result<String, String> {
    let mut data = [0u8; 1024];
    registry_value(name, RRF_RT_REG_SZ, &mut data)?;
    let chars: Vec<_> = data
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16(&chars).map_err(|e| format!("{name}: {e}"))
}

pub fn inventory() -> Result<(), String> {
    let build = registry_string("CurrentBuildNumber")?;
    let mut ubr = [0u8; 4];
    registry_value("UBR", RRF_RT_REG_DWORD, &mut ubr)?;
    println!(
        "os_build={build}.{} arch={} pointer_bits={}",
        u32::from_le_bytes(ubr),
        std::env::consts::ARCH,
        usize::BITS
    );
    println!("display_version={:?}", registry_string("DisplayVersion")?);
    println!("build_lab={:?}", registry_string("BuildLabEx")?);
    let mut directory = [0u16; 32768];
    // Public path lookup, not a DLL load.
    let length = unsafe { GetSystemDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    if length == 0 || length as usize >= directory.len() {
        return Err("GetSystemDirectoryW failed or exceeded buffer".into());
    }
    use std::os::windows::ffi::OsStringExt;
    let directory = PathBuf::from(std::ffi::OsString::from_wide(&directory[..length as usize]));
    super::report(&directory.join("dwmapi.dll"))?;
    super::report(&directory.join("dcomp.dll"))
}
