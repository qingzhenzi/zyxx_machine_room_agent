qa:
    cargo run -p qe-agent serve --ipc-socket /run/qe-agent.socket

ac:
    pnpm tauri --cwd apps/agent-client

# 开发模式(热重载)
bd:
    pnpm --dir apps/boot-display tauri dev

# 手动演示:mock 数据源 + 已构建的 boot-display(读已嵌入的 dist)
bd-demo:
    python3 scripts/mock-display-agent.py & \
    sleep 1; \
    QE_AGENT_IPC_SOCKET=/tmp/qe-mock-display.sock apps/boot-display/src-tauri/target/debug/boot-display

# 单独起 mock 数据源(5s 轮换 boot/progress/enroll/idle)
bd-mock:
    python3 scripts/mock-display-agent.py

bd-check:
    pnpm --dir apps/boot-display check && pnpm --dir apps/boot-display check:rust
