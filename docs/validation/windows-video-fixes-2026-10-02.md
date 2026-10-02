# Windows 录像问题修复与回归 — 2026-10-02

## 当前结果

被测产品版本：`474a30f`，程序为 production-assets debug build，`target/debug/e-desktop.exe`。

已修复并取得真实回归证据：

- 跨页出现静态预览后，再次同页拖动导致 WebView 无响应。
- 动画未覆盖区域的大块深色背景及后续出现的纯黑带。
- 概览逻辑焦点变化时原生激活底层窗口的缺口，以及控件 disabled 后 DOM 焦点落到 body、第一次 Escape 无法关闭的缺口。

**尚有明确残留：**最小化窗口恢复／调整尺寸时，DWM 实时缩略图仍可能短暂显示旧尺寸客户内容。最终录像 3.666667 秒再次捕捉到窗口 C 的该现象，不能把本轮描述为所有视觉问题已解决。经有界调用链分析，当前代码查询的是 DWM source size，并未直接用 GetWindowRect 当纹理尺寸；公开接口没有客户 repaint epoch 保证。现有静态缓存也不能保证覆盖这次首次冷恢复。本轮未增加无法保证修复该例的缓存模式，未引入猜测性等待或新的捕获架构。后续需要明确捕获时机、延迟与非实时画面的取舍。

## 实现与审查

| 问题 | 实现 |
| --- | --- |
| controller 拥有 HWND，却仅等待 Rust channel | bounded mpsc + Win32 event + MsgWaitForMultipleObjectsEx，空闲期间仍处理 HWND 消息；保持原 deadline 和线程所有权 |
| #262626 与壁纸切换 | 公开 IDesktopWallpaper/WIC 读取并绘制静态背景 |
| 先清黑、后绘图片的中间态 | 离屏预绘完整背景；新 show 在透明状态完成提交后才 opaque；已有 cover 不降透明 |
| 概览键盘焦点 | 共用 native Focus gate；Dismiss 后交接；useSurface 仅在可见且有焦点的 document 中恢复 body 丢失的 DOM 焦点 |

分阶段独立 review 均完成。最新增量 review 对 `3fcabf5 → 474a30f` 为 **No issues found / OK with notes**，未将单测或源码次序当作 DWM 呈现证据。

Wallpaper 状态曾有文档歧义。最终核对 Windows SDK `ShObjIdl_core.idl:7898` 明确的 “This is normally true, unless the Enable method is used”，结合该 interface 的 Enable 契约后使用 DSS_ENABLED；**没有要求 DSS_SLIDESHOW**。没有为了测试改变用户壁纸。

## 从失败到通过的证据

1. 原始录像与报告：[windows-video-2026-10-02.md](windows-video-2026-10-02.md)。
2. 纯诊断版本再次失败：[windows-video-diagnostic-2026-10-02.md](windows-video-diagnostic-2026-10-02.md)。第二次拖放没有进入 Rust；纯 `2+2` 求值也超时。
3. 离线应用转储显示 controller 在 mpsc recv_timeout；主线程在 GetMessageW，WebView browser 主线程在 PeekMessageW。用户态转储没有内核输入队列等待图；不能单凭这些栈声称完整死锁因果已证明。
4. 纯消息泵修复版本和关闭 Rust trace 的集成版本，均完成第二次同页堆叠与状态读取，支持该修复解决本次现场问题。
5. 后续录像发现黑带及首次 Escape 问题，继续修复后增加原生 HWND/foreground 与 DOM focus 分层断言。
6. 最终回归 20 项检查全部通过，黑带区间像素候选由 2 帧降为 0 帧；旧尺寸客户内容仍单独记录为未解决。

## 最终录像与断言

文件目录：

`D:/project/e-desktop/target/recordings/2026-10-02-final/`

