fn main() {
    #[cfg(windows)]
    println!("cargo:rustc-link-lib=advapi32");
}
