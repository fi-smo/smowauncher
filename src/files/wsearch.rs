//! Fallback file search through the Windows Search index (the one Explorer's search box
//! uses), for machines without Everything. It covers the indexed locations — the user's
//! libraries, Desktop, Downloads, Start menu — not whole drives.
//!
//! OLE DB and the search provider pull ~15 MB of DLLs into a process, so queries run in a
//! helper (`smowauncher --wsearch`) that lives only while the launcher is in use.
//! Protocol (one line each): request `<generation>\t<max>\t<text>`, reply `R <generation> <json>`.

use super::FileHit;
use super::everything::Event;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Search::*;
use windows::core::{GUID, HSTRING, IUnknown, Interface, w};

/// {C8B521FB-5CF3-11CE-ADE5-00AA0044773D}: the provider's default SQL dialect.
const DBGUID_DEFAULT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);
const DBSTATUS_S_OK: u32 = 0;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The helper exits by itself if the launcher stops talking to it.
const HELPER_IDLE_SECS: u64 = 60;

/// Windows Search SQL for a launcher query: every word must prefix-match a word of the
/// file name. Characters with meaning in the query syntax are dropped.
pub fn build_sql(text: &str, max: u32) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !"'\"*%()[];".contains(*c)).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    let conditions: Vec<String> = words.iter().map(|w| format!("CONTAINS(System.FileName, '\"{w}*\"')")).collect();
    Some(format!(
        "SELECT TOP {max} System.ItemPathDisplay, System.ItemType, System.DateModified, System.Size \
         FROM SystemIndex WHERE SCOPE='file:' AND {} ORDER BY System.DateModified DESC",
        conditions.join(" AND ")
    ))
}

/// One fetched row, laid out for the OLE DB accessor below.
#[repr(C)]
struct RowBuf {
    path_status: u32,
    path_len: usize,
    path: [u16; 1024],
    type_status: u32,
    type_len: usize,
    kind: [u16; 64],
    modified_status: u32,
    modified: u64,
    size_status: u32,
    size: i64,
}

fn binding(ordinal: usize, value: usize, len: Option<usize>, status: usize, max: usize, ty: u16) -> DBBINDING {
    DBBINDING {
        iOrdinal: ordinal,
        obValue: value,
        obLength: len.unwrap_or(0),
        obStatus: status,
        pTypeInfo: std::mem::ManuallyDrop::new(None),
        pObject: std::ptr::null_mut(),
        pBindExt: std::ptr::null_mut(),
        dwPart: (DBPART_VALUE.0 | DBPART_STATUS.0 | if len.is_some() { DBPART_LENGTH.0 } else { 0 }) as u32,
        dwMemOwner: DBMEMOWNER_CLIENTOWNED.0 as u32,
        eParamIO: DBPARAMIO_NOTPARAM.0 as u32,
        cbMaxLen: max,
        dwFlags: 0,
        wType: ty,
        bPrecision: 0,
        bScale: 0,
    }
}

fn wide_str(buf: &[u16], len_bytes: usize) -> String {
    let n = (len_bytes / 2).min(buf.len());
    let end = buf[..n].iter().position(|&c| c == 0).unwrap_or(n);
    String::from_utf16_lossy(&buf[..end])
}

pub struct Connection {
    commands: IDBCreateCommand,
}

