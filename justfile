qa:
    cargo run -p qe-agent serve --ipc-socket /run/qe-agent.socket

ac:
    pnpm tauri --cwd apps/agent-client
