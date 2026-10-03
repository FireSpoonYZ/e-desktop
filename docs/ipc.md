# 外部 IPC 与命令行客户端

e-desktop 运行时提供一个本机 IPC 端点，用法参照 `niri msg`：外部脚本可以执行布局命令、触发快捷键动作、查询当前状态，或持续接收状态变化。IPC 连接在控制器线程之外处理，动作通过现有的命令队列交给控制器，与界面按钮和全局快捷键走同一条路径。

## 端点

| 平台 | 端点 | 访问控制 |
| --- | --- | --- |
| Windows | 命名管道 `\\.\pipe\e-desktop-<用户名>`（用户名取自 `USERNAME`） | 管道 DACL 只授予当前用户；拒绝远程客户端 |
| Linux / macOS | `$XDG_RUNTIME_DIR/e-desktop.sock`；未设置 `XDG_RUNTIME_DIR` 时为系统临时目录下的 `e-desktop-<用户名>.sock` | 套接字文件权限 `0600` |

同一用户只能有一个 e-desktop 拥有端点。端点已被占用或无法创建时，应用照常运行，错误显示在错误区；IPC 不可用。Unix 上上次异常退出遗留的套接字文件会在启动时被替换。Linux/macOS 的 IPC 只通过了编译检查，尚未在目标系统上运行。

## 协议

每个连接可发送多行请求：每行一个 JSON 请求，e-desktop 对每行回一行 JSON 应答。空行被忽略。

应答：

```json
{"ok": <结果>}
{"error": {"code": "invalidCommand", "message": "...", "windowId": null}}
```

`code` 与界面错误相同：`notImplemented`、`unsupportedSession`、`backendUnavailable`、`permissionRequired`、`windowGone`、`operationDenied`、`invalidCommand`。

请求（每个请求对象只能有一个键）：

| 请求 | 结果 |
| --- | --- |
| `{"action": <命令>}` | 执行 [实现契约](implementation-contract.md) 中的命令对象，例如 `{"type":"focusDirection","direction":"left"}`。等待控制器执行完毕后回 `{"ok":null}`；命令被拒绝时回 `error` |
| `{"shortcutAction": <快捷键动作>}` | 按下绑定了该动作的快捷键时的效果，动作格式见 [配置文档](configuration.md#自定义快捷键)，例如 `{"type":"overview"}`、`{"type":"page","number":2}`、`{"type":"quit"}`。放入控制器队列后立即回 `{"ok":null}`，不等待执行结果；`unbind` 无效 |
| `{"query": "snapshot"}` | 完整状态快照，与界面收到的 `snapshot` 事件相同 |
| `{"query": "windows"}` | `snapshot.windows`：每个受管窗口的原生信息（`native`）和 `floating`、`fullscreen` |
| `{"query": "pages"}` | 每个显示器一项：`monitorId`、`monitorName`、`activePage` 和完整的 `pages`（列、列宽、列内顺序、浮动窗口、滚动位置） |
| `{"query": "focusedWindow"}` | 聚焦窗口的状态；没有聚焦窗口时为 `null` |
| `{"query": "config"}` | 当前生效的配置，快捷键为合并默认组合后的完整映射 |
| `{"eventStream": true}` | 先回 `{"ok":null}`，随后立即推送一行 `{"snapshot": <快照>}`，之后每当快照变化再推送一行。此后这个连接不再接受请求 |

`action` 与 `shortcutAction` 中的 ID 按提供的值使用；`addPage` 的空 `monitorId` 表示活动显示器。外部全屏窗口导致所在显示器暂停时，`shortcutAction` 与快捷键一样受限。事件流每 100 ms 检查一次快照，连续的多次变化可能合并成一行；客户端断开后服务端在下一次推送时结束这个连接。

查询读取控制器发布的最近一份快照，不会阻塞控制器或界面线程。

## 命令行客户端

`e-desktop-msg` 是独立的控制台程序（e-desktop 本体在 Windows 上属于 GUI 子系统，在终端里没有输出）。它是工作区成员 `e-desktop-msg/`，不依赖 Tauri，不参与 Tauri 打包：

```sh
cargo build -p e-desktop-msg --release
# Windows：target/release/e-desktop-msg.exe
```

用法：

```sh
e-desktop-msg action '{"type":"focusDirection","direction":"left"}'
e-desktop-msg action '{"type":"setColumnWidth","width":900}'
e-desktop-msg shortcut-action '{"type":"overview"}'
e-desktop-msg windows
e-desktop-msg pages
e-desktop-msg focused-window
e-desktop-msg snapshot
e-desktop-msg config
e-desktop-msg event-stream
e-desktop-msg --json windows
```

PowerShell 7.3 及以上用单引号包住 JSON 即可；Windows PowerShell 5.1 传给外部程序时会去掉内部双引号，需要写成 `'{\"type\":\"overview\"}'`；`cmd.exe` 中写成 `"{\"type\":\"overview\"}"`。

默认输出便于阅读：`windows` 每个窗口一行（ID、应用、标题，浮动或全屏时附标记），`pages` 按显示器列出页面并用 `*` 标出活动页面，`snapshot` 和 `config` 输出格式化的 JSON，`event-stream` 每次变化输出一行摘要，`action` 和 `shortcut-action` 成功时不输出。加 `--json` 时原样输出 e-desktop 返回的每一行 JSON。

e-desktop 返回错误或无法连接时，客户端把错误信息写到标准错误并以退出码 1 结束。
