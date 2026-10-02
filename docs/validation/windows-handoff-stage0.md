# 冷恢复交接：阶段 0 进度与离线证据

当前完成的是隔离实验与离线验证；尚未通过实机门槛，未接入正式渲染。

## 候选及审查

主仓库合入：

| 内容 | 提交 |
| --- | --- |
| fixture、像素分析器及测试 | `07ff79a` |
| fixture 显式 UI 异常策略 | `538c3f3` |
| 独立三路径 probe | `1943022`、`b357175` |
| COLORONCOLOR、离线互通与每次提交的 pose/QPC | `75f5ada`、`f012f0c`、`cf47263` |

独立 reviewer 对原候选逐文件审查了 11 个新增文件，结论为仅作为隔离实验候选可合入。原始报告保存在 `target/handoff-recovery/review/stage0-initial.md`。身份/保护准入、单捕获线程及资源所有权、迟到结果隔离、消息泵、还原与三路径分离未发现已确认的阻断问题；这不代表实机验证通过。

两项审查意见已修改：

- P1：GUI 分支在创建 Form 前设置 `UnhandledExceptionMode.ThrowException`，复用 Main 的 stderr/exit 1。离屏分支先返回。父级重新编译、自检通过；UI state-write 故障注入仍未执行。
- P2：实际 capture reduction 和 proxy StretchBlt 显式使用 COLORONCOLOR。增加仅操作文件/memory DC 的 `--offline-proxy`，调用运行时相同的 `Dib::render`。父级审查新增 363 行范围的实现、测试及文档，并复跑离线链路。未缩小 capture 标为 identity；两次缩放组合、DWM live 及有损证据继续不自动判定通过。

官方异常策略参考：

- <https://learn.microsoft.com/en-us/dotnet/api/system.windows.forms.unhandledexceptionmode?view=netframework-4.8.1>
- <https://learn.microsoft.com/en-us/dotnet/api/system.windows.forms.application.setunhandledexceptionmode?view=netframework-4.8.1>

Automatic 的异常路由可能不进入 Main catch；没有实测默认异常弹窗，不把文档示例弹窗当成默认行为证明。

## 父级复跑

在主仓库 `cf47263`：

- Rust 8 项通过，构建和严格 Clippy 通过。
- fixture 编译及无窗口 self-test 通过。
- 既有分析器 29 项测试此前由父级在原候选 worktree 重跑通过；P2 没有修改分析器。
- 真实 C# fixture renderer → 构造带客户区偏移的整窗 BGRA → probe 实际 GDI memory-DC 缩放 → 原分析器，4 组正例通过。
- 每组损坏一角、损坏客户区内部、使用错误客户区偏移的 3 个负例，共 12 个，全部得到 unknown / exit 2，没有假通过。

父级复跑命令：

```text
python experiments/window-handoff-probe/test_proxy_interop.py --probe experiments/window-handoff-probe/target/debug/window-handoff-probe.exe --fixture target/handoff-fixture/handoff-fixture.exe --analyzer scripts/analyze-handoff.py --output target/handoff-recovery/parent-interop-01
```

输出保存在 `target/handoff-recovery/parent-interop-01/`，包含输入、无损输出、分析 manifest、正负对照结果。正例分别比较 326900、183750、511000、261218 个客户区像素，共 1282868 个。四角标记通过之外，还要求全部投影客户区 RGB 像素与参考一致。

这些结果只验证生产者像素。客户区由真实 fixture renderer 绘制；外框和 padding 是离线构造值，不是实机非客户区或 PrintWindow 捕获。运行时新增 `presentation_submitted` 只记录 API 返回后的提交时刻，不是 DWM 显示确认。

## 待执行的实机预检

父级脚本：`target/handoff-recovery/run-pilot.py`。已完成 plan-only 和语法检查；首次 `--run` 在隔离检查阶段中止，未启动录像、故障注入或 probe。

预检为 baseline / prehide / staged × 0 / 60 / 300 ms 迟绘，共 9 次独立冷启动，轮换模式顺序。目标 visible geometry 为 `(16,16,1500,2034)`；针对此前的 4K 主屏，记录原生 `1600×2080` 区域的无损 FFV1，不缩放。实际采样 PTS、吞吐和颜色保真还需现场检查。

在独立虚拟桌面执行；每次使用新 fixture PID/HWND/tag，记录原始桌面 GUID，独立核对还原。先做 state-write 故障注入。脚本遇到拒绝、降级、超时或还原异常即停，不将它们作为视觉通过。所有进程退出与原桌面恢复需保留检查结果。

9 次预检仅用于验证执行和判读链路。随后按设计先补足每格 3 次，再对关键格至少 30 次。baseline 必须复现相关缺陷，否则实验没有证明区分能力。阶段 0 确实改善后，仍需正式帧协议、呈现状态机与渲染接入，并回归原五窗口路径。

## 首次桌面准备结果

