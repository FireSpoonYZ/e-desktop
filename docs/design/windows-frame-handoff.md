# Windows 冷恢复与 resize：稳定画面交接设计

状态：设计提案，未实现，未运行新的实机实验。

基线：450321acea49b624380f3412e1f99e93338f5a48。现有消息泵、背景黑带和焦点修复继续保留。问题证据见 [录像修复回归](../validation/windows-video-fixes-2026-10-02.md)：窗口 C 首次恢复时，非客户区已扩展，客户内容仍短暂保持旧尺寸。

## 1. 推荐决策

推荐采用 **变更前取得画面 → 冻结整窗代理 → 在遮盖下修改真实窗口 → 有界交接**。

先用现有 PrintWindow、位图缓存、GDI 与 compositor 做小范围可行性验证。不要先把 WGC 接入正式渲染，也不要仅在 DwmFlush 后加延时。第一次隐藏前的捕获和已经最小化且无图的路径都必须纳入验证。

目标分开：

1. **可控制的目标**：变更尺寸的动画期间，标题栏和客户内容来自同一张自有位图，不再持续采样正在发生尺寸变更的第三方 live surface。
2. **实测目标**：消除当前 WinForms fixture 在冷恢复中的小块旧客户内容，并验证常用应用。
3. **不能作通用承诺的目标**：任意第三方应用都在指定时间完成语义重绘。窗口尺寸、DWM flush、WGC 时间戳或连续相同帧都不提供这个保证。

“完整帧”在本设计中只表示完整、有效地取得并持有了一幅像素数据，**不表示应用业务绘制已经完成**。捕获到的图本身可能已经包含旧内容。

## 2. 先分流，不让所有窗口都承担捕获成本

| 情况 | 推荐路径 | 边界 |
| --- | --- | --- |
| 可见窗口，仅平移、没有改变源尺寸 | 保留现有 DWM live thumbnail | 无需为了平移冻结视频内容 |
| 将 resize，且有可用的变更前整窗图 | 固定该帧，代理动画；真实 HWND 在后方修改 | 旧帧不能冒充新的客户就绪状态 |
| 第一次将被 manager 隐藏／部分裁剪，仍可完整捕获 | 在破坏可捕获状态前，尝试有界的异步关键捕获 | 捕获可能失败，不能保证每个窗口都命中 |
| 已经 manager-minimized，没有可用图 | 单独验证“旧几何恢复取图”路径 | 不能直接恢复到新尺寸，再把第一帧当作可靠旧图 |
| 受保护、无可信旧几何、无法安全遮盖或捕获失败 | 显式降级到现有真实窗口路径 | 可能仍有补绘，不把降级计为无残影通过 |

概览的长期后台静态预览仍是原来的 best-effort 功能。动画期间固定帧是新的短生命周期用途，不应通过永久关闭可见窗口实时预览来实现。

## 3. 三种准备路径

### A. 有图的 resize／恢复

1. 在改变真实窗口尺寸之前，确认帧身份、捕获时的几何和当前生命周期匹配。
2. 为本次过渡固定该帧；动画期间不随后台预热结果更换。
3. 按当前显示位置绘制完整代理，准备背景与输入遮盖。
4. 确认自有 cover/proxy 已提交后，真实窗口移至最终尺寸。
5. 让真实窗口在后方重绘，前台只变换代理的位置、大小和裁剪。
6. 动画结束进入交接阶段。

冻结图片意味着动态内容暂时停止、旧截图可能过时，放大文字可能变模糊或失真。必须把这作为明确的体验取舍，不能宣称仍然是实时内容。

### B. 第一次隐藏前尚可取得图

现有 save() 会在首次修改前保存原状态；这是捕获授权与恢复责任建立后的合适入口。但当前 snapshot_before_hide() 仅接收完成结果并取消旧任务，不会等待新图；500 ms 全局／5 s 单窗节流也不是关键过渡调度器。

建议：

