//! Everything (voidtools) IPC client: `WM_COPYDATA` + `EVERYTHING_IPC_QUERY2`, as documented in
//! the Everything SDK's `everything_ipc.h`. No DLL needed.
//!
//! A dedicated thread owns a message-only window that receives Everything's replies. Each
//! search runs two small queries back to back (most-run first, then most recently modified);
//! Everything cancels an unfinished query when a new one arrives, so they are never overlapped.

use super::FileHit;
use crate::platform::{pcwstr, wide};
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const WNDCLASS: &str = "EVERYTHING_TASKBAR_NOTIFICATION";
const WNDCLASS_15A: &str = "EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)";
const COPYDATA_QUERY2W: usize = 18;
const COPYDATA_INC_RUN_COUNTW: usize = 24;

const REQUEST_NAME: u32 = 0x1;
const REQUEST_PATH: u32 = 0x2;
const REQUEST_FULL_PATH_AND_NAME: u32 = 0x4;
const REQUEST_EXTENSION: u32 = 0x8;
const REQUEST_SIZE: u32 = 0x10;
const REQUEST_DATE_CREATED: u32 = 0x20;
const REQUEST_DATE_MODIFIED: u32 = 0x40;
const REQUEST_DATE_ACCESSED: u32 = 0x80;
const REQUEST_ATTRIBUTES: u32 = 0x100;
const REQUEST_FILE_LIST_FILE_NAME: u32 = 0x200;
const REQUEST_RUN_COUNT: u32 = 0x400;
const REQUEST_DATE_RUN: u32 = 0x800;
const REQUEST_DATE_RECENTLY_CHANGED: u32 = 0x1000;

const SORT_DATE_MODIFIED_DESCENDING: u32 = 14;
const SORT_RUN_COUNT_DESCENDING: u32 = 20;
const ITEM_FOLDER: u32 = 0x1;

const WM_START_QUERY: u32 = WM_APP + 1;
const WM_NEXT_STAGE: u32 = WM_APP + 2;
/// Replies are tagged with this base + a sequence number (the `dwData` Everything echoes back).
const REPLY_BASE: u32 = 0x534D_0000;

pub enum Event {
    Results { generation: u32, hits: Vec<FileHit> },
    /// Everything isn't running (or didn't answer).
    Unavailable { generation: u32 },
}

struct Request {
    generation: u32,
    search: String,
    max: u32,
}

static REQUEST: Mutex<Option<Request>> = Mutex::new(None);
static WORKER_HWND: AtomicIsize = AtomicIsize::new(0);
static SINK: OnceLock<Box<dyn Fn(Event) + Send + Sync>> = OnceLock::new();

/// One in-flight search: stage 0 = run-count query, stage 1 = date-modified query.
struct Active {
    generation: u32,
    search: String,
    max: u32,
    stage: u8,
    reply_id: u32,
    sent: std::time::Instant,
    hits: Vec<FileHit>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    static NEXT_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn emit(ev: Event) {
    if let Some(sink) = SINK.get() {
        sink(ev);
    }
}

pub fn spawn(sink: impl Fn(Event) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
    std::thread::Builder::new()
        .name("everything".into())
        .stack_size(256 * 1024)
        .spawn(thread_main)
        .expect("spawn everything thread");
}

/// Replaces any pending search with this one. Results arrive through the sink.
pub fn query(generation: u32, search: String, max: u32) {
    *REQUEST.lock().unwrap() = Some(Request { generation, search, max });
    let h = WORKER_HWND.load(Ordering::Acquire);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_START_QUERY, WPARAM(0), LPARAM(0));
        }
    }
}

fn everything_window() -> Option<HWND> {
    [WNDCLASS, WNDCLASS_15A].iter().find_map(|class| {
        let w = wide(class);
        unsafe { FindWindowW(pcwstr(&w), None) }.ok().filter(|h| !h.is_invalid())
    })
}

/// Tells Everything a file was opened, so it ranks higher next time (run history).
pub fn inc_run_count(path: &str) {
    let path = path.to_owned();
    let _ = std::thread::Builder::new().name("ev-runcount".into()).stack_size(64 * 1024).spawn(move || {
        let Some(ev) = everything_window() else { return };
        let data = wide(&path);
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_INC_RUN_COUNTW,
            cbData: (data.len() * 2) as u32,
            lpData: data.as_ptr() as *mut _,
        };
        unsafe {
            SendMessageTimeoutW(ev, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 1000, None);
        }
    });
}

fn thread_main() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = wide("Smowauncher.Everything");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: pcwstr(&class), ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            pcwstr(&class),
            None,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::error!("everything: CreateWindowEx failed: {e}");
                return;
            }
        };
        // We usually run elevated and Everything doesn't: allow its replies through UIPI.
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        WORKER_HWND.store(hwnd.0 as isize, Ordering::Release);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_START_QUERY => {
            if let Some(req) = REQUEST.lock().unwrap().take() {
                // A newer search supersedes whatever is in flight.
                ACTIVE.with(|a| {
                    *a.borrow_mut() = Some(Active {
                        generation: req.generation,
                        search: req.search,
                        max: req.max,
                        stage: 0,
                        reply_id: 0,
                        sent: std::time::Instant::now(),
                        hits: Vec::new(),
                    })
                });
                send_stage(hwnd);
            }
            LRESULT(0)
        }
        WM_NEXT_STAGE => {
            send_stage(hwnd);
            LRESULT(0)
        }
        WM_COPYDATA => {
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            let bytes = if cds.lpData.is_null() {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) }
            };
            on_reply(hwnd, cds.dwData as u32, bytes);
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

