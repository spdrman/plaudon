//! Desktop binary entry point.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    plaudon_desktop::run();
}
