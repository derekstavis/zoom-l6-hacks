fn main() {
    if let Err(error) = l6patch::run_cli() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}
