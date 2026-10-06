//! The Windows resource section, which is where the application icon lives.
//!
//! GPUI asks the running executable for icon resource number 1 and puts it on
//! the window, so the icon has to be compiled into the binary rather than
//! shipped beside it — that is also what makes Explorer, the taskbar button and
//! Alt-Tab show it. `winresource` writes the icon out under exactly that id.
//!
//! Everywhere that is not Windows this does nothing.

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/tinnitus.ico");

        let mut resource = winresource::WindowsResource::new();
        resource.set_icon_with_id("assets/tinnitus.ico", "1");
        resource.set("ProductName", "Tinnitus");
        resource.set("FileDescription", "Tinnitus");

        // A machine with no resource compiler costs the icon, not the build:
        // the app runs perfectly well with the default window icon.
        if let Err(error) = resource.compile() {
            println!("cargo:warning=tinnitus: cannot embed the icon: {error}");
        }
    }
}
