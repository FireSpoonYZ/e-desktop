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

父级脚本：`target/handoff-recovery/run-pilot.py`。目前仅执行 plan-only 和语法检查，没有运行 `--run`。

预检为 baseline / prehide / staged × 0 / 60 / 300 ms 迟绘，共 9 次独立冷启动，轮换模式顺序。目标 visible geometry 为 `(16,16,1500,2034)`；针对此前的 4K 主屏，记录原生 `1600×2080` 区域的无损 FFV1，不缩放。实际采样 PTS、吞吐和颜色保真还需现场检查。

在独立虚拟桌面执行；每次使用新 fixture PID/HWND/tag，记录原始桌面 GUID，独立核对还原。先做 state-write 故障注入。脚本遇到拒绝、降级、超时或还原异常即停，不将它们作为视觉通过。所有进程退出与原桌面恢复需保留检查结果。

9 次预检仅用于验证执行和判读链路。随后按设计先补足每格 3 次，再对关键格至少 30 次。baseline 必须复现相关缺陷，否则实验没有证明区分能力。阶段 0 确实改善后，仍需正式帧协议、呈现状态机与渲染接入，并回归原五窗口路径。
