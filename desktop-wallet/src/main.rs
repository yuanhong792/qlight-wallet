// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{}", qlight_wallet_lib::node::USAGE);
        return;
    }
    // 无头节点：同一套账本与验签代码，没有界面（DESIGN.md §9.32）
    // 加 --mine 才出块：默认这台机器只验账与转发，不持币、不发行（§9.34）
    if args.iter().any(|a| a == "--node") {
        if let Some(i) = args.iter().position(|a| a == "--send") {
            let to = args.get(i + 1).cloned().unwrap_or_default();
            let amount = args.get(i + 2).cloned().unwrap_or_default();
            if to.is_empty() || amount.is_empty() {
                eprintln!("用法：qlight-wallet --node --send <ql1…收款地址> <金额QLX>");
                return;
            }
            qlight_wallet_lib::node::send_once(&to, &amount);
            return;
        }
        qlight_wallet_lib::node::run(args.iter().any(|a| a == "--mine"));
        return;
    }
    qlight_wallet_lib::run()
}