- 保留普通预热的节流；增加仅供将要失去可捕获性的窗口使用的关键优先级。
- 关键任务可绕过普通预热的时间门禁，但仍共享一个在途 PrintWindow worker，不能抢占已经阻塞的调用。
- 待捕获列表只存有界请求元数据，同一窗口合并为最新需求。首轮验证可从最多 8 项开始。
- 首次隐藏/裁剪可在明确预算内延后，控制器继续处理 HWND 消息、IPC、禁用与新意图。
- 帧完成、失败或期限到达后，由控制器检查当前意图并决定是否仍执行隐藏。
- 捕获完成必须主动唤醒控制器，不能只等原来的 500 ms 枚举周期。优先复用现有请求队列/event 的轻量完成通知；通知只携带任务标识，像素仍留在有界结果存储。队列繁忙时结果不能丢失，pending 标志与最近准备 deadline 作兜底；唤醒和取消竞态要单测。
- 多窗口共用一段准备预算，不是每个窗口串行再等 100 ms。

**必须处理“可见但未完成”的布局过渡。** 延后隐藏的窗口不能被留在前景遮住新布局；需要旧场景/cover 保护。不能安全遮盖时立即降级，而不是无条件推迟最小化。

不能在现有 Backend.apply() 内发起异步捕获后直接返回 Ok 假装 placement 完成。需要独立的准备结果和待执行意图；完成之前不更新 placements/observed geometry 为目标值。刷新逻辑也必须知道这是管理器的待执行操作，不能误认为用户手动移动。

### C. 已最小化、无图的真正冷恢复

这是本轮实际问题必须覆盖的路径。推荐先做以下受限 PoC：

1. 找到本窗口最后已知可绘制的正常几何、边框裁剪和 DPI。
2. 先提交完整、不透明的安全遮盖区域。
3. 在遮盖后按该旧几何恢复窗口，**先不改成目标布局尺寸**。
4. 异步按 HWND 取得该旧几何下的整窗位图。
5. 得到有效位图后固定它，再修改真实 HWND 到目标尺寸，运行代理动画与交接。

这把“从最小化恢复”和“改变源尺寸”拆开，避免一开始就要求 DWM 提供新尺寸下的完整客户内容。它是值得验证的机制，不是已证明可靠的实现。

严格前提：

- 只处理本管理器有恢复责任、且本次布局明确要求恢复的窗口；不会为了预热任意恢复用户主动最小化的窗口。
- 使用 last drawable geometry，不能拿用于退出还原的原始 undo rectangle 代替。二者用途和时间不同。
- 首轮仅支持同屏、同 DPI、能完全藏在已验证 cover 内的普通矩形窗口；先排除原生 maximized/fullscreen 和独立弹窗等状态。后续扩大范围时需要一起保存可绘制 show state，不能仅靠一个尺寸重建。
- 旧位置跨屏、尺寸超出遮盖、原生全屏/暂停显示器、独立 owned popup 无法遮住等情况，拒绝此路径。
- 不能随意搬到另一块屏幕“藏起来”，否则可能触发新 DPI/layout 变化。
- 不使用私有 cloak、不截取桌面来冒充源窗口画面。
- 遮挡后目标可能减少或停止绘制，恢复本身也可能触发应用内部布局；旧几何不是 semantic-ready 证明。

若此 PoC 不能改善真实 cold-C，不应继续堆叠几何判断，应保留降级并重新评估捕获路线。

## 4. 状态机与取消规则

拟议状态（不是现有 API）：

    LIVE / HIDDEN
          |
          v
       PREPARING -- timeout / unsupported --> DEGRADED
          |
          | 固定有效帧，完成自有画面提交
          v
       ANIMATING
          |
          v
     HANDOFF_PENDING -- deadline --> DEGRADED
          |
          | 完成正常交接
          v
         LIVE

每个状态都允许：新意图、窗口销毁、disable、显示器暂停／移除或 device/capture error。

