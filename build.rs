// 链接指向 rpi-rgb-led-matrix 编译产物。
// 通过环境变量指定其安装前缀（默认 /usr/local）：
//     RGB_MATRIX_DIR=/path/to/rpi-rgb-led-matrix cargo build --release
use std::env;

fn main() {
    let dir = env::var("RGB_MATRIX_DIR").unwrap_or_else(|_| "/usr/local".to_string());
    println!("cargo:rustc-link-search=native={dir}/lib");
    println!("cargo:rustc-link-lib=static=rgbmatrix");
    // 该库是 C++ 实现，需要 C++ 运行时
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=m");
    println!("cargo:rerun-if-env-changed=RGB_MATRIX_DIR");
}
