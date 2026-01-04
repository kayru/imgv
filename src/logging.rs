use crate::window::to_wide_string;
use env_logger;
use log;
use std::ptr::null_mut;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::processenv::SetStdHandle;
use winapi::um::winbase::{STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use winapi::um::consoleapi::AllocConsole;
use winapi::um::winnt::{FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE};

pub fn init_logging(verbose: bool) {
    let mut builder = env_logger::Builder::new();
    if std::env::var_os("RUST_LOG").is_some() {
        builder.parse_default_env();
    } else {
        let level = if verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        };
        builder.filter_level(level);
    }
    builder.format_timestamp_millis();
    let _ = builder.try_init();
}

pub fn maybe_alloc_console(requested: bool) {
    if !requested {
        return;
    }

    unsafe {
        if AllocConsole() == 0 {
            return;
        }

        let conout = to_wide_string("CONOUT$");
        let conin = to_wide_string("CONIN$");
        let out_handle = CreateFileW(
            conout.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if out_handle != INVALID_HANDLE_VALUE {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, out_handle);
            let _ = SetStdHandle(STD_ERROR_HANDLE, out_handle);
        }

        let in_handle = CreateFileW(
            conin.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if in_handle != INVALID_HANDLE_VALUE {
            let _ = SetStdHandle(STD_INPUT_HANDLE, in_handle);
        }
    }
}
