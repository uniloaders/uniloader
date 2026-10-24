//! HTTPS through WinHTTP, for the update check (GET) and crash report uploads (POST).
//!
//! WinHTTP is loaded from the system directory only when a request runs, never imported: the
//! loader may be standing in for a DLL that WinHTTP itself imports, and a user who never turns
//! a feature on that needs it should not have it in the process at all.

// FFI module: WinHTTP through function pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use windows::core::{s, PCWSTR};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

/// An answer: its status, its `ETag` (empty without one) and its body.
pub(super) struct Answer {
    pub status: u32,
    pub etag: String,
    pub body: Vec<u8>,
}

type Handle = *mut c_void;
type OpenFn = unsafe extern "system" fn(PCWSTR, u32, PCWSTR, PCWSTR, u32) -> Handle;
type ConnectFn = unsafe extern "system" fn(Handle, PCWSTR, u16, u32) -> Handle;
type OpenRequestFn =
    unsafe extern "system" fn(Handle, PCWSTR, PCWSTR, PCWSTR, PCWSTR, *const PCWSTR, u32) -> Handle;
type TimeoutsFn = unsafe extern "system" fn(Handle, i32, i32, i32, i32) -> i32;
type SendFn = unsafe extern "system" fn(Handle, PCWSTR, u32, *const c_void, u32, u32, usize) -> i32;
type ReceiveFn = unsafe extern "system" fn(Handle, *mut c_void) -> i32;
type QueryFn =
    unsafe extern "system" fn(Handle, u32, PCWSTR, *mut c_void, *mut u32, *mut u32) -> i32;
type ReadFn = unsafe extern "system" fn(Handle, *mut c_void, u32, *mut u32) -> i32;
type CloseFn = unsafe extern "system" fn(Handle) -> i32;

/// The WinHTTP functions used.
struct WinHttp {
    open: OpenFn,
    connect: ConnectFn,
    open_request: OpenRequestFn,
    timeouts: TimeoutsFn,
    send: SendFn,
    receive: ReceiveFn,
    query: QueryFn,
    read: ReadFn,
    close: CloseFn,
}

impl WinHttp {
    fn load() -> Option<WinHttp> {
        let name: Vec<u16> = "winhttp.dll\0".encode_utf16().collect();
        // SAFETY: NUL terminated name; the search is limited to the system directory.
        let module =
            unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .ok()?;
        macro_rules! function {
            ($name:literal, $ty:ty) => {
                // SAFETY: the export has the signature `$ty`, as documented for WinHTTP; the
                // module stays loaded for the process.
                unsafe {
                    core::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(
                        GetProcAddress(module, s!($name))?,
                    )
                }
            };
        }
        Some(WinHttp {
            open: function!("WinHttpOpen", OpenFn),
            connect: function!("WinHttpConnect", ConnectFn),
            open_request: function!("WinHttpOpenRequest", OpenRequestFn),
            timeouts: function!("WinHttpSetTimeouts", TimeoutsFn),
            send: function!("WinHttpSendRequest", SendFn),
            receive: function!("WinHttpReceiveResponse", ReceiveFn),
            query: function!("WinHttpQueryHeaders", QueryFn),
            read: function!("WinHttpReadData", ReadFn),
            close: function!("WinHttpCloseHandle", CloseFn),
        })
    }
}

/// A WinHTTP handle closed when dropped.
struct Owned<'a>(Handle, &'a WinHttp);

impl Drop for Owned<'_> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle WinHTTP returned and nobody closed.
            unsafe { (self.1.close)(self.0) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// `https://host[:port]/path` split into its parts.
pub(super) fn split_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, 443),
    };
    (!host.is_empty()).then(|| (host.to_owned(), port, path.to_owned()))
}

const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_ETAG: u32 = 54;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

