// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{}", qlight_wallet_lib::node::USAGE);
        return;
    }
    // 无头中继：同一套账本与验签代码，没有界面、不出块、不持币（DESIGN.md §9.32）
    if args.iter().any(|a| a == "--node") {
        qlight_wallet_lib::node::run();
        return;
    }
    qlight_wallet_lib::run()
}
