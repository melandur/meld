//! Compiles the translations in `po/` (languages listed in `po/LINGUAS`)
//! into catalogs embedded by `src/i18n`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "src/i18n/plural.rs"]
mod plural;
#[allow(dead_code)]
#[path = "src/i18n/po.rs"]
mod po;

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let po_dir = manifest.join("../../po");
    println!("cargo:rerun-if-changed=src/i18n/po.rs");
    println!("cargo:rerun-if-changed=src/i18n/plural.rs");
    println!(
        "cargo:rerun-if-changed={}",
        po_dir.join("LINGUAS").display()
    );

    let mut table = String::from("/// Embedded catalogs: (language, encoded catalog).\n");
    table.push_str("pub static CATALOGS: &[(&str, &[u8])] = &[\n");
    for lang in linguas(&po_dir) {
        let path = po_dir.join(format!("{lang}.po"));
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(source) = std::fs::read_to_string(&path) else {
            println!("cargo:warning=po/{lang}.po listed in LINGUAS but missing");
            continue;
        };
        let file = match po::parse(&source) {
            Ok(file) => file,
            Err(err) => panic!("po/{lang}.po: {err}"),
        };
        let (catalog, _skipped) = match po::catalog(&file) {
            Ok(c) => c,
            Err(err) => panic!("po/{lang}.po: bad Plural-Forms: {err}"),
        };
        let name = format!("{}.cat", lang.replace('@', "-"));
        std::fs::write(out_dir.join(&name), po::encode(&catalog)).expect("write catalog");
        let _ = writeln!(
            table,
            "    ({lang:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}\"))),"
        );
    }
    table.push_str("];\n");
    std::fs::write(out_dir.join("catalogs.rs"), table).expect("write catalogs.rs");
}

fn linguas(po_dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(po_dir.join("LINGUAS")).unwrap_or_default();
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .flat_map(|l| l.split_whitespace())
        .map(str::to_string)
        .collect()
}
