#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    marklock_lib::run()
}
