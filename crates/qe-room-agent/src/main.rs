//! 房间机器 agent 入口（独立二进制 `qe-room-agent`）。
//!
//! 运行逻辑见 [`qe_room_agent::run_from_args`] / [`qe_room_agent::run_with_cli`]；
//! 统一入口 `qe-agent run --role machine` 复用同一实现。

#![deny(unsafe_code)]

fn main() {
    eprintln!(
        "提示：qe-room-agent 已并入统一入口，请改用 `qe-agent run --role machine`（旧二进制将在 P4 退场）"
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(qe_room_agent::run_from_args(args));
}
