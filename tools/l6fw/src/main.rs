use l6fw::{repack, unpack};
use std::{env, path::Path, process::ExitCode};
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [command, source, output] if command == "unpack" => {
            unpack(Path::new(source), Path::new(output))
        }
        [command, input, output] if command == "repack" => {
            repack(Path::new(input), Path::new(output), false)
        }
        [command, input, output, flag] if command == "repack" && flag == "--recompute-checksum" => {
            repack(Path::new(input), Path::new(output), true)
        }
        [command, input, output] if command == "update-checksum" => {
            let mut package = std::fs::read(input).map_err(|error| error.to_string())?;
            l6fw::update::recompute_checksum(&mut package).map_err(|error| error.to_string())?;
            l6fw::update::validate(&package).map_err(|error| error.to_string())?;
            std::fs::write(output, package).map_err(|error| error.to_string())
        }
        _ => Err(
            "usage: cargo run --bin l6fw -- <unpack PACKAGE DIR | repack DIR PACKAGE [--recompute-checksum] | update-checksum INPUT OUTPUT> (L6 and L6max are detected from the package header)"
                .into(),
        ),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
