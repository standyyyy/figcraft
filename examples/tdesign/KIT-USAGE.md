# TDesign 三个库：手动 + MCP 都能用

> 实测：2026-11-10，FigCraft 0.15.0（源码构建，`--control 7979`）

## 现在能用什么

| 库（面板里显示的名字） | 文件 | 组件 | 样式 |
|---|---|---|---|
| `TDesign for web (Community)` | `~/figcraft-libs/tdesign-web.figcraft` | 2615 | 87 |
| `TDesign Mobile · Button 按钮` | `~/figcraft-libs/tdesign-mobile-button.figcraft` | 2281（含 2279 变体） | 10 |
| `TDesign Mobile · Styles 样式` | `~/figcraft-libs/tdesign-mobile-styles.figcraft` | 1（Token/color） | 87 |
| `icons-comp` | `~/figcraft-libs/tdesign-icons.figcraft` | 2358 图标 | 0 |

## 一次性动作（已完成，换机器/重装需重做）

移动端两个文件原本**从未发布为库**，所以资源面板里根本不出现。补发布：

```json
{"command":"open_file","params":{"path":"~/figcraft-libs/tdesign-mobile-button.figcraft"}}
{"command":"library.publish"}
{"command":"save_file"}          // ← 必须，否则发布信息不落盘
```

styles 同理。`tdesign-web` 和 `tdesign-icons` 本来就发布过，无需处理。

顺带把两个重名的库改了文件名（都叫 "TDesign for mobile (Community)" 时，
面板里分不清、按名字调用也歧义）：`file.rename` → 再 `library.publish` → `save_file`。

## 手动用（在 App 里）

1. 左侧面板切到 **Assets**（不是 File）
2. 上半部分是 **Local components / Local styles** —— 这是**本文件自己的**，
   库里的东西**永远不会**出现在这里（源码 `crates/engine/src/assets/library.rs:180` 显式排除了 remote）
3. 往下滚，每个库一个可折叠小节，标题就是上面表格里的名字
4. 拖组件到画布，或用 `library.place`

## MCP 用

```json
// 挂库（每个新文件都要挂一次）
{"command":"library.list"}
{"command":"library.add","params":{"path":"/Users/laichunping/figcraft-libs/tdesign-web.figcraft"}}

// 放组件（library 可以给 id、名字或路径）
{"command":"library.place","params":{"library":"TDesign Mobile · Button 按钮","component":"Button 按钮","x":100,"y":100}}
{"command":"library.place","params":{"library":"icons-comp","component":"icon/search","x":100,"y":300}}

// 应用库样式 —— 注意是 library.applyStyle，不是 style.apply
{"command":"library.applyStyle","params":{"ids":[<id>],"library":"TDesign Mobile · Styles 样式","style":"Brand 品牌/Brand7-Normal","target":"fill"}}
```

`style.apply {style:"Brand 品牌/Brand7-Normal"}` 会报 `no style "..."` —— 那是只认本文件样式的命令。

## 字体：PingFang SC 已可用

原稿写"PingFang SC 本机扫描不到"，**这个结论是错的**，已更正。真相分两层：

### 第一层：系统自带的 PingFang 用不了

`/System/Library/PrivateFrameworks/FontServices.framework/Resources/Reserved/PingFangUI.ttc`
是 Apple 私有格式：

```
cidg   len=144         ← Apple 私有 CID 字形索引
hvgl   len=58,687,296  ← Apple 私有可变字形表（58 MB）
```

**没有 `glyf` / `CFF ` / `sbdf` 等任何标准轮廓表**，开源渲染器（FigCraft 用的 skrifa）无法渲染。
`fontdb.rs` 的扫描过滤器把这被判为"不是可渲染字体"而丢弃——不是路径问题，加上目录也没用。

### 第二层：refinec/PingFangSC 的 TTF 有元数据错误

[refinec/PingFangSC](https://github.com/refinec/PingFangSC)（MIT，6 个字重 TTF，带标准 `glyf`）
原始文件存在两处缺陷，直接装会导致**部分字重渲染为空白**：

1. **文件名与真实字重错位**

   | 文件名 | 内部真实字重 |
   |---|---|
   | `-Light.ttf` | Regular |
   | `-Regular.ttf` | Medium |
   | `-Medium.ttf` | Bold |
   | `-Semibold.ttf` | Heavy |
   | `-Thin.ttf` | Light |
   | `-Ultralight.ttf` | ExtraLight |

2. **所有文件的 `usWeightClass` 都是 400**（应为 100/200/300/500/700/900），
   导致 FigCraft 按字重匹配时全部落到同一个 face 上。

### 已修复的版本（当前安装的就是）

重写了 `name` 表和 `OS/2.usWeightClass`，文件名/family/style/字重全部一致：

| 文件 | style | usWeightClass |
|---|---|---|
| `PingFangSC-Ultralight.ttf` | Ultralight | 100 |
| `PingFangSC-Thin.ttf` | Thin | 200 |
| `PingFangSC-Light.ttf` | Light | 300 |
| `PingFangSC-Medium.ttf` | Medium | 500 |
| `PingFangSC-Bold.ttf` | Bold | 700 |
| `PingFangSC-Heavy.ttf` | Heavy | 900 |

安装位置 `~/Library/Fonts/`（在 FigCraft 扫描列表内）。

**验收**（导出 PNG 像素级检查，六个字重的墨迹量随粗细单调递增）：

```
Ultralight 1295 -> Thin 1546 -> Light 2552 -> Medium 3729 -> Bold 4456 -> Heavy 5058
```

### 注意

- **改完字体必须重启 FigCraft**。App 进程会缓存字体库，不重启会继续用旧的 face，
  表现为部分字重空白。
- 字重名是规范的 `Light/Medium/Bold/Heavy`，**没有 `Semibold`**（PingFang 原版就没有）
  也没有 `Regular`（用 `Light` 代替，它是真正的 400 字重）。
- 仍缺 `Source Code Pro`（24 层，`tdesign-mobile-styles` 里的等宽代码文本），与 PingFang 无关。

