//! windows-link 0.2.1 (microsoft/windows-rs, MIT OR Apache-2.0), changed in one way.
//!
//! Upstream links with `raw-dylib` everywhere. On the `windows-gnu` targets rustc builds those
//! import libraries by calling `dlltool`, which calls an assembler, and rustup's self-contained
//! MinGW ships no assembler, so a machine with only rustup cannot build anything that needs this
//! crate. There the macro below links against the import library that `windows-targets` carries
//! instead, which needs no external tool. Every other target behaves exactly as upstream does.
#![no_std]

/// Defines an external function to import.
#[cfg(all(windows, target_env = "gnu"))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = "windows.0.52.0")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Defines an external function to import.
#[cfg(all(windows, not(target_env = "gnu"), target_arch = "x86"))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim", import_name_type = "undecorated")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Defines an external function to import.
#[cfg(all(windows, not(target_env = "gnu"), not(target_arch = "x86")))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Defines an external function to import.
#[cfg(not(windows))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        extern $abi {
            pub fn $($function)*;
        }
    )
}
