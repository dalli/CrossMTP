//! Bindgen-generated raw FFI for libmtp.
//!
//! All `unsafe`-related lints are squelched here because the file is
//! generated and we audit usages from `lib.rs`.

#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    improper_ctypes,
    clippy::all
)]

include!(concat!(env!("OUT_DIR"), "/libmtp_sys.rs"));
