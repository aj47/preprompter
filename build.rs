fn main() {
    // Add rpath for Swift runtime libraries (needed for ScreenCaptureKit)
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    
    // Link against required macOS frameworks
    println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=IOKit");
}

