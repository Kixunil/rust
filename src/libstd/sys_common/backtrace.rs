/// Common code for printing the backtrace in the same way across the different
/// supported platforms.

use crate::env;
use crate::fmt;
use crate::io;
use crate::borrow::Cow;
use crate::io::prelude::*;
use crate::path::{self, Path, PathBuf};
use crate::sys::mutex::Mutex;

use backtrace_rs::{BacktraceFmt, BytesOrWideString, PrintFmt};

/// Max number of frames to print.
const MAX_NB_FRAMES: usize = 100;

pub fn lock() -> impl Drop {
    struct Guard;
    static LOCK: Mutex = Mutex::new();

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                LOCK.unlock();
            }
        }
    }

    unsafe {
        LOCK.lock();
        return Guard;
    }
}

/// Prints the current backtrace.
pub fn print(w: &mut dyn Write, format: PrintFmt) -> io::Result<()> {
    // There are issues currently linking libbacktrace into tests, and in
    // general during libstd's own unit tests we're not testing this path. In
    // test mode immediately return here to optimize away any references to the
    // libbacktrace symbols
    if cfg!(test) {
        return Ok(());
    }

    // Use a lock to prevent mixed output in multithreading context.
    // Some platforms also requires it, like `SymFromAddr` on Windows.
    unsafe {
        let _lock = lock();
        _print(w, format)
    }
}

unsafe fn _print(w: &mut dyn Write, format: PrintFmt) -> io::Result<()> {
    struct DisplayBacktrace {
        format: PrintFmt,
    }
    impl fmt::Display for DisplayBacktrace {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            unsafe {
                _print_fmt(fmt, self.format)
            }
        }
    }
    write!(w, "{}", DisplayBacktrace { format })
}

unsafe fn _print_fmt(fmt: &mut fmt::Formatter<'_>, print_fmt: PrintFmt) -> fmt::Result {
    let cwd = env::current_dir().ok();
    let mut print_path = move |fmt: &mut fmt::Formatter<'_>, bows: BytesOrWideString<'_>| {
        output_filename(fmt, bows, print_fmt, cwd.as_ref())
    };
    let mut bt_fmt = BacktraceFmt::new(fmt, print_fmt, &mut print_path);
    bt_fmt.add_context()?;
    let mut idx = 0;
    let mut res = Ok(());
    backtrace_rs::trace_unsynchronized(|frame| {
        if print_fmt == PrintFmt::Short && idx > MAX_NB_FRAMES {
            return false;
        }

        let mut hit = false;
        let mut stop = false;
        backtrace_rs::resolve_frame_unsynchronized(frame, |symbol| {
            hit = true;
            if print_fmt == PrintFmt::Short {
                if let Some(sym) = symbol.name().and_then(|s| s.as_str()) {
                    if sym.contains("__rust_begin_short_backtrace") {
                        stop = true;
                        return;
                    }
                }
            }

            res = bt_fmt.frame().symbol(frame, symbol);
        });
        if stop {
            return false;
        }
        if !hit {
            res = bt_fmt.frame().print_raw(frame.ip(), None, None, None);
        }

        idx += 1;
        res.is_ok()
    });
    res?;
    bt_fmt.finish()?;
    if print_fmt == PrintFmt::Short {
        writeln!(
            fmt,
            "note: Some details are omitted, \
             run with `RUST_BACKTRACE=full` for a verbose backtrace."
        )?;
    }
    Ok(())
}

/// Fixed frame used to clean the backtrace with `RUST_BACKTRACE=1`.
#[inline(never)]
pub fn __rust_begin_short_backtrace<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
    F: Send,
    T: Send,
{
    f()
}

// For now logging is turned off by default, and this function checks to see
// whether the magical environment variable is present to see if it's turned on.
pub fn log_enabled() -> Option<PrintFmt> {
    use crate::sync::atomic::{self, Ordering};

    // Setting environment variables for Fuchsia components isn't a standard
    // or easily supported workflow. For now, always display backtraces.
    if cfg!(target_os = "fuchsia") {
        return Some(PrintFmt::Full);
    }

    static ENABLED: atomic::AtomicIsize = atomic::AtomicIsize::new(0);
    match ENABLED.load(Ordering::SeqCst) {
        0 => {}
        1 => return None,
        2 => return Some(PrintFmt::Short),
        _ => return Some(PrintFmt::Full),
    }

    let val = env::var_os("RUST_BACKTRACE").and_then(|x| {
        if &x == "0" {
            None
        } else if &x == "full" {
            Some(PrintFmt::Full)
        } else {
            Some(PrintFmt::Short)
        }
    });
    ENABLED.store(
        match val {
            Some(v) => v as isize,
            None => 1,
        },
        Ordering::SeqCst,
    );
    val
}

/// Prints the filename of the backtrace frame.
///
/// See also `output`.
pub fn output_filename(
    fmt: &mut fmt::Formatter<'_>,
    bows: BytesOrWideString<'_>,
    print_fmt: PrintFmt,
    cwd: Option<&PathBuf>,
) -> fmt::Result {
    let file: Cow<'_, Path> = match bows {
        #[cfg(unix)]
        BytesOrWideString::Bytes(bytes) => {
            use crate::os::unix::prelude::*;
            Path::new(crate::ffi::OsStr::from_bytes(bytes)).into()
        }
        #[cfg(not(unix))]
        BytesOrWideString::Bytes(bytes) => {
            Path::new(crate::str::from_utf8(bytes).unwrap_or("<unknown>")).into()
        }
        #[cfg(windows)]
        BytesOrWideString::Wide(wide) => {
            use crate::os::windows::prelude::*;
            Cow::Owned(crate::ffi::OsString::from_wide(wide).into())
        }
        #[cfg(not(windows))]
        BytesOrWideString::Wide(_wide) => {
            Path::new("<unknown>").into()
        }
    };
    if print_fmt == PrintFmt::Short && file.is_absolute() {
        if let Some(cwd) = cwd {
            if let Ok(stripped) = file.strip_prefix(&cwd) {
                if let Some(s) = stripped.to_str() {
                    return write!(fmt, ".{}{}", path::MAIN_SEPARATOR, s);
                }
            }
        }
    }
    fmt::Display::fmt(&file.display(), fmt)
}