- Engine 的布局命令仍原子提交。准备和交接是原生呈现状态，不拆成“移页、激活、再 drop”多个命令。
- 回调只能投递结果与唤醒；不能直接移动 HWND、调用 Engine 或重新执行旧布局。
- 控制器线程核验结果，保持现有消息感知等待。禁止 controller join/sleep 等待捕获线程。
- 新布局增加 intent generation；旧回调不能触发旧 placement、显示旧 cover 或覆盖最新目标。
- retarget 从当前显示姿态继续，不能回跳到捕获时的屏幕位置；可以继续固定同一幅旧图，但必须重新计算最新目标。
- 准备期间旧动画可以继续；新过渡开始时从当时的显示姿态接续。不能把准备时间消耗成“动画已跑完”。
- disable 优先取消呈现意图、移除遮盖并走现有还原路径。晚到图只能释放，不能再次改变窗口。
- 禁止在 handle/PID 重用后使用旧帧；继续保留现有 cookie/PID 身份校验。

## 5. 帧数据与窗口意图分开

建议扩展现有 Frame，而不是再建一套不受预算控制的缓存。以下字段是拟议内部元数据：

    CapturedFrame
      window_identity        // HWND + PID + lifetime cookie
      capture_id
      source_geometry_id
      source_outer_rect
      source_visible_rect    // 捕获时边框/padding，不能使用当前新 padding 回算
      source_dpi
      pixel_size             // 实际缓存尺寸，可能已降采样
      captured_at
      pixels                 // 自有不可变数据

    PresentationIntent
      generation
      target_rect / clip / visibility
      phase
      prepare_deadline
      handoff_deadline
      pinned_frame_id

原始尺寸、缓存像素尺寸、目标矩形必须是三个不同概念。窗口屏幕原点改变不一定使图片失效；源尺寸/DPI/边框变化则必须换代或明确使用旧代做历史代理。

帧选择也区分用途：可见窗口的 resize 优先使用当前源几何下的新图，首轮可把 250 ms 帧龄作为试验门槛；超过则申请关键捕获，不默默退回几秒前的动态内容。隐藏窗口允许使用该会话中最后一次隐藏前的历史图，并明确其静态性质。这个帧龄门槛同样需要实测调优，不是 API 保证。

缓存中的帧可以是历史图，但不得因为完成得晚就被标为“当前布局已完成重绘”。fixed frame 的身份有效、几何一致、数据有效，与真实客户端完成语义绘制是不同条件。

## 6. 渲染方式与交接

### 原型先用现有 GDI，正式路径必须验证层级和开销

复用位图、裁剪及完整离屏提交经验，禁止在可见 DC 先清背景再逐块补画。

不要简单把 frozen 图片画到壁纸层、再让所有 DWM live thumbnails 盖在上面；这种混合方式不能表达任意窗口重叠顺序。

首轮原型建议：参与冻结的监视器，在该次过渡中统一使用完整快照场景绘制，其他监视器继续原路径。按显式场景顺序将必要窗口帧绘入同一离屏场景，再一次提交。只准备当前/目标/运动路径可见的窗口；缺图按事先定义的占位或降级策略处理，不伪造 live。

这意味着该监视器上的相关动态内容在短动画中暂时冻结，是第一版的简化。不能证明性能与层级正确之前，不把混合 GDI/DWM host 分层方案直接推广到所有场景。

背景解码和像素源复用，不在每个 tick 重新 WIC decode 或 PrintWindow。额外场景工作缓冲先设全局 64 MiB 预算；同一 monitor 复用缓冲，预算不足就降级。只更新 dirty union 能否满足性能目标，需要先测量，不预先增加复杂 GPU 渲染框架。

若 CPU/GDI 路径达不到目标帧时间，再用公开 DirectComposition/Direct2D/D3D 的自有 surface 替换绘制层。不得依赖私有 shared thumbnail visual ABI。

### 真实窗口交接

满足以下条件才能作为正常交接候选：

