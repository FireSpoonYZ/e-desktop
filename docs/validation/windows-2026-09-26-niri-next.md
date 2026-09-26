# Windows 验收：尺寸、滚动、配置和窗口规则

日期：2026-09-26。基线 `7f3e221`，本轮功能和测试集成提交 `aa98309`，文档单独提交。

## 开发与合并

四个 worker 均使用 `openai/gpt-6-astra:high`，各自的 Orca worktree 位于 `C:/Users/46040/orca/workspaces/e-desktop/`。worker 只开发和编写测试；父会话完成代码检查、合并、接口接线和验收。

| 分支 | Worker 提交 | 合入 master |
| --- | --- | --- |
| `niri-sizing` | `0441533` | `0241311` |
| `niri-controls` | `877faaf` | `b5d6f28` |
| `niri-config` | `66bb9c8` | `cbe0f52` |
| `niri-rules` | `1633be3`、`bc9bd7e` | `c4d5940`、`c11758f` |

父会话另提交实窗尺寸检查 `c697b57`、尺寸快捷键接线 `6e42916`、配置规则集成及实窗规则检查 `aa98309`。按交付顺序滚动验收，没有等待全部 worker 完成才开始合并。

窗口规则 review 发现：暂停时首次跨屏浮动规则生成的位置，会在下一轮枚举时被原位置覆盖。worker 先增加失败回归，再用待首次可见放置的窗口标记修复。仅最小化的 Placement 不清理标记，因为 Win32 此时不应用目标矩形。覆盖了暂停两次刷新、后台页、外部恢复、规则重载、还原和断屏。

## 自动化结果

| 命令 | 结果 |
| --- | --- |
| `npm test` | 19 passed，0 failed |
| `npm run typecheck` | 通过 |
| `npm run build` | 通过，44 modules |
| `cargo test --workspace --no-default-features --locked` | 39 passed，0 failed |
| `cargo test --workspace --lib --locked` | 40 passed，0 failed，含实际快捷键解析器验证 |
| `npm run tauri -- build --debug --no-bundle` | 通过，`target/debug/e-desktop.exe` |
| `git diff --check` | 通过 |

前端检查在合入 controls 后执行，其后没有前端代码变更；最终 Tauri 构建也重新执行了 typecheck 和 Vite 构建。未为不相关代码重复运行检查。

## 隔离实窗 smoke

执行 `powershell -NoProfile -File scripts/windows-smoke.ps1`，以三个测试进程内 WinForms 窗口为操作范围。脚本不启动完整桌面管理 GUI。

最终证据：`%TEMP%\e-desktop-smoke-20260926-174642\evidence.json`。末条结果为 `passed: true`、`failure: null`、`finalRestore: null`。临时证据文件可能随系统清理消失，复现应重新运行脚本。

| 显示器 | 物理尺寸与原点 | 缩放 |
| --- | --- | --- |
| DISPLAY1 | 1920 × 1080，(-1920, 0) | 100% |
| DISPLAY2 | 3840 × 2160，(0, 0) | 150% |
| DISPLAY3 | 1080 × 1920，(3840, 0) | 100% |

验证了：

- 任意列宽 500，再增加 50，原生观察结果为 550 像素。
- 合列后聚焦窗口高度增加 50，随后恢复等高，原生高度与初始值一致。
- 自由滚动命令改变视口，重新聚焦恢复可见性。
- 既有浮动切页恢复、布局全屏、逐屏移动、停用还原、优雅关闭、失效窗口 ID 拒绝。
- 从 JSON 加载浮动跨屏规则，连续两次暂停枚举不产生动作，启用后真实窗口位于目标显示器。
- 规则操作后再次停用，所有测试窗口恢复原矩形和最小化状态。

测试进程在验收后均已结束。

## 完整 Tauri 应用检查

使用 `E_DESKTOP_CONFIG=%TEMP%\e-desktop-acceptance-config.json` 启动本次 debug 程序，保持管理暂停。通过 Orca Windows accessibility 和合成快捷键逐项检查，未对用户其他应用启用平铺。

1. 从初始映射热更新为 `Ctrl+Alt+Shift+O → overview`；操作后实际活动窗口为 `e-desktop overview`，界面显示管理暂停。
2. 将配置改为无效 JSON `{`；界面显示对应路径及 EOF 解析错误，同一快捷键仍能打开概览，证明旧配置保留。
3. 将同一组合改为 `commands`；无需重启即可打开 `e-desktop commands`，此前错误消失，树中包含新增尺寸动作。
4. 设置 `shortcuts: []`；旧快捷键不再打开概览或命令面板，活动窗口保持顶栏。
5. 再热加载 `Ctrl+Alt+Shift+Q → quit`；操作后 PID 消失，应用已退出。工具的动作后观察返回 app_not_found，与独立进程检查一致。

顶栏 accessibility 树包含当前显示器、启动平铺、左右滚动按钮、横向位置、刷新和退出控件。该机器的 Orca 截图对次屏返回黑色图片，且窗口坐标与原生显示器坐标不一致，因此**未据此确认像素呈现和窄屏布局**。功能判断依据实际窗口标题、accessibility 树和进程状态。

Orca Windows 输入接口不支持 F12，最初测试组合已改为字母组合；这不代表应用不支持 F12。重复修饰键、未知组合、重复映射及注册失败回滚由 Rust 测试验证；没有在 GUI 中人为制造系统注册冲突。

## 剩余验证范围

- 没有真实触控板手势手感测试；滚轮单位、DPI 换算、节流、Ctrl-wheel 排除由前端行为测试覆盖。
- GUI 全程暂停；布局操作由隔离实窗脚本验证。完整 GUI 内的鼠标滚动到原生窗口链路尚未人工端到端操作。
- 未验证任意第三方应用的最小尺寸约束、显示器热插拔、强制退出恢复以及 Linux/macOS 目标机。
- 本轮不包含标签页列、动画、实时缩略图、命名/持久工作区、niri 外部 IPC 协议或合成器能力。