enum Progress {
    /// Reply for a superseded query.
    Stale,
    /// First batch arrived (generation, hits so far); query the next stage.
    Next(u32, Vec<FileHit>),
    Done(Active),
}

fn on_reply(hwnd: HWND, reply_id: u32, bytes: &[u8]) {
    let progress = ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let Some(active) = a.as_mut().filter(|x| x.reply_id == reply_id) else { return Progress::Stale };
        log::debug!("everything: stage {} replied after {:?}", active.stage, active.sent.elapsed());
        for hit in parse_list2(bytes).unwrap_or_default() {
            if !active.hits.iter().any(|h| h.path == hit.path) {
                active.hits.push(hit);
            }
        }
        active.stage += 1;
        if active.stage >= 2 {
            Progress::Done(a.take().unwrap())
        } else {
            Progress::Next(active.generation, active.hits.clone())
        }
    });
    match progress {
        Progress::Stale => {}
        // Everything replies from *inside* its handling of our query (we're still in
        // SendMessageTimeout). Querying again from here deadlocks it, so send the next
        // stage once this exchange has unwound.
        Progress::Next(generation, hits) => {
            // Show the first batch right away; the second query refines it ~25 ms later.
            emit(Event::Results { generation, hits });
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_NEXT_STAGE, WPARAM(0), LPARAM(0));
            }
        }
        Progress::Done(done) => emit(Event::Results { generation: done.generation, hits: done.hits }),
    }
}

/// Sends the query for the active search's current stage.
fn send_stage(hwnd: HWND) {
    let prepared = ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let active = a.as_mut()?;
        let id = NEXT_ID.with(|n| {
            n.set(n.get().wrapping_add(1) & 0xFFFF);
            REPLY_BASE | n.get()
        });
        active.reply_id = id;
        active.sent = std::time::Instant::now();
        let (sort, max) = match active.stage {
            0 => (SORT_RUN_COUNT_DESCENDING, (active.max / 2).max(10)),
            _ => (SORT_DATE_MODIFIED_DESCENDING, active.max),
        };
        Some((active.generation, build_query2(hwnd, id, &active.search, max, sort)))
    });
    let Some((generation, buf)) = prepared else { return };
    let Some(ev) = everything_window() else {
        log::info!("everything: not running");
        ACTIVE.with(|a| a.borrow_mut().take());
        emit(Event::Unavailable { generation });
        return;
    };
    let cds = COPYDATASTRUCT { dwData: COPYDATA_QUERY2W, cbData: buf.len() as u32, lpData: buf.as_ptr() as *mut _ };
    let ok = unsafe {
        SendMessageTimeoutW(
            ev,
            WM_COPYDATA,
            WPARAM(hwnd.0 as usize),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        )
    };
    if ok.0 == 0 {
        log::warn!("everything: query not accepted ({:?})", windows::core::Error::from_thread());
        ACTIVE.with(|a| a.borrow_mut().take());
        emit(Event::Unavailable { generation });
    }
}

/// Serializes EVERYTHING_IPC_QUERY2 (7 packed DWORDs) followed by the UTF-16 search string.
fn build_query2(reply_hwnd: HWND, reply_id: u32, search: &str, max: u32, sort: u32) -> Vec<u8> {
    let request = REQUEST_NAME | REQUEST_PATH | REQUEST_SIZE | REQUEST_DATE_MODIFIED | REQUEST_RUN_COUNT;
    let header = [reply_hwnd.0 as usize as u32, reply_id, 0, 0, max, request, sort];
    let mut buf: Vec<u8> = header.iter().flat_map(|d| d.to_le_bytes()).collect();
    buf.extend(search.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()));
    buf
}

