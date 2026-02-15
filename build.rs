fn main() {
    // macOS: Link against required frameworks
    #[cfg(target_os = "macos")]
    {
        // Add rpath for Swift runtime libraries (needed for ScreenCaptureKit)
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

        // Link against required macOS frameworks
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=IOKit");
    }

    // Linux: No special linking required (x11rb uses pure Rust connection)
    #[cfg(target_os = "linux")]
    {
        // x11rb RustConnection communicates via socket, no C library linking needed
    }

    // Windows: Link against GDI32 and User32 for screen capture
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rustc-link-lib=gdi32");
        println!("cargo:rustc-link-lib=user32");
        println!("cargo:rustc-link-lib=dxgi");
    }
}
