//! Minimal HTTPS GET through WinHTTP (system TLS and proxy settings, no extra dependencies).

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, w};

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn check(h: *mut core::ffi::c_void, what: &str) -> Result<Handle, String> {
    if h.is_null() { Err(format!("{what}: {}", windows::core::Error::from_thread())) } else { Ok(Handle(h)) }
}

/// GET https://<url> (redirects are followed), refusing bodies over max_bytes.
pub fn get_url(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("only https URLs are supported")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    get_with_limit(host, if path.is_empty() { "/" } else { path }, max_bytes)
}

/// POST https://<host><path> and hand the response body to `on_data` as it arrives (for
/// server-sent events). `on_data` returns false to stop reading (cancel). `headers` is
/// "Name: value\r\n…". Non-200 responses become an error carrying the start of the body.
pub fn post_stream(
    host: &str,
    path: &str,
    headers: &str,
    body: &[u8],
    mut on_data: impl FnMut(&[u8]) -> bool,
) -> Result<(), String> {
    unsafe {
        let agent = HSTRING::from(concat!("Smowauncher/", env!("CARGO_PKG_VERSION")));
        let session = check(
            WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "open",
        )?;
        // Long gaps between chunks are normal while the model thinks.
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 300_000);
        let conn = check(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0), "connect")?;
        let req = check(
            WinHttpOpenRequest(
                conn.0,
                w!("POST"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "request",
        )?;
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(
            req.0,
            Some(&headers),
            Some(body.as_ptr() as *const _),
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .map_err(|e| format!("send: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("receive: {e}"))?;

        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("status: {e}"))?;

        let mut error_body = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read)
                .map_err(|e| format!("read: {e}"))?;
            if read == 0 {
                break;
            }
            let chunk = &buf[..read as usize];
            if status != 200 {
                if error_body.len() < 64 * 1024 {
                    error_body.extend_from_slice(chunk);
                }
            } else if !on_data(chunk) {
                return Ok(());
            }
        }
        if status != 200 {
            return Err(format!("HTTP {status}: {}", String::from_utf8_lossy(&error_body)));
        }
        Ok(())
    }
}

pub fn get(host: &str, path: &str) -> Result<Vec<u8>, String> {
    get_with_limit(host, path, 4 * 1024 * 1024)
}

fn get_with_limit(host: &str, path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    unsafe {
        // GitHub's API rejects requests without a User-Agent; WinHTTP sends this one.
        let agent = HSTRING::from(concat!("Smowauncher/", env!("CARGO_PKG_VERSION")));
        let session = check(
            WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "open",
        )?;
        let _ = WinHttpSetTimeouts(session.0, 5000, 5000, 10000, 30000);
        let conn = check(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0), "connect")?;
        let req = check(
            WinHttpOpenRequest(
                conn.0,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "request",
        )?;
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).map_err(|e| format!("send: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("receive: {e}"))?;

        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("status: {e}"))?;
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }

        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut available).map_err(|e| format!("read: {e}"))?;
            if available == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(req.0, body[start..].as_mut_ptr() as *mut _, available, &mut read)
                .map_err(|e| format!("read: {e}"))?;
            body.truncate(start + read as usize);
            if body.len() > max_bytes {
                return Err("response too large".into());
            }
        }
        Ok(body)
    }
}
