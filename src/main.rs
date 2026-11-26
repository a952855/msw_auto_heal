#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod monitor;
mod native;
mod ocr;

fn main() {
    if let Err(error) = native::run() {
        native::report_startup_error(&format!("啟動失敗：{error}"));
    }
}
