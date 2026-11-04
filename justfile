qa:
    cargo run -p qe-agent serve --ipc-socket .

ac:
    pnpm tauri --cwd apps/agent-client
