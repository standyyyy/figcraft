# 中文设计稿规则（FigCraft 用）

> 给 agent 的硬性规则。用 FigCraft 画**中文**界面时，每条都要遵守。
> 实测版本：`figcraft-cli 0.16.0`（macOS arm64）。

## 为什么需要这份规则

FigCraft 的默认字体是 **Inter**，Inter **没有中文字形**。界面上中文靠系统字体回退能显示，
但 `inspect.code` 导出的 CSS 会写成 `font-family: "Inter"`，**交给前端开发一用就是错**。

实测证据：

```
默认（不设 fontFamily）  → font-family: "Inter";              ← 错
fontFamily:"Hiragino Sans GB" → font-family: "Hiragino Sans GB";  ← 对
```

## 规则 1：每段中文文本都显式指定 fontFamily

`create_node` / `set_properties` 的 `props` 里，**只要 `text` 含中文，就必须带 `fontFamily`**。

macOS 上实测可用（`file.fontReport` 认得到，渲染也正确）：

| 字体 | 状态 | 建议 |
|---|---|---|
| `Hiragino Sans GB` | ✅ 可用，系统自带 | **首选，跨 macOS 版本最稳** |
| `STHeiti` | ✅ 可用，系统自带 | 备选 |
| `PingFang SC` | ⚠️ 本机报 `available: false` | 别依赖（见下文说明） |
| `Noto Sans CJK SC` | ⚠️ 本机报 `available: false` | 需自己安装后才有 |

> `PingFang SC` 在 macOS 上是通过 AssetsV2 按需下载的，不在 `/System/Library/Fonts` 常规目录里，
> 所以扫描不到。名字写进文件不会崩，但换台机器可能就不一致 —— 用 `Hiragino Sans GB` 更保险。

```json
{"name":"create_node","arguments":{
  "type":"text","parent":2,"text":"欢迎回来",
  "props":{"fontSize":32,"fontStyle":"Bold","fontFamily":"Hiragino Sans GB","fill":"#111111"}}}
```

## 规则 2：用文本样式把中文字体钉死（推荐）

比每次手写 `fontFamily` 更可靠的做法，是**先建一套中文文本样式**，之后全部应用样式。

**同文件内的样式用 `style.apply`：**

```
style.create {type:"text", name:"CN/Heading L", from:<一个已设中文字体的文本层>}
style.apply  {ids:[新文本], style:"CN/Heading L"}   → fontFamily 变成 "Hiragino Sans GB" ✅
```

**⚠️ 来自「库」的样式，必须用 `library.applyStyle`，不是 `style.apply`：**

```json
{"command":"library.applyStyle",
 "params":{"ids":[<文本id>],"style":"CN/Display","library":"<库id>"}}
```

实测踩坑：库挂载后 `library.list` 明明列出了 `CN/Display`，
但 `style.apply {style:"CN/Display"}` 报 **`no style "CN/Display"`** —— 因为它只认本文件的样式。
用 `library.applyStyle` 才成功，且完整带过字体、字号、行高、字重。

搭一套最小中文排版系统（尺寸按常见网页规范，可自行调）：

| 样式名 | 字号 | 字重 | 行高 |
|---|---|---|---|
| `CN/Display` | 48 | Bold | 56 |
| `CN/Heading L` | 30 | Bold | 38 |
| `CN/Heading M` | 22 | SemiBold | 30 |
| `CN/Body L` | 18 | Regular | 28 |
| `CN/Body M` | 15 | Regular | 22 |
| `CN/Label` | 13 | Medium | 18 |

## 规则 3：中文字号要偏大，行高要偏松

中文方块字的视觉重量比同字号拉丁文字大，而且没有 x-height 的层次。实测对照：

```
24pt Bold：「标题：我的设计稿」宽 180px / 「Title: My Design」宽 191px
```

同字号下中文占宽略窄但**视觉更挤**。实践建议：

- 中文正文字号 ≥ 15px（英文 14px 的场合，中文用 15–16px）
- 中文行高 ≥ 字号的 1.6 倍（英文常用 1.4–1.5）
- 中文段落的字间距别加，英文才需要 `letterSpacing`

## 规则 4：绝不开 `--no-system-fonts`

```
figcraft-cli ... --no-system-fonts     ← ❌ 中文全部变豆腐块
```

这个 flag 只留内嵌字体，而内嵌字体里**中文字形覆盖不完整**（实测 `embeddedFontFiles: 0`）。
它的用途是"跨机器像素级一致的渲染"，和中文场景直接冲突。

同理，设了环境变量 `FIGCRAFT_NO_SYSTEM_FONTS=1` 也会毁掉中文。

## 规则 5：文字换行交给 auto layout，不要手算宽度

实测中文长段落设 `sizingH: "fill"` 后自动换行正确：

```
「这是一段较长的中文说明文字，用来测试自动换行是否正确工作，以及标点符号避头尾的处理。」
→ width 335, height 38（两行）✅
```

做法：文本层设 `sizingH:"fill"` 或给 `width` + `textAutoResize:"height"`，让它自己长高。
**不要**手动设 `height`，中文换行行数一变就错。

## agent 自检清单

画完中文稿，交付前必查：

1. `inspect.code {id, language:"css"}` 抽查 3 个文本层 → `font-family` 不能是 `Inter`
2. `file.fontReport {}` → `missing` 应为 `0`
3. 导出 PNG 让**人**看一眼（agent 若看不了图，必须明确说明并请人验收）
4. `figcraft-cli info <file> --json` → `missingFonts` 应为空数组

## 附：让中文字体彻底不依赖系统（进阶）

项目源码里有构建期选项 `FIGCRAFT_EMBED_CJK`（见 `crates/text/build.rs`），
配合 `CRAFT_FONTS_DIR` 可以把 CJK 字体**编译进二进制**，这样任何机器渲染都一致。

缺点：需要源码构建，而本仓库的 git 依赖（`storytold/egui` 等 18 个）在国内网络下拉取极慢，
实测 25 分钟无产出。**除非网络条件改善，否则不建议走这条路**，用规则 1、2 的运行时方案即可。