/// `method` `url` over HTTPS with `headers` (each line ending in `\r\n`) and `body`, waiting
/// `timeout_ms` for each of resolve, connect, send and receive; the answer's body is read up
/// to `max_body` bytes. Redirects are followed by WinHTTP.
pub(super) fn request(
    method: &str,
    url: &str,
    headers: &str,
    body: &[u8],
    (timeout_ms, max_body): (i32, usize),
) -> Result<Answer, String> {
    let (host, port, path) = split_url(url).ok_or("only https:// URLs are supported")?;
    let api = WinHttp::load().ok_or("winhttp.dll is not available")?;
    let agent = wide(concat!("uniloader/", env!("CARGO_PKG_VERSION")));
    // Every string below is NUL terminated and outlives its call; every handle comes from
    // WinHTTP and is closed once by `Owned`.
    let session = Owned(
        // SAFETY: see above.
        unsafe {
            (api.open)(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        },
        &api,
    );
    if session.0.is_null() {
        return Err("WinHttpOpen failed".to_owned());
    }
    // SAFETY: see above.
    unsafe { (api.timeouts)(session.0, timeout_ms, timeout_ms, timeout_ms, timeout_ms) };
    let host = wide(&host);
    // SAFETY: see above.
    let connection = Owned(
        unsafe { (api.connect)(session.0, PCWSTR(host.as_ptr()), port, 0) },
        &api,
    );
    if connection.0.is_null() {
        return Err("cannot connect".to_owned());
    }
    let (verb, path) = (wide(method), wide(&path));
    // SAFETY: see above.
    let request = Owned(
        unsafe {
            (api.open_request)(
                connection.0,
                PCWSTR(verb.as_ptr()),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                core::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        &api,
    );
    if request.0.is_null() {
        return Err("cannot open the request".to_owned());
    }
    let headers = wide(headers);
    let length = u32::try_from(body.len()).map_err(|_| "body too large")?;
    // SAFETY: see above; a header length of u32::MAX means "NUL terminated", and `body` is
    // readable for `length` bytes.
    let sent = unsafe {
        (api.send)(
            request.0,
            PCWSTR(headers.as_ptr()),
            u32::MAX,
            if body.is_empty() {
                core::ptr::null()
            } else {
                body.as_ptr().cast()
            },
            length,
            length,
            0,
        ) != 0
            && (api.receive)(request.0, core::ptr::null_mut()) != 0
    };
    if !sent {
        return Err("no answer".to_owned());
    }
    let mut status = 0u32;
    let mut size = 4u32;
    // SAFETY: `status` is writable for `size` bytes.
    unsafe {
        (api.query)(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            (&raw mut status).cast(),
            &raw mut size,
            core::ptr::null_mut(),
        )
    };
    let mut tag = [0u16; 256];
    let mut tag_size = u32::try_from(core::mem::size_of_val(&tag)).unwrap_or(0);
    // SAFETY: `tag` is writable for `tag_size` bytes.
    let has_tag = unsafe {
        (api.query)(
            request.0,
            WINHTTP_QUERY_ETAG,
            PCWSTR::null(),
            tag.as_mut_ptr().cast(),
            &raw mut tag_size,
            core::ptr::null_mut(),
        ) != 0
    };
    let etag = if has_tag {
        String::from_utf16_lossy(&tag[..(tag_size as usize / 2).min(tag.len())])
    } else {
        String::new()
    };
    let mut answer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let mut read = 0u32;
        // SAFETY: `chunk` is writable for its length.
        let ok = unsafe {
            (api.read)(
                request.0,
                chunk.as_mut_ptr().cast(),
                u32::try_from(chunk.len()).unwrap_or(0),
                &raw mut read,
            ) != 0
        };
        if !ok {
            return Err("reading the answer failed".to_owned());
        }
        if read == 0 {
            break;
        }
        answer.extend_from_slice(&chunk[..read as usize]);
        if answer.len() > max_body {
            return Err("answer too large".to_owned());
        }
    }
    Ok(Answer {
        status,
        etag,
        body: answer,
    })
}

#[cfg(test)]
mod tests {
    use super::split_url;

    #[test]
    fn urls_split() {
        assert_eq!(
            split_url("https://api.github.com/repos/a/b/releases/latest"),
            Some((
                "api.github.com".to_owned(),
                443,
                "/repos/a/b/releases/latest".to_owned()
            ))
        );
        assert_eq!(
            split_url("https://example.org:8443"),
            Some(("example.org".to_owned(), 8443, "/".to_owned()))
        );
        assert_eq!(split_url("http://example.org/x"), None);
        assert_eq!(split_url("https://:1/x"), None);
    }
}