/// Parses an EVERYTHING_IPC_LIST2 reply. Field order inside each item's data follows the
/// SDK: name, path, full path, size, extension, dates, attributes, file list name, run count, ...
pub(crate) fn parse_list2(b: &[u8]) -> Option<Vec<FileHit>> {
    let dword = |off: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?)) };
    let qword = |off: usize| -> Option<u64> { Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?)) };
    let string = |off: &mut usize| -> Option<String> {
        let len = dword(*off)? as usize;
        let start = *off + 4;
        let raw = b.get(start..start + len * 2)?;
        *off = start + (len + 1) * 2;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units))
    };

    let num_items = dword(4)? as usize;
    let flags = dword(12)?;
    let mut hits = Vec::with_capacity(num_items);
    for i in 0..num_items {
        let item = 20 + i * 8;
        let item_flags = dword(item)?;
        let mut off = dword(item + 4)? as usize;
        let (mut name, mut path, mut size, mut modified, mut run_count) = (String::new(), String::new(), 0u64, 0u64, 0u32);
        if flags & REQUEST_NAME != 0 {
            name = string(&mut off)?;
        }
        if flags & REQUEST_PATH != 0 {
            path = string(&mut off)?;
        }
        if flags & REQUEST_FULL_PATH_AND_NAME != 0 {
            string(&mut off)?;
        }
        if flags & REQUEST_SIZE != 0 {
            size = qword(off)?;
            off += 8;
        }
        if flags & REQUEST_EXTENSION != 0 {
            string(&mut off)?;
        }
        if flags & REQUEST_DATE_CREATED != 0 {
            off += 8;
        }
        if flags & REQUEST_DATE_MODIFIED != 0 {
            modified = filetime_to_unix(qword(off)?);
            off += 8;
        }
        if flags & REQUEST_DATE_ACCESSED != 0 {
            off += 8;
        }
        if flags & REQUEST_ATTRIBUTES != 0 {
            off += 4;
        }
        if flags & REQUEST_FILE_LIST_FILE_NAME != 0 {
            string(&mut off)?;
        }
        if flags & REQUEST_RUN_COUNT != 0 {
            run_count = dword(off)?;
            off += 4;
        }
        let _ = (off, REQUEST_DATE_RUN, REQUEST_DATE_RECENTLY_CHANGED);
        let full = if path.is_empty() {
            name.clone()
        } else if path.ends_with('\\') {
            format!("{path}{name}")
        } else {
            format!("{path}\\{name}")
        };
        let folder = item_flags & ITEM_FOLDER != 0;
        hits.push(FileHit { name, path: full, folder, size: if folder { 0 } else { size }, modified, run_count });
    }
    Some(hits)
}

fn filetime_to_unix(ft: u64) -> u64 {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    if ft == u64::MAX || ft < EPOCH_DIFF { 0 } else { (ft - EPOCH_DIFF) / 10_000_000 }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a LIST2 reply the way Everything does, for the flags we request.
    pub(crate) fn fake_reply(items: &[(&str, &str, bool, u64, u64, u32)]) -> Vec<u8> {
        let flags = REQUEST_NAME | REQUEST_PATH | REQUEST_SIZE | REQUEST_DATE_MODIFIED | REQUEST_RUN_COUNT;
        let mut data = Vec::new();
        let mut offsets = Vec::new();
        let header_len = 20 + items.len() * 8;
        for (name, path, _, size, unix, runs) in items {
            offsets.push((header_len + data.len()) as u32);
            for s in [name, path] {
                let u: Vec<u16> = s.encode_utf16().collect();
                data.extend((u.len() as u32).to_le_bytes());
                data.extend(u.iter().chain(std::iter::once(&0)).flat_map(|c| c.to_le_bytes()));
            }
            data.extend(size.to_le_bytes());
            data.extend((unix * 10_000_000 + 116_444_736_000_000_000).to_le_bytes());
            data.extend(runs.to_le_bytes());
        }
        let mut b = Vec::new();
        for d in [items.len() as u32, items.len() as u32, 0, flags, SORT_RUN_COUNT_DESCENDING] {
            b.extend(d.to_le_bytes());
        }
        for (i, (_, _, folder, ..)) in items.iter().enumerate() {
            b.extend((if *folder { ITEM_FOLDER } else { 0 }).to_le_bytes());
            b.extend(offsets[i].to_le_bytes());
        }
        b.extend(data);
        b
    }

    #[test]
    fn parses_list2() {
        let b = fake_reply(&[
            ("report.pdf", r"C:\Users\me\Documents", false, 1234, 1_700_000_000, 3),
            ("Projects", r"D:\", true, u64::MAX, 1_600_000_000, 0),
        ]);
        let hits = parse_list2(&b).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, r"C:\Users\me\Documents\report.pdf");
        assert_eq!((hits[0].size, hits[0].modified, hits[0].run_count), (1234, 1_700_000_000, 3));
        assert!(hits[1].folder);
        assert_eq!(hits[1].path, r"D:\Projects");
        assert_eq!(hits[1].size, 0);
    }

    #[test]
    fn rejects_truncated_reply() {
        let b = fake_reply(&[("a.txt", r"C:\x", false, 1, 1_700_000_000, 0)]);
        assert!(parse_list2(&b[..b.len() - 3]).is_none());
        assert!(parse_list2(&b[..10]).is_none());
    }

    #[test]
    fn query_layout() {
        let q = build_query2(HWND(0x1234 as *mut _), 7, "ab", 20, SORT_RUN_COUNT_DESCENDING);
        assert_eq!(q.len(), 28 + 6);
        assert_eq!(&q[0..4], &0x1234u32.to_le_bytes());
        assert_eq!(&q[4..8], &7u32.to_le_bytes());
        assert_eq!(&q[16..20], &20u32.to_le_bytes());
        assert_eq!(&q[28..], &[b'a', 0, b'b', 0, 0, 0]);
    }
}