- 当前意图未过期，源身份仍有效；
- 真实窗口已处于最新目标几何；
- 原生可见性、裁剪与恢复责任一致；
- 若取得变更后的帧，其捕获时序和源几何属于此次变更；
- 代理仍覆盖着窗口，直到交接的显示／输入顺序完成。

这些只是候选条件，不叫 ClientReady。尤其是 PrintWindow 的完成时间晚于 resize，不代表它一定包含新的业务绘制；WGC 的 compositor timestamp 也一样。

目标是让真实窗口在代理动画的约 160 ms 内并行重绘，减少动画末尾等待。正常情况下动画结束即可交回真实窗口。确需等待时受绝对 deadline 限制，不把超时不断延期。

不永久保留一张挡住输入的旧图。超时后移除代理并恢复真实窗口的交互，记录降级原因；可能看到后续补绘，不能记作无残影成功。跳过动画也只是降级，不会自动消除真实窗口 resize 的补绘。

保留已修复的焦点规则：overview/commands 打开时不向底层窗口原生交焦点；关闭后再交接。代理阶段阻止错误命中，不转发/重放第三方输入；已有 gesture release/cancel 必须正常排出。

## 7. 时间与内存预算

下列是实验起点，不是测得的性能或公开 API 保证：

| 项目 | 初始预算/策略 |
| --- | --- |
| 首 hide 前准备 | 一批关键窗口共 100 ms |
| cold staged restore + 取图 | 合计 150 ms，单独计入额外延迟 |
| 原动画 | 保留当前默认 160 ms |
| 目标几何设置后的交接 | 绝对上限先取 250 ms，包含动画时间；不是动画后再等 250 ms |
| PrintWindow | 维持 1 个在途任务；卡住不增开替代线程 |
| 缓存帧 | 沿用总 32 MiB、单帧至多 4 MiB、长边 1024 的起点 |
| 原始捕获 DIB | 沿用 32 MiB 上限 |
| 新场景工作缓冲 | 全局至多 64 MiB，与背景缓存分开记账 |

固定帧必须计入同一帧存储预算。不能从 LRU 删掉条目却让动画 Arc 继续持有，从而绕过 32 MiB 限制。所有帧都已固定且无可回收空间时拒绝新冻结任务，不能无界分配。

这些预算只限制本方案主动增加的等待和存储；不能宣称它们强制终止同步 Win32/PrintWindow 调用。消费者超时后，仍在执行的 worker 继续拥有自己的 DC/bitmap，直至实际返回或进程结束。相关临时分配、原有 wallpaper cache、window backing 和 WIC 内部分配不在“32 MiB 帧缓存”中。

## 8. 故障表

| 故障 | 行为 |
| --- | --- |
| 旧帧不可用 | 尝试受限 cold path；不满足前提直接降级 |
| PrintWindow 失败、全黑或不支持 | 不覆盖已有有效帧；记录原因 |
| PrintWindow 卡住 | 到期停止等待，不强杀线程、不释放其资源、不不断创建新线程 |
| 新意图/disable | 取消旧呈现意图；迟到结果不能触发动作 |
| HWND 重用/源销毁 | 丢弃帧，释放代理，禁止访问新实例 |
| 目标应用拒绝尺寸 | 复用现有拒绝/还原逻辑，不永久维持遮盖 |
| 无法确认新内容可接受 | 到 deadline 降级交回真实窗口，明确未保证无残影 |
| 显示器/DPI/区域变化 | 重新核验几何与 cover；不满足则取消该代理路径 |
| 内存/设备/绘图失败 | 保留现有安全 cleanup/native fallback |

## 9. WGC 的位置

WGC 值得作为第二阶段的像素获取对照实验，尤其是 PrintWindow 对 Chromium/Electron/DirectComposition 内容支持不足时。现有 wgc-probe 只证明成功取得过 D3D11 帧，不能直接当作产品级捕获服务。

WGC 真正提供的是可复制、自持的帧数据与时间/尺寸信息。仍需处理 HWND item 生命周期、FrameArrived 线程、frame pool Recreate、设备丢失、保护内容、可选捕获边框权限和 HDR。归还帧池前必须复制需要固定的像素／纹理；不能把池内 surface 当永久缓存。

