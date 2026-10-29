// Aether DV desktop shell. Phase B spike: window only. The shell will connect
// to the separately running engine (aetherd) over the local API in Phase C;
// it never owns serial ports itself (ADR-0001).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running Aether DV");
}