impl Connection {
    pub fn open() -> windows::core::Result<Self> {
        unsafe {
            let init: IDataInitialize = CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER)?;
            let mut source: Option<IUnknown> = None;
            init.GetDataSource(
                None,
                CLSCTX_INPROC_SERVER.0,
                w!("Provider=Search.CollatorDSO;Extended Properties='Application=Windows';"),
                &IDBInitialize::IID,
                &mut source,
            )?;
            let db: IDBInitialize = source.ok_or(windows::core::Error::empty())?.cast()?;
            db.Initialize()?;
            let session = db.cast::<IDBCreateSession>()?.CreateSession(None, &IDBCreateCommand::IID)?;
            Ok(Self { commands: session.cast()? })
        }
    }

    pub fn search(&self, text: &str, max: u32) -> windows::core::Result<Vec<FileHit>> {
        let Some(sql) = build_sql(text, max) else { return Ok(Vec::new()) };
        unsafe {
            let cmd: ICommandText = self.commands.CreateCommand(None, &ICommandText::IID)?.cast()?;
            cmd.SetCommandText(&DBGUID_DEFAULT, &HSTRING::from(sql))?;
            let mut rows: Option<IUnknown> = None;
            cmd.Execute(None, &IRowset::IID, None, None, Some(&mut rows))?;
            let rowset: IRowset = rows.ok_or(windows::core::Error::empty())?.cast()?;
            let accessor: IAccessor = rowset.cast()?;

            use std::mem::offset_of;
            let bindings = [
                binding(1, offset_of!(RowBuf, path), Some(offset_of!(RowBuf, path_len)), offset_of!(RowBuf, path_status), 2048, DBTYPE_WSTR.0 as u16),
                binding(2, offset_of!(RowBuf, kind), Some(offset_of!(RowBuf, type_len)), offset_of!(RowBuf, type_status), 128, DBTYPE_WSTR.0 as u16),
                binding(3, offset_of!(RowBuf, modified), None, offset_of!(RowBuf, modified_status), 8, DBTYPE_FILETIME.0 as u16),
                binding(4, offset_of!(RowBuf, size), None, offset_of!(RowBuf, size_status), 8, DBTYPE_I8.0 as u16),
            ];
            let mut handle = HACCESSOR::default();
            accessor.CreateAccessor(
                DBACCESSOR_ROWDATA.0 as u32,
                bindings.len(),
                bindings.as_ptr(),
                size_of::<RowBuf>(),
                &mut handle,
                None,
            )?;

            let mut hits = Vec::new();
            let mut row = Box::new(std::mem::zeroed::<RowBuf>());
            loop {
                // The binding folds cRows into the slice length; the provider writes the
                // row handles into the buffer the first pointer points at.
                let mut handles = [0usize; 32];
                let mut ptrs = [handles.as_mut_ptr(); 32];
                let mut obtained = 0usize;
                if rowset.GetNextRows(0, 0, &mut obtained, &mut ptrs).is_err() || obtained == 0 {
                    break;
                }
                for &h in &handles[..obtained] {
                    *row = std::mem::zeroed();
                    if rowset.GetData(h, handle, &mut *row as *mut RowBuf as *mut _).is_err() || row.path_status != DBSTATUS_S_OK {
                        continue;
                    }
                    let path = wide_str(&row.path, row.path_len);
                    if path.is_empty() {
                        continue;
                    }
                    let kind = if row.type_status == DBSTATUS_S_OK { wide_str(&row.kind, row.type_len) } else { String::new() };
                    let folder = kind.eq_ignore_ascii_case("directory");
                    let name = path.rsplit('\\').next().unwrap_or(&path).to_owned();
                    const EPOCH: u64 = 116_444_736_000_000_000;
                    let modified = if row.modified_status == DBSTATUS_S_OK && row.modified > EPOCH {
                        (row.modified - EPOCH) / 10_000_000
                    } else {
                        0
                    };
                    let size = if row.size_status == DBSTATUS_S_OK && !folder { row.size.max(0) as u64 } else { 0 };
                    hits.push(FileHit { name, path, folder, size, modified, run_count: 0 });
                }
                let _ = rowset.ReleaseRows(obtained, handles.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
                if obtained < handles.len() {
                    break;
                }
            }
            let _ = accessor.ReleaseAccessor(handle, None);
            Ok(hits)
        }
    }
}

// ------------------------------------------------------------------------ helper process

