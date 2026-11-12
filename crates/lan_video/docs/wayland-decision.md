# 采集后端决策：X11 标准化（Wayland 单独立项）

> 决策时间：2026-10-31。背景：Debian KWin 6.3.6 + PipeWire 1.4.2/1.6.9 下
> `kwin` screencast 实测 **0 帧**；trixie 无更新 KWin；跨 sid/experimental 风险大。

## 结论

**产品主线标准化 X11（方案 A）**。Wayland/KWin 采集降级为独立实验性轨道，单独立项跟进，
不阻塞产品化。

## 决策依据

| 维度 | A. 标准化 X11 | B. 攻坚 Wayland |
|---|---|---|
| 现在能否用 | ✅ 稳定出帧（Plasma X11） | ❌ Debian KWin 0 帧 |
| 工作量 | 小（固化默认 + 文档） | 大（换采集后端：升级 KWin / portal / DBus 截图 / 自研） |
| 确定性 | 高 | 低（依赖 KWin 版本 + 会话权限） |
| 前提 | Debian 切 Plasma X11 会话 | 无版本上限保证 |
| 适用 | 同 LAN 产品化、可控部署 | 必须 Wayland 的场景（高安全、无 XWayland） |

## 落地动作（已做）

1. **默认采集 `x11`**：`connect-debian.sh` 默认 `capture=x11`；文档示例统一改用 x11。
2. **文档标注**：README 功能表把 KDE Wayland 采集标为"实验/需 KWin 出帧"，X11 标为推荐。
3. **`kwin` 保留可用**：代码路径不删，供 Wayland 轨道继续实验；但不承诺 Debian trixie 可用。

## 适用约束（产品前提）

- 被控 Debian **必须运行 Plasma X11 会话**（不是 Wayland）。
- 部署时确认 `echo $XDG_SESSION_TYPE` = `x11`；Wayland 会话下 X11 采集只能抓 XWayland 窗口（不推荐）。
- 未来若 Wayland 轨道攻克（KWin 升级 / portal 稳定 / DBus 截图 / 自研），可切回双后端并存。

## Wayland 轨道（独立立项，不在主线）

待做方向（按可行性）：
1. **升级 KWin**：等 Debian 提供能出帧的 KWin 版本，验证 `zkde_screencast_unstable_v1`。
2. **XDG Desktop Portal**：`ScreenCast` + `restore_token` 持久授权（免每次弹窗）。
3. **DBus 截图**：`org.freedesktop.portal.Screenshot` / `org.kde.KWin` 截图接口（适合低频，不适合视频流）。
4. **自研采集**：KMS/DRM 直读 + libei 输入注入（参考 Sunshine `capture=kms`）。

> 该轨道需单独评估，不并入主线发布节奏。验收标准与主线一致（跟手、无黑框、音画同步），
> 但允许更长周期。

## 验收（X11 主线）

- `./scripts/connect-debian.sh 192.168.8.115 6000 x11` 稳定出帧。
- 鼠标跟手、无黑框（P0-1 已修）。
- 撕裂 / 音画同步：见 P0-2 / P0-3 实测判定。
