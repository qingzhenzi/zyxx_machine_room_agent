qa:
    cargo run -p qe-agent serve --ipc-socket /run/qe-agent.socket

ac:
    pnpm tauri --cwd apps/agent-client

bd:
    pnpm --dir apps/boot-display tauri dev

bd-check:
    pnpm --dir apps/boot-display check && pnpm --dir apps/boot-display check:rust
