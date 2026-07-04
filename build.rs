use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=RUFFLE_SELFHOSTED_DIST_DIR");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let dist_dir = env::var_os("RUFFLE_SELFHOSTED_DIST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../ruffle/web/packages/selfhosted/dist"));

    println!("cargo:rerun-if-changed={}", dist_dir.display());

    if !dist_dir.is_dir() {
        panic!(
            "Ruffle selfhosted dist was not found at {}. Build ../ruffle/web first with \
             `npm ci`, `npm run build --workspace=ruffle-core`, and \
             `npm run build --workspace=ruffle-selfhosted`.",
            dist_dir.display()
        );
    }

    require_file(&dist_dir, "ruffle.js");
    require_file(&dist_dir, "ruffle.js.map");

    let core_js = find_single_file(&dist_dir, "Ruffle core JS", |name| {
        name.starts_with("core.ruffle.") && name.ends_with(".js")
    });
    let core_js_map = find_single_file(&dist_dir, "Ruffle core JS source map", |name| {
        name.starts_with("core.ruffle.") && name.ends_with(".js.map")
    });
    let wasm = find_single_file(&dist_dir, "Ruffle wasm", |name| name.ends_with(".wasm"));

    println!(
        "cargo:rustc-env=RUFFLE_SELFHOSTED_DIST_DIR={}",
        normalize_path(&dist_dir)
    );
    println!("cargo:rustc-env=RUFFLE_CORE_JS_FILE={core_js}");
    println!("cargo:rustc-env=RUFFLE_CORE_JS_MAP_FILE={core_js_map}");
    println!("cargo:rustc-env=RUFFLE_WASM_FILE={wasm}");
}

fn require_file(dist_dir: &Path, file_name: &str) {
    let path = dist_dir.join(file_name);
    if !path.is_file() {
        panic!(
            "Required Ruffle selfhosted asset is missing: {}",
            path.display()
        );
    }
    println!("cargo:rerun-if-changed={}", path.display());
}

fn find_single_file(dist_dir: &Path, description: &str, matches: impl Fn(&str) -> bool) -> String {
    let mut files = fs::read_dir(dist_dir)
        .unwrap_or_else(|err| panic!("Failed to read {}: {err}", dist_dir.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            matches(&name).then_some(name)
        })
        .collect::<Vec<_>>();

    files.sort();

    match files.len() {
        1 => {
            let file = files.remove(0);
            println!("cargo:rerun-if-changed={}", dist_dir.join(&file).display());
            file
        }
        _ => panic!(
            "Expected exactly one {description} in {}, found {}: {}",
            dist_dir.display(),
            files.len(),
            files.join(", ")
        ),
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
