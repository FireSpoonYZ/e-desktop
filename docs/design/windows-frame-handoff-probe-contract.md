# 冷恢复阶段 0：实验开发边界与互通约定

状态：开发受控对照实验，尚未证明方案有效。设计见 windows-frame-handoff.md。

## 分工

| Owner | 独占范围 | 责任 |
| --- | --- | --- |
| probe worker | experiments/window-handoff-probe/** | 独立 Rust 原生三路径驱动、纯逻辑/memory-DC 测试、CLI、README |
| fixture worker | scripts/handoff-fixture.cs、scripts/build-handoff-fixture.ps1、scripts/analyze-handoff.py、scripts/test_handoff_analysis.py | 布局代号/角标/可控迟绘目标窗口、仅编译脚本、离线像素分析与测试 |
| 父级 | 本约定、启动/录像/集成脚本、实机执行及正式接入决定 | 接口协调、review、合并、验收 |
| reviewer | 只读候选提交 | 检查实验有效性、资源/线程/恢复与测试边界 |

这是多组件工作。workers 各用独立 worktree，只编译和执行无可见窗口测试，不启动 fixture/probe 的实机模式，不操作桌面、录像或输入。报告使用工具绑定 artifact。

阶段 0 不改根 Cargo workspace members、不接正式 renderer、不改 src-tauri/src 或已有 smoke fixture。需要越界先请求父级，不私自扩大。

## Fixture 协议

建议 CLI：

    handoff-fixture.exe --tag TOKEN --role C --state-file PATH --paint-delay-ms 0

- 每进程一个普通 WinForms 窗口，父级可启动 A..E 五个进程并记录 PID 集合。
- 标题严格为 e-desktop Handoff <tag> <role>；tag 限 ASCII 字母数字/短横线。
- C 初始可见大小尽量匹配既有 702×491，记录实际 physical outer/client/DPI，不能把 WinForms logical Size 当 physical。
- 显示角色、desired/painted layout generation、客户尺寸；客户四角有可离线识别的 generation 角标。worker 明确编码、颜色、边距、解码公式，并提供纯像素检查。
- resize 更新 desired generation；延迟结束前继续显示旧客户位图，之后绘制新大小/代次。延迟范围 0..2000 ms；不得用 Thread.Sleep 阻塞消息循环模拟迟绘。
- state-file UTF-8 JSON、原子替换：schemaVersion=1、tag、role、pid、hwnd十六进制字符串、monotonic/QPC时间及单位、physical outer/client screen rect、DPI、desiredGeneration、paintedGeneration、paintedClientSize。
- --self-test（如提供）只能测试离屏像素/协议，不创建窗口、不 Application.Run。
- 编译脚本默认只编译，不启动窗口。 默认产物名为 handoff-fixture.exe。
- marker/state-file 仅供父级/离线 analyzer 判定；probe 不能用它们决定交接就绪。

## Probe 协议

独立 Cargo crate，自有 [workspace] 与 Cargo.lock，使用必要官方 Windows bindings，不引私有 API、新 GUI 框架或正式 WGC 集成。

建议 CLI：

    window-handoff-probe --run --pid PID --hwnd HEX --tag TOKEN --role C
      --mode baseline|prehide|staged --output DIRECTORY --target x,y,width,height

- 无 --run 不发生桌面副作用；严格校验 HWND/PID/title tag/role、普通非最大化窗口、同屏同 DPI、安全 cover。只修改显式目标。
- 第一次修改前保存 original placement/show/region；退出/取消/失败尝试还原并记结果。
- 每次一个 trial；fixture启动、退出、冷试次重启和录像由父级负责。不跨trial暗中复用帧。
- 保护查询：GetWindowDisplayAffinity 成功返回非 WDA_NONE 一律拒绝；查询失败默认拒绝。仅普通非 layered 且 ERROR_INVALID_PARAMETER，父级显式给出 --fixture-only-allow-unknown-affinity，并满足上述身份条件与映像名 handoff-fixture.exe时，允许受控 fixture 例外。记录完整映像路径供父级与构建/启动清单核对；这不是基于文件名的通用安全认证。
- 此例外结果必须记 protectionMetadata=unavailable、admission=explicit-owned-fixture-exception、原始错误；不可声称API证明unprotected。其它错误仍拒绝。不清除源保护、不切换其style，不把例外接入正式第三方窗口策略。

- baseline：不预捕获，最小化后直接恢复目标尺寸，使用 live DWM 内容。
- prehide：最小化前异步取得整窗图；恢复/改尺寸时用固定图代理。
- staged：无缓存启动；先最小化，opaque cover 后按 last drawable geometry 恢复取图，得到后才改目标尺寸、运行固定图代理。
- 除待测策略外，三路径目标矩形、动画、fixture延迟、输入门禁和记录方式一致。baseline是独立实验对照，不宣称与正式程序时序完全等价。
- 一个在途异步 PrintWindow，窗口控制仍在一个消息感知线程；结果只通知、不直接placement。消费者超时不强杀线程、不释放其在用资源、不增开替代线程。
- 默认预算：prehide 100 ms；staged恢复+捕获150 ms；动画160 ms；target apply后总交接250 ms（包含动画）。实验覆盖参数及单位必须记入结果；不宣称是底层Win32硬时限。
- 完整离屏提交，自有代理层级明确；遮盖完成前不能改尺寸/显露staged源。保持错误命中阻止和消息泵。
- 核验意图代次、窗口身份、capture-time几何；取消后晚到帧不得改变窗口。
- 无图、超时、不支持、资源失败必须记 degraded 与原因，不计为正常呈现通过。
- 不能用marker、fixture状态、两帧一致、尺寸、时间戳冒充客户语义ready。只记录公开可观察条件和有界交接。
- 复用本仓库足够小的GDI/thumbnail模式，不复制整个Backend，不为实验新增产品公共API。

## 输出与分析

每trial输出 trial.json、events.jsonl（UTF-8）、必要的目标fixture捕获 BMP 或raw BGRA+metadata、还原结果。

事件建议：start/original_saved/minimized/cover_presented/capture_requested/capture_completed/capture_rejected/staged_restored/target_placed/proxy_presented/handoff/degraded/cancelled/original_restored/finished。

共同字段：schemaVersion=1、trialId、mode、pid、hwnd字符串、相对monotonicUs及可校准时钟、intent generation。几何明确区分physical outer/visible/client；图片记录pixelSize及捕获时几何。

统计capture等待、handoff等待、额外延迟、输入遮挡解除、降级原因、还原成功。exit 0 本身不是视觉通过。

离线analyzer可以用已安装FFmpeg解码；不安装新图像框架。必须区分未知/遮挡/裁掉/压缩损坏，不能把无法识别算通过。单测使用构造像素或离线文件，不屏幕截图。

## 正式接入门槛

父级review后执行同一cold-C对照；不能事先人为暖好C。画面混合代次重复减少/消失、延迟符合预算，且无跨屏泄露、焦点回归、永久遮挡、旧回调动作或无界资源后，才推进正式帧协议、呈现状态机和代理渲染。

未通过则记录证据/具体阻塞，不能用原型API/几何断言宣称实机成功。
