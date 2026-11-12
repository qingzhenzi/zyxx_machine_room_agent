# 依赖许可证

本 crate 自身为 MIT。直接依赖及其传递依赖：

| crate | 版本 | 许可证 | feature |
|---|---|---|---|
| sysinfo | 0.39 | MIT | 默认 |
| serde | 1 | MIT OR Apache-2.0 | 默认 |
| serde_derive | 1 | MIT OR Apache-2.0 | 默认 |
| serde_json | 1 | MIT OR Apache-2.0 | 默认 |
| libc（unix 平台） | 0.2 | MIT OR Apache-2.0 | 默认 |
| windows / objc2 系列（win/mac 平台） | - | MIT OR Apache-2.0 | 默认 |
| nvml-wrapper | 0.13 | MIT OR Apache-2.0 | `gpu` |

**没有 GPL、AGPL、LGPL 代码**，可以闭源分发、静态链接、嵌入桌面安装包。

可选 feature 只增 `nvml-wrapper`（MIT/Apache-2.0）。`smart`/`security`/`containers`/`packages` 等 shell-out 域调用外部工具（smartctl、docker、ss、dpkg…），其中部分工具本身是 GPL，但**独立进程调用、只解析其文本输出不构成衍生作品**（同调用 gcc），不传染本 crate。

升级依赖后请重新核对：

```console
cargo install cargo-deny --locked
cargo deny check licenses
```

建议在 CI 里加上 `cargo deny`，防止将来引入 copyleft 依赖。
