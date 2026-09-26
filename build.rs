// 编译内置的 rpi-rgb-led-matrix（third_party/），不再依赖外部安装的 librgbmatrix。
// 只调用系统的 g++/gcc 和 ar，不引入 crates.io 依赖。
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("cc") | Some("c") | Some("cpp")
        ) {
            out.push(p);
        }
    }
}

fn compile(src: &Path, obj: &Path, includes: &[PathBuf], is_cxx: bool) {
    let mut cmd = Command::new(if is_cxx { "g++" } else { "gcc" });
    cmd.arg("-c")
        .arg(src)
        .arg("-o")
        .arg(obj)
        .arg("-O2")
        .arg("-fPIC")
        .arg("-fno-exceptions")
        .arg("-Wno-unused-parameter")
        .arg("-DDEFAULT_HARDWARE=\"regular\"");
    if is_cxx {
        cmd.arg("-std=c++11");
    } else {
        // strdup / O_CLOEXEC / nanosleep 等需要 _GNU_SOURCE
        cmd.arg("-std=gnu11").arg("-D_GNU_SOURCE");
    }
    for inc in includes {
        cmd.arg(format!("-I{}", inc.display()));
    }
    let st = cmd.status().unwrap_or_else(|e| panic!("failed to spawn compiler for {src:?}: {e}"));
    if !st.success() {
        panic!("compile failed: {src:?}");
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("third_party/rpi-rgb-led-matrix");
    if !root.is_dir() {
        panic!("third_party/rpi-rgb-led-matrix is missing");
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let includes = vec![
        root.join("include"),
        root.join("lib"),
        root.join("lib/rp1"),
        root.join("lib/rp1/rp1_pio_vendor/piolib/include"),
        root.join("lib/rp1/rp1_pio_vendor/include"),
    ];

    let mut sources = Vec::new();
    walk(&root, &mut sources);
    sources.sort();
    assert!(!sources.is_empty(), "no C/C++ sources under third_party/rpi-rgb-led-matrix");

    let mut objs = Vec::new();
    for src in &sources {
        let stem = src
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace('.', "_");
        let rel = src.strip_prefix(&root).unwrap_or(src);
        let tag: String = rel.to_string_lossy().replace('/', "_").replace('.', "_");
        let obj = out_dir.join(format!("{tag}.o"));
        // 源文件变了才重编
        println!("cargo:rerun-if-changed={}", src.display());
        if !obj.exists()
            || fs::metadata(&obj).and_then(|m| m.modified()).ok()
                < fs::metadata(src).and_then(|m| m.modified()).ok()
        {
            let is_cxx = matches!(
                src.extension().and_then(|s| s.to_str()),
                Some("cc") | Some("cpp")
            );
            compile(src, &obj, &includes, is_cxx);
        }
        let _ = stem;
        objs.push(obj);
    }

    let lib = out_dir.join("librgbmatrix.a");
    let _ = fs::remove_file(&lib);
    let mut ar = Command::new("ar");
    ar.arg("crs").arg(&lib);
    for o in &objs {
        ar.arg(o);
    }
    let st = ar.status().expect("failed to run ar");
    if !st.success() {
        panic!("ar failed");
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=rgbmatrix");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=m");
    println!("cargo:rustc-link-lib=dylib=rt");
}
