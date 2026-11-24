const COMMANDS: &[&str] = &["check", "download", "install", "download_and_install"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
