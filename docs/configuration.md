# 配置、快捷键和窗口规则

应用读取 JSON 配置。默认位置是 Tauri 的应用配置目录下 `config.json`；Windows 通常为 `%APPDATA%\local.e-desktop.manager\config.json`。可以用环境变量 `E_DESKTOP_CONFIG` 指定完整文件路径。应用不会自动创建或覆盖这个文件。

控制器每 500 ms 检查一次内容变化。保存后自动加载，不需要重启；删除文件恢复默认配置。JSON、字段、规则或快捷键无效时显示错误并保留上次有效配置。系统拒绝注册新快捷键时会尝试回滚；回滚失败也会显示具体错误。启动时配置无效则尝试使用默认快捷键。

配置不保存现有窗口布局，启动后仍然默认暂停。窗口规则只在一个窗口 ID 首次被发现时执行，修改规则不会重新排列已发现的窗口。

## 文件结构

```json
{
  "windowRules": [
    { "appName": "notepad", "columnWidth": 800 },
    { "title": "计算器", "floating": true }
  ]
}
```

省略 `shortcuts` 使用默认快捷键；`"shortcuts": []` 禁用全部全局快捷键。提供非空 `shortcuts` 数组会**完整替换**默认映射，不与默认映射合并。省略 `windowRules` 或提供空数组均不设置窗口规则。

## 自定义快捷键

```json
{
  "shortcuts": [
    { "key": "Control+Alt+Space", "action": { "type": "commands" } },
    { "key": "Control+Alt+O", "action": { "type": "overview" } },
    { "key": "Control+Alt+Left", "action": { "type": "scroll", "direction": "left" } },
    { "key": "Control+Alt+Right", "action": { "type": "scroll", "direction": "right" } },
    { "key": "Control+Alt+W", "action": { "type": "command", "command": { "type": "setColumnWidth", "width": 800 } } },
    { "key": "Control+Alt+PageDown", "action": { "type": "relativePage", "delta": 1 } },
    { "key": "Control+Alt+Shift+2", "action": { "type": "page", "number": 2, "moveWindow": true } },
    { "key": "Control+Alt+Backspace", "action": { "type": "command", "command": { "type": "disable" } } },
    { "key": "Control+Alt+Q", "action": { "type": "quit" } }
  ]
}
```

支持的动作：

| `action.type` | 参数与行为 |
| --- | --- |
| `command` | `command` 为 [实现契约](implementation-contract.md) 中的命令对象 |
| `overview` / `commands` | 打开概览 / 命令面板 |
| `quit` | 还原后退出 |
| `page` | `number` 为从 1 开始的已有页面编号；可选 `moveWindow` 默认为 false |
| `relativePage` | `delta` 为非零相对偏移；可选 `moveWindow` 默认为 false |
| `scroll` | `direction` 为 `left` 或 `right`，每次滚动活动显示器视口宽度的 1/3 |

页面和滚动动作在执行时读取当前活动显示器，不保存易失效的页面 ID。页面不存在时不执行。`command` 中的 ID 则按提供的值使用；`addPage` 的空 `monitorId` 表示执行时的活动显示器。

默认新增快捷键：

| 快捷键 | 行为 |
| --- | --- |
| `Ctrl+Alt+Left/Right` | 向左 / 右滚动当前显示器 |
| `Ctrl+Alt+Shift+Left/Right` | 减少 / 增加列宽 50 物理像素 |
| `Ctrl+Alt+Shift+Up/Down` | 增加 / 减少聚焦窗口高度 50 物理像素 |
| `Ctrl+Alt+Shift+R` | 当前列恢复等高 |

宽度和高度命令仅适用于聚焦的平铺、非布局全屏窗口。任意列宽通过 `setColumnWidth` 指定：0 无效，大于视口的值夹到视口宽度。高度调整会重新分配同列其余窗口的空间；窗口顺序、视口变化后保留会话内高度偏好。

## 窗口规则

| 字段 | 含义 |
| --- | --- |
| `appName` | 应用名的字面子串，不区分大小写 |
| `title` | 标题的字面子串，不区分大小写 |
| `floating` | 初始浮动状态；不可缩放窗口始终浮动 |
| `columnWidth` | 初始列宽，正整数物理像素，最大为目标视口宽度 |
| `monitorId` | 目标显示器 ID，需与快照中 ID 完全相同 |
| `pageIndex` | 目标屏已有页面的编号，从 1 开始 |

同时提供 `appName` 和 `title` 时必须都匹配；不提供条件则匹配所有窗口。不支持正则表达式或通配符。多条匹配规则逐字段合并，后面的规则覆盖前面规则指定的同一动作字段。每条规则至少有一个动作，条件和显示器 ID 不得为空白。

目标显示器不存在时回退到原生窗口所在显示器；目标页面尚不存在时回退到目标屏当前页面。规则不会创建指定数量的空页。启动时通常只有初始页，因此不能用 `pageIndex` 预创建页面。后台页规则不会主动切页或抢焦点。

窗口名称和 ID 来自原生后端，平台之间可能不同。规则在窗口首次枚举时匹配当时的标题，后续标题变化不触发重新匹配。列宽对浮动窗口无效。

## 顶栏滚动

顶栏的横向滚动区支持左右按钮、鼠标滚轮与触控板。按钮每次 160 物理像素；命令面板也提供相同动作。滚轮像素值按显示器缩放比例转换，按行与按页事件分别使用控件行高和视口宽度；对角滚动只取绝对值较大的轴。Ctrl+滚轮 / 捏合不会触发布局滚动。

输入按 80 ms 合并，每次只允许一个原生请求在途。忙碌、暂停、目标失效或页面切换时丢弃待处理输入，不延迟追赶。此处没有滚动动画，也不全局拦截其他应用的滚轮。
