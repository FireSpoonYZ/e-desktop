# e-desktop

使用 Rust、Tauri 2 和 React 管理真实应用窗口。窗口按横向列排列，列内可以纵向堆叠；各显示器拥有独立页面，聚焦窗口时横向视口随之移动。

应用默认暂停。点击“启动平铺”开始管理；“暂停并还原”恢复原窗口状态；退出前也会尝试还原。还原失败时保留暂停的应用并显示错误，便于重试。

## 当前支持

| 平台 | 实现与验证范围 |
| --- | --- |
| Windows | Win32 后端、真实窗口布局、裁剪、聚焦、最小化、正常关闭与还原。已构建，并用独立测试窗口验证三屏混合缩放、Tauri 界面命令、部分全局快捷键和退出还原。 |
| Linux X11 | 已实现 x11rb/EWMH 后端、窗口实例标记与还原。通过 Windows 上的独立编译/纯逻辑检查；尚未在 Linux/X11 会话运行。 |
| Linux Wayland | 明确报告不支持窗口管理；可以正常退出。 |
| macOS | 已实现 Accessibility/CoreGraphics 后端及权限状态；仅通过 host 类型检查和几何测试，尚未在 Apple 目标构建或运行。混合 backing scale 暂不支持。 |

X11/macOS 没有部分窗口裁剪能力，采用“完整可见或可恢复地最小化”的处理。Windows 裁剪也受应用窗口类型限制；分层/RTL 窗口的部分裁剪会明确报错。当前没有实时缩略图、鼠标跟随聚焦或平滑滚动动画。布局保存在当前会话内。

这轮实现不含手机端、远程连接、NAT 穿透、账户或 LLM。命令与状态接口保持独立，便于后续扩展。

## 启动与构建

需要 stable Rust、Node.js 22.12+ 和 npm。Windows 还需要 MSVC Build Tools、Windows SDK、WebView2；其他平台参见 [Tauri 环境要求](https://v2.tauri.app/start/prerequisites/)。

```sh
npm ci
npm run tauri -- dev
```

开发时 `npm run dev` 只启动前端服务；真实窗口管理需要 Tauri。

```sh
npm test
cargo test --workspace --no-default-features --locked
npm run tauri -- build --debug --no-bundle
```

Windows 调试可执行文件：`target/debug/e-desktop.exe`。去掉 `--debug` 可构建 release；本次实际验证的是 debug 构建，未生成安装包。

## 操作

顶部为控制栏，左侧为页面栏。“概览”显示实际窗口元数据及列/堆叠关系；“命令”可以搜索窗口、页面和布局动作。概览中的窗口没有实时预览，会明确显示“预览不可用”。

| 快捷键 | 操作 |
| --- | --- |
| `Ctrl+Alt+H/J/K/L` | 向左/下/上/右聚焦 |
| 上述组合增加 `Shift` | 移动当前窗口：左右合并相邻列或拆出边缘堆叠；上下在堆叠内排序 |
| `Ctrl+Alt+PageUp/PageDown` | 切换页面 |
| 上述组合增加 `Shift` | 把当前窗口移至相邻页面并跟随 |
| `Ctrl+Alt+1…9` | 进入已有的对应页面 |
| 上述组合增加 `Shift` | 把当前窗口移至对应页面 |
| `Ctrl+Alt+N` | 新增/进入空页面 |
| `Ctrl+Alt+R` | 循环列宽：1/3、1/2、2/3、完整视口 |
| `Ctrl+Alt+C` | 居中当前窗口 |
| `Ctrl+Alt+V` | 切换浮动 |
| `Ctrl+Alt+F` | 切换布局全屏（填满预留后的视口） |
| `Ctrl+Alt+Space` / `Ctrl+Alt+O` | 命令面板 / 概览 |
| `Ctrl+Alt+Backspace` | 暂停并还原 |
| `Ctrl+Alt+Q` | 还原后退出 |

全局快捷键可能与现有软件冲突，注册失败会显示具体组合；其余已注册组合仍可使用。此次测试机的 `Ctrl+Alt+L`、`Ctrl+Alt+R` 被占用，对应功能仍有命令面板入口，列宽操作已通过面板实测。当前尚无快捷键配置界面。在空页面或没有聚焦窗口时，先选择一个窗口再执行移动、列宽等动作。

## 可复现的 Windows 实窗检查

```powershell
powershell -NoProfile -File scripts/windows-smoke.ps1
```

脚本创建三个可丢弃的 WinForms 窗口，Rust example 将操作严格限定到该测试进程。检查列宽/堆叠、浮动切页恢复、布局全屏、可用显示器之间的移动、停用还原、正常关闭和失效 ID 拒绝，并输出 JSON 证据路径。不要并发运行该脚本。它不会启动完整 Tauri GUI；GUI 的验证记录见下文。

## 文档与边界

- [实现契约](docs/implementation-contract.md)
- [Windows 验证记录](docs/validation/windows-2026-09-26.md)
- [Windows 后端边界与人工检查](src-tauri/src/platform/windows/SMOKE.md)
- [macOS 接入与验证步骤](src-tauri/src/platform/macos/README.md)

当前不是系统合成器。独立弹窗、应用自定义窗口区域、提权窗口、挂起进程及异常终止都可能影响管理效果；强制结束进程或系统崩溃时不能保证恢复。尚未完成任意第三方应用、显示器热插拔、真实 IME、Linux/macOS 目标机和恢复失败交互的全面验证。
