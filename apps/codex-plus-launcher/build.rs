fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/chatgpt.ico");
        let mut resource = winresource::WindowsResource::new();
        resource.set("ProductName", "Z8 Codex");
        resource.set("FileDescription", "Z8 Codex");
        resource.set("CompanyName", "Z8");
        resource.set_icon("assets/chatgpt.ico");
        resource.set_manifest(include_str!(
            "../codex-plus-manager/src-tauri/windows-app-manifest.xml"
        ));
        resource.compile().expect("compile launcher icon resource");
    }
}
