// Every binary that links rp-dos needs this flag; see src/window.rs.
fn main() {
    println!("cargo:rustc-link-arg=-Wl,--wrap=glfwDefaultWindowHints");
}
