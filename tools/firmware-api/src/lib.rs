//! Firmware addresses for host tools, from the same declarations patches
//! compile against (`patches/api`).
//!
//! Each model module covers one firmware revision, the supported stock 1.10
//! MAIN image. Patched images our tools build keep that image's addresses, so
//! these are valid for any MAIN that `smoke_core::firmware::identify` accepts.
//!
//! Every function, variable and symbol becomes a `u32` constant of the same
//! name, and each model's `SYMBOLS` lists them by name for scenario files. Function addresses are entry points with the Thumb bit cleared;
//! variables and symbols are as declared.
#![allow(non_upper_case_globals, dead_code)]

macro_rules! firmware_functions {
    ($( $(#[$meta:meta])* $vis:vis unsafe fn $name:ident ($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $result:ty,)? at $address:expr; )+) => {
        $( $(#[$meta])* $vis const $name: u32 = ($address as u32) & !1; )+
    };
}

macro_rules! firmware_variables {
    (@parse) => {};
    (@parse $(#[$meta:meta])* $vis:vis mut $name:ident : $ty:ty, at $address:expr; $($rest:tt)*) => {
        $(#[$meta])* $vis const $name: u32 = $address as u32;
        firmware_variables!(@parse $($rest)*);
    };
    (@parse $(#[$meta:meta])* $vis:vis $name:ident : $ty:ty, at $address:expr; $($rest:tt)*) => {
        $(#[$meta])* $vis const $name: u32 = $address as u32;
        firmware_variables!(@parse $($rest)*);
    };
    ($($declarations:tt)*) => {
        firmware_variables!(@parse $($declarations)*);
    };
}

macro_rules! firmware_symbols {
    ($( $(#[$meta:meta])* $vis:vis $name:ident at $address:expr; )+) => {
        $( $(#[$meta])* $vis const $name: u32 = $address as u32; )+
    };
}

/// L6 MAIN 1.10.
pub mod l6 {
    include!("../../../patches/api/l6/mod.rs");
    include!(concat!(env!("OUT_DIR"), "/l6_symbols.rs"));
}

/// L6max MAIN 1.10.
pub mod l6max {
    include!("../../../patches/api/l6max/mod.rs");
    include!(concat!(env!("OUT_DIR"), "/l6max_symbols.rs"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functions_lose_the_thumb_bit_and_data_keeps_its_address() {
        // Declared as `midi_in ... at 0x8002_f491`.
        assert_eq!(l6::midi_in, 0x8002_f490);
        assert_eq!(l6max::current_window, 0x8020_2610);
        assert_eq!(l6max::GUI_RECORDER_WINDOW, 0x8020_0408);
    }

    #[test]
    fn symbol_tables_list_every_declaration_once_in_name_order() {
        for table in [l6::SYMBOLS, l6max::SYMBOLS] {
            assert!(table.windows(2).all(|pair| pair[0].0 < pair[1].0));
        }
        assert!(l6max::SYMBOLS.contains(&("system_mode", l6max::system_mode)));
        assert!(l6::SYMBOLS.contains(&("midi_in", l6::midi_in)));
        // An array-typed declaration, whose type contains a semicolon.
        assert!(l6max::SYMBOLS.contains(&("dsp_effect_workspace", l6max::dsp_effect_workspace)));
    }
}
