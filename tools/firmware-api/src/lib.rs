//! Firmware addresses for host tools, from the same declarations patches
//! compile against (`patches/api`).
//!
//! Each model module covers one firmware revision, the supported stock 1.10
//! MAIN image. Patched images our tools build keep that image's addresses, so
//! these are valid for any MAIN that `smoke_core::firmware::identify` accepts.
//!
//! Every function, variable and symbol becomes a `u32` constant of the same
//! name, and each model's `symbol` looks one up by name for scenario files. Function addresses are entry points with the Thumb bit cleared;
//! variables and symbols are as declared.
#![allow(non_upper_case_globals, dead_code)]

macro_rules! firmware_functions {
    ($( $(#[$meta:meta])* $vis:vis unsafe fn $name:ident ($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $result:ty,)? at $address:expr; )+) => {
        $( $(#[$meta])* $vis const $name: u32 = ($address as u32) & !1; register!($name); )+
    };
}

macro_rules! firmware_variables {
    (@parse) => {};
    (@parse $(#[$meta:meta])* $vis:vis mut $name:ident : $ty:ty, at $address:expr; $($rest:tt)*) => {
        $(#[$meta])* $vis const $name: u32 = $address as u32;
        register!($name);
        firmware_variables!(@parse $($rest)*);
    };
    (@parse $(#[$meta:meta])* $vis:vis $name:ident : $ty:ty, at $address:expr; $($rest:tt)*) => {
        $(#[$meta])* $vis const $name: u32 = $address as u32;
        register!($name);
        firmware_variables!(@parse $($rest)*);
    };
    ($($declarations:tt)*) => {
        firmware_variables!(@parse $($declarations)*);
    };
}

macro_rules! firmware_symbols {
    ($( $(#[$meta:meta])* $vis:vis $name:ident at $address:expr; )+) => {
        $( $(#[$meta])* $vis const $name: u32 = $address as u32; register!($name); )+
    };
}

/// A declared name and its address, registered by the declaration macros.
pub struct Symbol {
    /// The model module that declares it, such as `firmware_api::l6max`.
    module: &'static str,
    pub name: &'static str,
    pub address: u32,
}

inventory::collect!(Symbol);

macro_rules! register {
    ($name:ident) => {
        inventory::submit! {
            crate::Symbol { module: module_path!(), name: stringify!($name), address: $name }
        }
    };
}

/// The symbols `module` declares.
fn declared(module: &'static str) -> impl Iterator<Item = &'static Symbol> {
    inventory::iter::<Symbol>
        .into_iter()
        .filter(move |symbol| symbol.module == module)
}

/// L6 MAIN 1.10.
pub mod l6 {
    include!("../../../patches/api/l6/mod.rs");

    /// Every symbol this model declares.
    pub fn symbols() -> impl Iterator<Item = &'static crate::Symbol> {
        crate::declared(module_path!())
    }

    /// The address `name` is declared at.
    pub fn symbol(name: &str) -> Option<u32> {
        symbols()
            .find(|symbol| symbol.name == name)
            .map(|symbol| symbol.address)
    }
}

/// L6max MAIN 1.10.
pub mod l6max {
    include!("../../../patches/api/l6max/mod.rs");

    /// Every symbol this model declares.
    pub fn symbols() -> impl Iterator<Item = &'static crate::Symbol> {
        crate::declared(module_path!())
    }

    /// The address `name` is declared at.
    pub fn symbol(name: &str) -> Option<u32> {
        symbols()
            .find(|symbol| symbol.name == name)
            .map(|symbol| symbol.address)
    }
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
    fn every_declaration_registers_its_symbol_once() {
        for (model, symbols) in [
            ("l6", l6::symbols().collect::<Vec<_>>()),
            ("l6max", l6max::symbols().collect::<Vec<_>>()),
        ] {
            let mut names: Vec<_> = symbols.iter().map(|symbol| symbol.name).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), symbols.len(), "{model} registers a name twice");
            // Each declaration ends with ` at <address>;`. A linker that
            // dropped registrations would leave fewer symbols than these.
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../patches/api")
                .join(model);
            let declarations: usize = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
                .flat_map(|source| {
                    source
                        .lines()
                        .filter(|line| !line.trim_start().starts_with("//"))
                        .filter(|line| line.contains(" at ") && line.trim_end().ends_with(';'))
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .count();
            assert_eq!(symbols.len(), declarations, "{model}");
        }
        assert_eq!(l6max::symbol("system_mode"), Some(l6max::system_mode));
        assert_eq!(l6::symbol("midi_in"), Some(l6::midi_in));
        assert_eq!(l6::symbol("system_mode"), None);
        assert_eq!(
            l6max::symbol("dsp_effect_workspace"),
            Some(l6max::dsp_effect_workspace)
        );
    }
}
