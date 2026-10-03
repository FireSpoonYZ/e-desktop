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

X11/macOS 没有部分窗口裁剪能力，采用“完整可见或可恢复地最小化”的处理。Windows 裁剪也受应用窗口类型限制；分层/RTL 窗口的部分裁剪会明确报错。Chromium、Electron、Windows Terminal 等 DirectComposition 窗口的画面不受窗口区域裁剪，因此屏幕边缘露出一部分的窗口会放到 Z 序最底层，超出屏幕的部分由相邻显示器上的窗口盖住；相邻显示器的当前页面没有平铺窗口时，这个边缘窗口改为最小化，避免画面溢出到那块屏幕。Windows 概览使用 DWM 实时缩略图，源窗口或平台不支持时明确显示不可用。支持裁剪的后端（目前是 Windows）为滚动、聚焦、移动、调整宽高、打开或关闭窗口和切换工作区提供布局动画，概览打开和关闭时缩略图在桌面位置与概览位置之间缩放。Windows 上窗口关闭、被应用隐藏到托盘或新窗口出现时立即重排，相邻窗口直接滑过去补位，不等下一次轮询；关闭当前焦点窗口后，焦点交给同一页面里的相邻窗口（先同列上下，再右侧、左侧的列）。受管窗口的最小化/还原系统动画被关闭，还原时直接进入目标位置。Windows 上的布局动画参照 niri 按显示器合成：动画期间，参与动画的显示器盖上一层鼠标可穿透的遮罩，用 DWM 实时缩略图按帧画出各窗口，超出这块显示器的部分直接裁掉，不会画到相邻显示器上；真实窗口在遮罩下一次移到最终位置，缩小或移出屏幕的窗口等动画结束再移动，然后撤掉遮罩。例行刷新保留正在执行的动画；尚未完整滑入屏幕的窗口等到落位后才获得原生焦点。Windows 通过低级鼠标钩子提供焦点跟随鼠标、切换焦点时移动鼠标、修饰键拖动和屏幕热角（见 [配置文档](docs/configuration.md#鼠标)）；X11/macOS 暂无这些鼠标功能。布局在暂停和退出时保存，下次启用平铺时按窗口重新匹配并恢复（见[布局保存与恢复](docs/configuration.md#布局保存与恢复)）；外部程序可通过 IPC 和 e-desktop-msg 控制（见 [IPC 文档](docs/ipc.md)）。

支持创建可远程连接的托管终端：Windows 上的 PowerShell / Windows PowerShell、Git Bash，以及各平台可用的 Bash、Nushell。顶栏“终端”提供新建会话、重新打开窗口和手机配对；由 e-launcher 通过局域网或 Tailscale 连接同一个 PTY，会话保留真实 shell / agent TUI。默认远程端口为 7768。窗口分离和手机断线不结束 shell，退出 e-desktop 会结束托管会话。外部终端窗口仍只参与窗口管理，不能自动接管其现有 PTY。详见 [终端使用说明](docs/terminals.md) 和 [终端宿主协议](docs/terminal-host.md)。首版不含云中继、NAT 穿透服务或账户系统。

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

每个显示器顶部有 36 CSS 像素的控制栏，工作区编号直接放在其中。配置 `topBar: false` 可完全关闭控制栏。Windows 上控制栏默认收起，鼠标移到该显示器最顶端时只显示这一块屏幕的控制栏；点击“固定”可让它常驻并为它保留顶部空间。“概览”默认展示当前显示器的纵向工作区，以半尺寸显示横向列和堆叠窗口；查看更大显示器时会进一步缩小。顶部按钮可切换显示器，横向位置对应当前视口。点击窗口聚焦，拖动窗口跨工作区移动，拖动列边缘调宽；展开“调整布局”可输入列宽或选择跨屏移动目标。平铺时按住 `Alt` 左键拖动窗口，或直接拖动窗口标题栏，会在目标显示器上显示布局预览，松手后按预览移动：可以换到其他显示器、改变列顺序、加入或移出纵向堆叠、在堆叠内换位置，拖到屏幕左右边缘则排到屏幕外等待。拖动两列之间或堆叠窗口之间的分界线调整宽高，预览标出每个窗口占屏幕的百分比，分界线吸附屏幕中线和边缘；拖到屏幕边缘时，这一侧的窗口被挤出屏幕，另一侧窗口独占屏幕宽度或整列高度（详见[配置文档](docs/configuration.md#拖动分界线)）。列总是铺满屏幕；鼠标移到显示器左上角打开或关闭概览。“命令”可以搜索窗口、页面和布局动作。

视觉以 [niri 默认布局](https://niri-wm.github.io/niri/Configuration%3A-Layout.html) 和 [Overview](https://niri-wm.github.io/niri/Overview.html) 为参考：中性深灰背景、浅蓝焦点、纵向工作区和预览优先。默认保留 Windows 控制栏。窗口间距、焦点边框颜色和圆角可在配置中调整（边框颜色与圆角仅 Windows 11 生效）；壁纸未复刻，浮动窗口在概览中单独列出。

| 快捷键 | 操作 |
| --- | --- |
| `Ctrl+Alt+H/J/K/L` | 向左/下/上/右聚焦 |
| 上述组合增加 `Shift` | 移动当前窗口：左右合并相邻列或拆出边缘堆叠；上下在堆叠内排序 |
| `Ctrl+Alt+PageUp/PageDown` | 切换页面 |
| 上述组合增加 `Shift` | 把当前窗口移至相邻页面并跟随 |
| `Ctrl+Alt+1…9` | 进入已有的对应页面 |
| 上述组合增加 `Shift` | 把当前窗口移至对应页面 |
| `Ctrl+Alt+N` | 新增/进入空页面 |
| `Ctrl+Alt+R` | 循环列宽：1/3、1/2、2/3 |
| `Ctrl+Left/Right`、`Ctrl+Alt+Left/Right` | 向左 / 右滑动一列：下一个窗口完整进入屏幕并获得焦点 |
| `Ctrl+Alt+Shift+Left/Right` | 减少 / 增加当前列宽 50 物理像素 |
| `Ctrl+Alt+Shift+Up/Down` | 增加 / 减少聚焦窗口高度 50 物理像素 |
| `Ctrl+Alt+Shift+R` | 当前列恢复等高 |
| `Ctrl+Alt+C` | 居中当前窗口 |
| `Ctrl+Alt+V` | 切换浮动 |
| `Ctrl+Alt+F` | 切换布局全屏（填满预留后的视口） |
| `Ctrl+Alt+Space` / `Ctrl+Alt+O` | 命令面板 / 概览 |
| `Ctrl+Alt+Backspace` | 暂停并还原 |
| `Ctrl+Alt+Q` | 还原后退出 |
| `Ctrl+Alt+[ / ]` | 当前窗口在堆叠中时移出为左 / 右侧新列；单独一列时并入左 / 右侧相邻列 |
| `Ctrl+Alt+Shift+[ / ]` | 当前列整列左移 / 右移 |
| `Ctrl+Alt+Home/End` | 聚焦第一列 / 最后一列 |
| `Ctrl+Alt+Shift+Home/End` | 当前列移到最前 / 最后 |
| `Ctrl+Alt+,/.` | 聚焦左侧 / 右侧显示器 |
| `Ctrl+Alt+Shift+,/.` | 当前列移到左侧 / 右侧显示器的当前页面并跟随 |
| `` Ctrl+Alt+` `` | 回到上一个聚焦的窗口（可跨页面和显示器） |
| `Ctrl+Alt+P` | 当前显示器回到上一个页面 |
| `Ctrl+Alt+M` | 切换当前列最大化：视口全宽 / 恢复原宽度（不同于布局全屏） |
| `Ctrl+Alt+E` | 循环聚焦窗口在列内的预设高度（见[布局选项](docs/configuration.md#布局选项与命名页面)） |
| `Ctrl+Alt+W` | 切换当前列的标签显示：只显示一个窗口，上下聚焦切换标签（见[配置文档](docs/configuration.md#标签列)） |
| `Ctrl+Alt+/` | 显示 / 关闭快捷键提示（首次启用平铺时自动显示一次） |
| 概览中 `Ctrl+滚轮` / 触控板捏合 | 缩放概览 |
| `Win+滚轮` | 鼠标所在显示器切换上 / 下页面（仅 Windows，修饰键可配置，见[滚轮与触控板手势](docs/configuration.md#滚轮与触控板手势)） |
| `Win+Shift+滚轮`、`Win+横向滚轮` | 向左 / 右聚焦一列（仅 Windows） |
| `Ctrl+Alt+S` | 截图：冻结画面后框选区域，保存 PNG 并复制到剪贴板（仅 Windows，见[配置文档](docs/configuration.md#正则窗口规则启动命令和截图)） |
| `Ctrl+Alt+Shift+S` | 截取当前活动显示器 |
| `Ctrl+Alt+X` | 截取当前焦点窗口 |

全局快捷键可能与现有软件冲突，注册失败会显示具体组合；其余已注册组合仍可使用。此次测试机的 `Ctrl+Alt+L`、`Ctrl+Alt+R` 被占用，对应功能仍有命令面板入口，列宽操作已通过面板实测。支持通过 JSON 文件修改快捷键并自动热加载：只写需要覆盖、新增或删除（`unbind`）的组合，其余沿用默认值；尚无设置界面。在空页面或没有聚焦窗口时，先选择一个窗口再执行移动、列宽等动作。

顶栏的横向滚动区支持按钮、滚轮与触控板；命令面板提供列宽、高度调整及恢复等高。尺寸命令仅适用于平铺、非全屏窗口。配置还支持按应用名和标题设置新窗口的浮动、列宽、显示器及页面；已有窗口不因重载而重新排列。格式、默认路径和示例见 [配置文档](docs/configuration.md)。

## 可复现的 Windows 实窗检查

```powershell
powershell -NoProfile -File scripts/windows-smoke.ps1
```

脚本创建三个可丢弃的 WinForms 窗口，Rust example 将操作严格限定到该测试进程。检查任意列宽/增减列宽、堆叠高度调整/恢复等高、自由滚动、浮动切页恢复、布局全屏、可用显示器之间的移动、停用还原、正常关闭和失效 ID 拒绝，并输出 JSON 证据路径。不要并发运行该脚本。它不会启动完整 Tauri GUI；GUI 的验证记录见下文。

## 文档与边界

- [实现契约](docs/implementation-contract.md)
- [配置、快捷键和窗口规则](docs/configuration.md)
- [Windows 验证记录](docs/validation/windows-2026-09-26.md)
- [尺寸、滚动、配置和规则验收](docs/validation/windows-2026-09-26-niri-next.md)
- [niri UI 对齐与多屏实机检查](docs/validation/windows-2026-09-26-niri-ui.md)
- [Windows 后端边界与人工检查](src-tauri/src/platform/windows/SMOKE.md)
- [macOS 接入与验证步骤](src-tauri/src/platform/macos/README.md)
- [外部 IPC 与命令行客户端](docs/ipc.md)

当前不是系统合成器。独立弹窗、应用自定义窗口区域、提权窗口、挂起进程及异常终止都可能影响管理效果；强制结束进程或系统崩溃时不能保证恢复。尚未完成任意第三方应用、显示器热插拔、真实 IME、Linux/macOS 目标机和恢复失败交互的全面验证。
