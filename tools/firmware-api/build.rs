//! Lists the names each model's API declares, so tests can name an address
//! by its symbol. Only names are read here: the generated table refers to the
//! constants the declaration macros produce, so the compiler checks each name
//! and every address comes from the declaration itself.
use std::{env, fs, path::Path};

const MACROS: [&str; 3] = [
    "firmware_functions!",
    "firmware_variables!",
    "firmware_symbols!",
];

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../patches/api");
    let out = Path::new(&env::var("OUT_DIR").unwrap()).to_path_buf();
    for model in ["l6", "l6max"] {
        let dir = root.join(model);
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut names = Vec::new();
        let mut files: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .collect();
        files.sort();
        for file in files {
            println!("cargo:rerun-if-changed={}", file.display());
            names.extend(declared_names(&fs::read_to_string(&file).unwrap()));
        }
        names.sort();
        let entries: String = names
            .iter()
            .map(|name| format!("    ({name:?}, {name}),\n"))
            .collect();
        fs::write(
            out.join(format!("{model}_symbols.rs")),
            format!(
                "/// Every declared name and its address, sorted by name.\n\
                 pub const SYMBOLS: &[(&str, u32)] = &[\n{entries}];\n"
            ),
        )
        .unwrap();
    }
}

/// The names declared in a file's `firmware_*!` blocks.
fn declared_names(source: &str) -> Vec<String> {
    let code: String = source
        .lines()
        .map(|line| line.split("//").next().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let mut names = Vec::new();
    let mut rest = code.as_str();
    while let Some(start) = MACROS.iter().filter_map(|name| rest.find(name)).min() {
        let open = start + rest[start..].find('{').unwrap();
        let close = open + rest[open..].find("\n}").unwrap();
        for declaration in split_declarations(&rest[open + 1..close]) {
            let declaration = strip_attributes(&declaration);
            if declaration.trim().is_empty() {
                continue;
            }
            let head = declaration.split(" at ").next().unwrap();
            let head = match head.find("fn ") {
                Some(at) => &head[at + 3..],
                None => head,
            };
            let name = head
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .find(|word| !word.is_empty() && !["pub", "unsafe", "mut"].contains(word))
                .unwrap();
            names.push(name.to_owned());
        }
        rest = &rest[close..];
    }
    names
}

fn strip_attributes(declaration: &str) -> String {
    let mut text = declaration.trim().to_owned();
    while text.starts_with("#[") {
        let end = text.find(']').unwrap();
        text = text[end + 1..].trim_start().to_owned();
    }
    text
}

/// Split a block at the semicolons that end declarations, not those inside
/// array types such as `[u8; N]`.
fn split_declarations(block: &str) -> Vec<String> {
    let mut declarations = vec![String::new()];
    let mut depth = 0i32;
    for c in block.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            ';' if depth == 0 => {
                declarations.push(String::new());
                continue;
            }
            _ => {}
        }
        declarations.last_mut().unwrap().push(c);
    }
    declarations
}