/// `smowauncher --wsearch`: answers queries from stdin until it closes (or stays idle).
pub fn serve() -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let conn = match Connection::open() {
        Ok(c) => c,
        Err(e) => {
            log::error!("wsearch: cannot open the Windows Search index: {e}");
            println!("E {e}");
            return 1;
        }
    };
    // Only the newest request matters: the reader keeps overwriting it while a search runs.
    let latest: Arc<(Mutex<Option<(u32, u32, String)>>, Condvar)> = Arc::new((Mutex::new(None), Condvar::new()));
    let reader = latest.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let mut parts = line.splitn(3, '\t');
            if let (Some(g), Some(m), Some(t)) = (parts.next(), parts.next(), parts.next())
                && let (Ok(g), Ok(m)) = (g.parse(), m.parse())
            {
                *reader.0.lock().unwrap() = Some((g, m, t.to_owned()));
                reader.1.notify_one();
            }
        }
        std::process::exit(0); // launcher went away
    });
    let (lock, cvar) = &*latest;
    loop {
        let mut guard = lock.lock().unwrap();
        let (g2, timeout) = cvar.wait_timeout_while(guard, std::time::Duration::from_secs(HELPER_IDLE_SECS), |r| r.is_none()).unwrap();
        guard = g2;
        if timeout.timed_out() {
            return 0;
        }
        let (generation, max, text) = guard.take().unwrap();
        drop(guard);
        let t = std::time::Instant::now();
        let hits = conn.search(&text, max).unwrap_or_else(|e| {
            log::warn!("wsearch: query failed: {e}");
            Vec::new()
        });
        log::debug!("wsearch: {} hits for {text:?} in {:?}", hits.len(), t.elapsed());
        let json = serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into());
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "R {generation} {json}");
        let _ = out.flush();
    }
}

// ------------------------------------------------------------------------ client (launcher)

struct Helper {
    child: Child,
    stdin: ChildStdin,
}

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
static SINK: OnceLock<Box<dyn Fn(Event) + Send + Sync>> = OnceLock::new();

pub fn set_sink(sink: impl Fn(Event) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

fn start_helper() -> Option<Helper> {
    let exe = std::env::current_exe().ok()?;
    let mut child = Command::new(exe)
        .arg("--wsearch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| log::error!("wsearch: cannot start helper: {e}"))
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    std::thread::Builder::new()
        .name("wsearch-pipe".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Some(rest) = line.strip_prefix("R ") else { continue };
                let Some((generation, json)) = rest.split_once(' ') else { continue };
                let (Ok(generation), Ok(hits)) = (generation.parse(), serde_json::from_str::<Vec<FileHit>>(json)) else { continue };
                if let Some(sink) = SINK.get() {
                    sink(Event::Results { generation, hits });
                }
            }
        })
        .ok()?;
    Some(Helper { child, stdin })
}

/// Queues a search; results arrive through the sink as `Event::Results`.
pub fn query(generation: u32, text: &str, max: u32) {
    let mut helper = HELPER.lock().unwrap();
    if helper.is_none() {
        *helper = start_helper();
    }
    let clean: String = text.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect();
    let ok = helper.as_mut().is_some_and(|h| writeln!(h.stdin, "{generation}\t{max}\t{clean}").and_then(|_| h.stdin.flush()).is_ok());
    if !ok {
        // The helper died (or never started); retry with a fresh one next time.
        if let Some(mut h) = helper.take() {
            let _ = h.child.kill();
        }
    }
}

/// Ends the helper (called when the launcher hides, to give its memory back).
pub fn stop() {
    if let Some(mut h) = HELPER.lock().unwrap().take() {
        let _ = h.child.kill();
        let _ = h.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql() {
        let s = build_sql("q3 rep'ort", 20).unwrap();
        assert!(s.starts_with("SELECT TOP 20 System.ItemPathDisplay"));
        assert!(s.contains("CONTAINS(System.FileName, '\"q3*\"') AND CONTAINS(System.FileName, '\"report*\"')"));
        assert!(build_sql("  '\" ", 5).is_none());
    }
}