`target/recordings/handoff-pilot-01/report.json` 记录 0 次 trial。父级脚本将 NVIDIA Overlay 与 NapCatQQ-Desktop 的 4 个分层辅助窗口按普通应用窗口拦截；这不是已证明的产品缺陷或像素遮挡。详细样式与矩形在该目录的 `isolation-details.json`。

所有自建进程已退出，父级通过原始 HWND/GUID 复核原桌面 `current=true`。当时没有强行关闭测试桌面，因此留下一个测试桌面。父级随后修复了 runner 在“拒绝关闭桌面”分支也跳过返回原桌面的缺口。

下轮区分普通应用窗口与带 tool/layered、nonactivating 或 click-through 样式的辅助窗口，并保留后者记录。样式不证明像素不可见；probe 的 cover 检查及实际画面仍须验证其影响。`pilot-02` 尚未执行。说明保存在 `target/handoff-recovery/pilot-01-blocker.md`。

## 后续预检与原生 API 修复

- `pilot-02` 在读取原子替换中的 fixture JSON 时遇到 PermissionError，仍未录制或调用 probe。父级读者现改用共享 read/write/delete 的文件句柄与有界重试，避免妨碍生产者替换。此轮自建进程退出、原桌面经单独复核恢复，测试桌面留开。
- `pilot-03` 通过隔离和故障注入，但 FFmpeg 拒绝 `stats_period=.1`，未调用 probe。参数已改为 `0.1`。此轮测试桌面关闭、原桌面恢复、进程退出均通过。
- `pilot-04` 能录制，但首个 baseline 在变更源窗口前拒绝：`get transitions: HRESULT 0x80070057`，`targetModified=false`。此轮还原/退出核对通过。

`pilot-03` 和 `pilot-04` 的 state-write 故障注入均在父级 5 秒上限内以 exit 1 结束，没有人工输入；没有单独检查短暂弹窗。

官方 `DWMWINDOWATTRIBUTE` 文档将 `DWMWA_TRANSITIONS_FORCEDISABLED` 列为供 setter 使用，不能用失败的 getter 猜测原值。修复 `3c758f4` 删除 probe 的该属性读取、设置及恢复，明确记录 unchanged-by-probe / originalQueried=false；`27dd108` 由 fixture 在自己的 OnHandleCreated 中明确设置初始禁用过渡策略并检查 HRESULT。全部模式初始条件相同，这仍不是正式程序时序等价证明。父级审查、重建、8 项 Rust 测试及 fixture 离屏自检通过，新的 GUI 路径尚未复验。官方摘录：`target/handoff-recovery/dwm-transition-contract.txt`。

录像预检 `pilot-04/r0-baseline-d0/screen.mkv` 为 FFV1 / bgr0、1600×2080，实际 11 帧，PTS 间隔为 33/34 ms，保留 wall-clock PTS。它仅录到正常窗口和拒绝过程，没有冷恢复动画。父级检查了第 5 帧的窗口局部图。四角均为 G1 / 698×444 / C；完整客户区比较仍为 unknown：309912 个像素中 62 个不匹配，均在底部两角最后 10 行，与图中圆角裁剪位置一致。没有把四角一致或其余像素匹配当作完整画面通过，原始差异保存在 `pixel-mismatch-detail.json`。

## 遮罩与恢复中间状态的实机证据

`pilot-05` 在 Shell 的 `VirtualDesktopHotkeySwitcher` 仍显示时中止。父级增加只等待该实际窗口消失的有界检查；不是给客户重绘增加固定等待。该轮原桌面恢复、进程退出通过，测试桌面留开。

`pilot-06/07` 执行到最小化与遮罩检查，但未完成目标恢复。父级增量 `bf9cfb8` 记录具体遮挡者；`pilot-07` 显示被拦对象为 Explorer 的 `ThumbnailDeviceHelperWnd`，1×1 像素，DWM_CLOAKED 查询成功且值为 1。`7714f38` 因而仅排除 DWM 明确确认 cloaked 的窗口；查询失败及真正可见窗口仍按原规则检查。

`pilot-08/09` 随后执行到 cover_presented 与 target_apply_requested。`d63713b` 增加 monitor/DPI/outer 的失败现场字段。`pilot-09` 捕捉到：

- IsIconic=false；outer=(-32000,-32000,237,39)；MonitorFromWindow 返回 0。
- 当前 DPI 与预期均为 144，当前 workarea 与预期均为 (0,0,3840,2088)。
- 该状态发生在 controller 已发出恢复请求、实际位置尚未落位时。不能将此记录写成真实的 DPI 或工作区改变。

这几轮均因检查拒绝而失败，不是视觉通过。`pilot-06` 至 `pilot-09` 的窗口还原、测试桌面关闭、原桌面恢复与所有自建进程退出均核验成功；没有关闭 NVIDIA 或 NapCatQQ。

已恢复原 probe worker 修复这段有界恢复等待：许可只限本 controller 发起的恢复范围，仍保留身份、保护、实际工作区/DPI、遮罩及可见遮挡检查；源真正映射到其它屏幕或离开遮罩仍拒绝。成功/错误/取消/超时后不得遗留许可；恢复完成后必须重新严格校验。修复尚未交付或实机复验。