- [desktop-live.mp4](../../target/recordings/2026-10-02-final/desktop-live.mp4)：32.466667 秒、1920 × 1080、811 帧，平均约 24.98 帧／秒，无音频。
- `events.jsonl`、`results.json`：20 项全部为 true，录制进程 exit 0。
- `native-open.json` 及各阶段 native probe JSON：只枚举测试应用与 fixture 的 HWND，记录 foreground、visible、DOM activeElement/document.hasFocus。
- `black-bands.json`、`scroll-frames.jpg`：黑带候选与连续录制帧。
- [client-redraw-residual.jpg](../../target/recordings/2026-10-02-final/client-redraw-residual.jpg)：3.666667 秒尚存的旧尺寸客户内容。
- `cleanup.json`：测试进程退出、本轮虚拟桌面关闭、原始虚拟桌面核对恢复。
- `SHA256SUMS.txt`：被测程序与原录像哈希。

| 原录像约时段 | 检查 |
| --- | --- |
| 03–07 秒 | 连续滚轮、反向与快速 retarget；位置 0 → 3000 → 0 |
| 14–16 秒 | 拖动中 Escape 取消，布局不变 |
| 16–18 秒 | E 移入后台 page-9，当前页仍为 page-2，overview 保留原生和 DOM 焦点 |
| 18–20 秒 | D/C 实际同列；第二次 pointerdown/up 正常；独立 JS 求值返回 4；overview 保持焦点 |
| 20–22 秒 | 列宽变化；原生 foreground 仍是 overview，DOM activeElement 是 dialog section |
| 22–23 秒 | 第一次普通 Escape 后，overview HWND 的 IsWindowVisible=false |
| 23–26 秒 | 两轮重开／关闭，每次分别核验 visible 和焦点，未仅凭脚本标签认定成功 |
| 26–28 秒 | 点击 B 卡片，概览隐藏，逻辑 focus 与 OS foreground HWND 都交给 B |
| 28–32 秒 | 工作区切换、无 controller errors、暂停并恢复初始窗口矩形与非最小化状态 |

最终回归关闭 Rust trace，`app.err.log` 为 0 字节。CDP 的只读 DOM 事件探针仍启用。点击、拖放、滚轮和 Escape 是 Win32 SendInput 合成输入，不是人工硬件输入验收。

### 黑带对照

相同检测器检查两份录像的 3.2–6.6 秒区间：

- 早先集成版：88 帧中发现 2 个候选，PTS 4.133333 和 6.433333；人工及原始解码 ROI 确认为 RGB 0 黑带。
- 最终版：84 帧中 **0 候选**，并人工检查连续帧，未见同类黑带。

检测器对固定裁切区域缩小后寻找整列近黑像素，用来定位候选，不是通用视觉质量指标。录制帧率与采样间隙不能排除更短的瞬态异常，合法纯黑桌面也不能用该指标判断错误。

## 自动检查

在集成源码上执行：

- `npm test`：50 passed。
- `npm run typecheck`：通过。
- `cargo test --workspace --locked`：163 passed。
- `cargo test --workspace --no-default-features --locked`：154 passed。
- `npm run tauri -- build --debug --no-bundle`：通过。
- `git diff --check`：通过。

消息窗口回归有旧 mpsc 等待的失败负对照；Focus gate/DOM 恢复也有删除补丁后的失败负对照。memory-DC 检查未创建可见窗口；部分和空 clip region 实验都返回完整 scanline 数，实际像素遵守剪裁。

## 清理与限制

本轮 app PID 60940、fixture PID 65448；仅这 5 个 disposable fixture 窗口在启用前通过隔离检查。测试于约 16:59 完成，正常暂停还原已验证，测试进程退出。本轮记录起始桌面 GUID，并通过公开 IVirtualDesktopManager 查询核对 `originalDesktopRestored=true`。之前准备失败留下的空测试桌面未擅自删除。

本轮没有更改用户配置、壁纸，也没有录制其他应用内容。大文件位于 Git 忽略的 `target/` 下，执行 `cargo clean` 前需要备份。

未验证人工 Alt-Tab、硬件鼠标、高刷新率、广泛第三方应用、动态壁纸、所有混合 DPI/壁纸模式和长期资源表现。背景缓存有明确预算但不等于进程总内存上限，冷准备延迟也未完成专项测量。仍不能保证第三方应用客户区重绘与 DWM thumbnail 尺寸同步。
