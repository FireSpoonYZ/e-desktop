# Windows 拖放无响应：诊断复测 — 2026-10-02

## 范围与结果

使用 interaction worker 的诊断提交 `840d8a3699deb97f0b85b9fd527545db0de482fa`，产品行为仍为原基线；没有混入新的 compositor/wallpaper 实现。通过 Tauri CLI 的 production-assets debug build 启动，设置 `E_DESKTOP_TRACE_IPC=1`，stderr 直接写本地文件。

在独立 Windows 虚拟桌面和 5 个 fixture 窗口上，**再次出现跨页拖放成功、随后同页拖动无响应**。主 assistant 操作真实窗口；worker 没有操作桌面。

本次缩小了故障范围，但没有确定根因，也没有宣称修复。

## 证据

目录：`D:/project/e-desktop/target/recordings/2026-10-02-diagnostic/`。

- `desktop-live.mp4`：本轮录像。
- `events.jsonl`：输入、前端捕获事件、CDP 超时。
- `app.err.log`：带请求编号的 Rust 阶段日志。
- `after-background-drop.json`：最后成功状态，后台 E 位于 page-9，当前 page-2。
- `process-tree.json`：被测应用及其 WebView2 后代进程。
- `e-desktop-hang.dmp`、`process-<PID>.dmp`：应用和 10 个 WebView2 后代进程的线程转储。
- `cleanup.json`：本轮退出与虚拟桌面清理结果。
- `record.mjs`、`cdp.mjs`、`browser-dumps.ps1`：实际运行的测试／取证脚本。
- `SHA256SUMS.txt`：录像与 stderr 哈希。

本轮 app PID 67708，fixture PID 12108。录制前断言仅有这 5 个 fixture 窗口，再启用管理。截图／视频未上传外部服务。

## 时序与观察

事件日志时间为录制脚本的单调墙钟，Rust 时间为首个 trace 的相对时间；二者起点不同，不直接按微秒相减。

1. 脚本约 11.44 秒发起 E → 后台页拖放。前端记录 pointerdown、gotpointercapture、pointerup、lostpointercapture。
2. Rust request 29 `execute.dropWindow` 经过 dispatch、compose、apply、snapshot publish、reply.sent、reply.wait.exit、ipc.exit。初次 compose 约 64 ms。
3. requests 30–32 `sync_previews` 均完成 native 调用并返回。
4. request 33 `get_snapshot` 在主线程进入并退出 snapshot.read，随后 ipc.exit；脚本约 13.51 秒确认跨页状态正确。
5. 第二次 D → C 原生拖动开始后，**没有收到第二次 pointerdown console 日志，也没有新的 execute.dropWindow Rust 入口日志**。
6. 输入结束后执行的 topbar `Runtime.evaluate('2+2')` 在 15 秒上限超时。该表达式不调用 Tauri IPC。
7. 随后关闭概览的 evaluate 也超时。stderr 最后记录仍为 request 33 的完整返回。

这不支持“第二次 DropWindow 已进入 Rust，然后 engine/controller 死锁”的说法。需要检查 WebView2、原生输入消息、命中／捕获与调试链路；console 消息未收到本身不能证明事件在浏览器内部从未分发。

实时缩略图持续更新也不能证明 controller 正在处理新请求。转储已保存，尚未符号化。

## 清理和前置问题

用户同意短暂停止输入后，正式复测于约 15:35 开始，15:37 已完成取证并返回原桌面。本轮只结束经过路径核验的测试应用进程树和 fixture，确认退出后关闭本轮测试虚拟桌面。正常还原路径仍未通过验收。

更早的准备阶段有两个测试设施问题，未计为产品故障：

- 单纯 `cargo build` 产物访问 dev URL `localhost:1420`；改用 `npm run tauri -- build --debug --no-bundle` 后才用于正式复测。
- 隔离检查曾发现非测试应用，脚本在启用管理、录制和发送交互前停止。该次测试进程已退出；当时不处于测试桌面，未自动关闭那个空测试桌面，避免关闭用户当前桌面。

## 下一步

原交互 worker 基于这些新证据继续离线分析和最小修复；不能仅将诊断提交算作原故障修复。动画 worker 同时处理独立 review 发现的壁纸启用状态遗漏。集成、再次审查与真实录像回归仍待完成。