若问题主要是目标应用迟绘，换成 WGC 仍可能捕获同样的过渡画面。因此先证明“能在正确时机取得一幅用于过渡的图”，再决定是否值得引入持续捕获。

## 10. 实施拆分与验收门槛

### 阶段 0：最小实验，不接正式渲染

对原 five-window fixture，比较：

- 直接恢复到目标尺寸（当前基线）；
- 首次 hide 前捕获；
- 无缓存时，遮盖后按旧正常几何恢复、捕获，再改目标尺寸。

沿用冷启动流程，不能先人为暖好 C 来回避原问题。记录申请/完成捕获、源 outer/pad/DPI、意图代次、目标设置、代理固定、解除遮盖、foreground 和 DOM focus 的时间。

fixture 增加可读的布局代号、客户四角标记及主动延迟绘制的模式，检测整张图是否来自同一布局；这种标记仅用于测试，产品不能要求第三方应用配合。

**进入产品实现的门槛**：原 cold-C 的混合尺寸帧可重复减少或消失，额外等待在预算内，没有跨屏泄露、错误命中、焦点回归或无界资源。若失败，不扩大实现来掩盖实验失败。

### 阶段 1：内部帧元数据、固定和关键捕获

主要修改 snapshot.rs 与 windows/mod.rs 的管理保存/隐藏边界。缓存、固定和捕获结果采用统一预算与身份/代次检查。

必须证明：迟到回调不能执行旧动作、禁用不复活窗口、多窗口准备共用预算、挂住的 capture 不导致线程增长。不要只改 snapshot_before_hide 然后把异步行为伪装成 apply 完成。

### 阶段 2：呈现状态与代理场景

主要修改 app.rs 的待执行呈现意图、animation.rs 的开始/retarget 时序，以及 compositor.rs/专属静态场景绘制。保持 model/wire 的逻辑原子命令，暂不为一个方案建立多实现工厂。

提交划分至少为：帧/捕获协议、呈现状态机、渲染/交接；共享结构先定，一个文件/接口一个 owner。不能只把它分配为 snapshot.rs 的两行修补。

### 阶段 3：分离变量实机验收，再评估 WGC

- 冷 C、已有缓存、连续方向反转、连续 resize、窗口关闭/复用、disable、源挂起/拒绝捕获、预算耗尽。
- 同屏同 DPI 先过，再扩展混合 DPI/负原点、多屏和第三方应用。
- native foreground、窗口 visible 与 DOM focus 分开断言；一次 Escape、卡片交接、暂停还原不能退步。
- 尽可能提高实际录制采样率并报告帧间隔；“未捕捉到”不能升级为“绝对不存在”。
- 原子快照标记、像素残影、输入可用时刻、P50/P95/P99额外延迟、超时/降级比例、固定帧总字节和线程/GDI对象数共同作为验收证据。
- 可从每条关键路径重复 30 次起步；该次数只是冒烟统计，不能证明低概率问题不存在。
- GDI 场景绘制若在目标机器上持续超出一帧预算，不通过降低证据要求发布；再评估 GPU 自有 surface。

## 官方依据

- [PrintWindow：同步阻塞与目标应用绘制](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-printwindow)
- [DwmQueryThumbnailSourceSize](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmquerythumbnailsourcesize)
- [DwmFlush：调用者 queued batch 边界](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmflush)
- [WGC Screen capture：帧生命周期与 resize](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture)
- [CreateForWindow](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow)
- [CreateFreeThreaded](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded)
- [SystemRelativeTime：合成时间](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframe.systemrelativetime)
- [DirectComposition bitmap surfaces](https://learn.microsoft.com/en-us/windows/win32/directcomp/bitmap-surfaces)
- [SetWindowDisplayAffinity](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity)
- [IsBorderRequired 与 Borderless 权限](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired?view=winrt-26100)
