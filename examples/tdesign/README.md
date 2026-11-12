# 在 FigCraft 里用 TDesign

> TDesign 是腾讯的开源企业级设计体系（MIT）。
> 官网：<https://tdesign.tencent.com/>
> 官方 Figma：[TDesign for Web](https://www.figma.com/community/file/1053279236128724321/tdesign-for-web)

## ⭐ 已完成：官方 Figma 文件导入成功

**TDesign 官方完整组件库已导入并验证，结论见
[`OFFICIAL-IMPORT-REPORT.md`](OFFICIAL-IMPORT-REPORT.md)。**

| 指标 | 结果 |
|---|---|
| 原始文件 | 323 MB，77 页 |
| 导入耗时 | 4.16 秒 |
| 节点 | 149,611 |
| 组件 | 2,615 个可放置 |
| 样式 | 87 |
| 导入警告 | **0**（无损） |
| 颜色保真 | `#0052D9` / `#D54941` 精确 |

库文件位置：`~/figcraft-libs/tdesign-web.figcraft`（15 MB，不在仓库内）

```json
{"command":"library.add","params":{"path":"~/figcraft-libs/tdesign-web.figcraft"}}
→ 返回 2,615 个组件 + 87 个样式
```

下面是可复现的完整步骤。

## 步骤：导入 TDesign 官方 Figma 文件

**不用手工重建组件。** TDesign 官方在 Figma 社区有完整文件，
FigCraft 支持导入 Figma 数据。详细步骤见 **[`FIGMA-OFFICIAL.md`](FIGMA-OFFICIAL.md)**。

```bash
# 1. 复制社区文件到你自己的 drafts（社区文件只读）
# 2. 拉 REST JSON
curl -H "X-Figma-Token: $FIGMA_TOKEN" \
  "https://api.figma.com/v1/files/$FILE_KEY?geometry=paths" -o tdesign-web.json
# 3. 导入
figcraft-cli convert tdesign-web.json tdesign-web.figcraft
figcraft-cli info tdesign-web.figcraft --json     # 看 missingFonts / notes
```

**实测验证过导入链路**（用 `figma-rest-sample.json` 模拟 REST 输出）：

```
输入: 2 个 COMPONENT，变体名 "Type=Primary, Size=Medium"
      fill #0052D9, cornerRadius 3, fontSize 14, fontFamily "PingFang SC"
输出: ✅ 2 个 COMPONENT 保留
      ✅ 颜色 #0052D9 精确
      ✅ 圆角 3px、字号 14 保留
      ✅ 变体命名保留
      ⚠ missingFonts: PingFang SC (2 层)  ← 已知问题，见下
```

### 为什么这条路更好

FigCraft 的 REST 导入器覆盖（`docs/file-format-parity.md`）：
pages、图层树、几何、paint、stroke、effects、text runs、vectors、
**auto layout**、constraints、grids、**components、component sets、instances（含 overrides）**、
component properties、slots、**styles**、prototyping、**variables with modes**、bindings。

**导入的是真组件和变体，不是一堆矩形。**

---

## 备选：token 重建（拿不到文件时）

如果只有 token 值、没有 Figma 文件，用本目录的脚本快速起步。
见下文 [「token 重建」](#备选token-重建) 一节。

---

## TDesign 资源的真实形态

| 形态 | 能不能直接导入 | 做法 |
|---|---|---|
| **Figma 社区文件** | ✅ 推荐 | REST JSON 或 `.fig`，见 `FIGMA-OFFICIAL.md` |
| 设计 token（颜色/字号/圆角） | ✅ 已提取 | 本目录 `TOKENS.md` |
| Vue/React 组件代码 | ❌ 不适用 | FigCraft 是设计工具 |
| 图标 | ✅ 可提取 | 见下文「图标」 |

**TDesign 官方 README 确认**提供 Figma / Adobe XD / Sketch 设计资源。

---

## 备选：token 重建

> 只在**拿不到 Figma 文件**时用。有文件请走上面的官方导入。

## 已做好的东西

### 1. `TOKENS.md` — 真实 token 值

从 `tdesign-vue-next@1.20.11` 的 `dist/tdesign.css`（518 KB）**提取的真实值**，不是估计：

- 品牌色 10 级：`#F2F3FF` … `#001A57`，主色 **`#0052D9`**
- 成功/警告/错误各 7 级
- 文字色（TDesign 用透明度做层级）
- 字号/行高成对：`body-medium` 14/22、`title-medium` 16/24 …
- 圆角：`radius-default` **3px**
- 尺寸刻度 `--td-size-1..16`（2px…72px）
- 组件高度：按钮默认 **32px**、内边距 **0 15px**

### 2. `setup-tdesign.json` — 仓库原生的生成脚本

**这是 `figcraft-cli run` 的 JSON 命令脚本**，不是 Python。
每次运行只用 Rust 二进制，无外部依赖。

```bash
figcraft-cli run examples/tdesign/setup-tdesign.json \
  --out examples/tdesign/tdesign-kit.figcraft
```

生成：**37 个颜色变量 + 8 个文本样式 + 4 个组件**，已发布为库。

> 80 步全部成功，`missingFonts: []`，实测可重复运行。

### 3. `tdesign-kit.figcraft` — 现成的库文件

不用跑脚本，直接挂载：

```json
{"command":"library.add","params":{"path":"examples/tdesign/tdesign-kit.figcraft"}}
```

含：37 个颜色变量、8 个文本样式、4 个组件（Button/Primary + Outline + Text、Input）。

### 4. 图标

`icons-common.json`（脚本）+ `icons-common.figcraft`（成品），32 个常用图标。
详见下文「图标」一节。

---

## 怎么在项目里用

### 步骤 1：挂载库

```json
{"name":"run_command","arguments":{"command":"library.add",
  "params":{"path":"<绝对路径>/examples/tdesign/tdesign-kit.figcraft"}}}
```

返回库 id 和清单。**agent 拿到清单就知道有哪些组件和样式。**

### 步骤 2：用组件

```json
{"name":"run_command","arguments":{"command":"library.place",
  "params":{"library":"<库id>","component":"Button/Primary","x":100,"y":100}}}
```

放上去是 **INSTANCE**，改文案用 `type_text`，换变体用 `library.swapAt`。

### 步骤 3：用样式（注意命令）

```json
{"name":"run_command","arguments":{"command":"library.applyStyle",
  "params":{"ids":[<文本id>],"style":"TD/Title M","library":"<库id>"}}}
```

> ⚠️ **不是 `style.apply`**。后者只认本文件自己的样式，会报 `no style "TD/Title M"`。

### 步骤 4：绑定颜色变量

```json
{"name":"run_command","arguments":{"command":"library.bindVariable",
  "params":{"ids":[<图层id>],"field":"fill","variable":"brand-color-7","library":"<库id>"}}}
```

---

## 验证过的规格对齐

实测建一个 TDesign 按钮并导出 CSS：

```
输入: height 32, radius 3, fill #0052D9, padding [0,15,0,15]
导出: width: 86px; height: 32px; border-radius: 3px;
      padding: 0px 15px 0px 15px; background: #0052D9;
官方: height 32px; radius 3px; padding-left 15px; bg var(--td-brand-color-7)
```

**完全对上。** 这就是重建 token 的价值：交给开发不用二次翻译。

---

## 中文字体：TDesign 自己也有这个问题

TDesign 官方字体栈是：

```css
--td-font-family: PingFang SC, Microsoft YaHei, Arial Regular;
```

第一款就是 `PingFang SC`。这和上一轮发现的问题**完全一致**，
而且**导入官方 Figma 文件时一定会撞上**。

### 实测：导入后必然报缺字体

```
figcraft-cli info tdesign-web.figcraft --json
→ "missingFonts": [{"family": "PingFang SC", "layers": 2}]
```

### 三层处理，按顺序做

**① 结构不受影响** —— 组件、变体、颜色、圆角、字号全部正确导入，
只有字体名对不上。**不要因为这个就放弃导入。**

**② 渲染回退** —— 屏幕上会用系统字体画出来，中文能看，但不是 PingFang。

**③ 按需改名** —— 如果要求渲染精确，把 `PingFang SC` 批量换成
FigCraft 能扫描到的 `Hiragino Sans GB`：

```bash
# 先用 MCP 找出所有用了 PingFang 的文本层
# 再用 node.set 批量改
```

**但注意**：如果这份稿子的 CSS 要交给前端，**导出时应保留 `PingFang SC`**，
因为那才是 TDesign 的真实约定。改名只用于本地渲染确认。

### 为什么 PingFang 扫不到

macOS 的 PingFang 不走 `/System/Library/Fonts`，而是通过 AssetsV2 按需下发。
FigCraft 的字体扫描器找不到它，这是平台机制问题，不是 bug。
`docs/file-format-parity.md` 里 `.sketch` 一节也记了同一件事：
「macOS's reserved system fonts (PingFang) aren't found」。

详细规则见 [`../../docs/cn-design-rules.md`](../../docs/cn-design-rules.md)。

---

## 图标：实测可提取，已做好 32 个常用图标

TDesign 图标包 `tdesign-icons-vue-next@0.4.12`（MIT）里**没有现成的 `.svg` 文件**，
每个图标是一个 Vue 组件（`esm/components/*.js`，共 7074 个）。

但**SVG path 数据就在 JS 里**，可以提取。实测：

- 能提取出 path 的图标：**2356 个**
- 已提取并验证导入的常用图标：**32 个**（`add` `close` `check` `chevron-*` `search`
  `setting` `user` `home` `delete` `edit` `download` `upload` `refresh` `info-circle`
  `error-circle` `check-circle` `minus` `more` `arrow-*` `calendar` `time` `mail` `link`
  `copy` `star` `heart`）

产物：

| 文件 | 内容 |
|---|---|
| `icons-common.json` | 32 个图标的 `figcraft-cli run` 脚本 |
| `icons-common.figcraft` | 导入好的文件，32 个可编辑矢量层 |

```bash
figcraft-cli run examples/tdesign/icons-common.json --out my-icons.figcraft
```

实测结果：32 个矢量层全部导入成功，渲染出 **3390 个品牌蓝像素**，`missingFonts: []`。

### 提取时的一个坑

**不要给 `node.create` 传 `width`/`height`**：

```
node.create {type:"vector", path:"M2 12h20", width:24, height:24}
→ ❌ `height` can't size a path with no vertical extent
```

`minus`、`arrow-left` 这类纯水平/垂直的图标没有纵向尺寸，写死 `height` 会失败。
**只传 `path`，让 path 自带尺寸**，用 `x`/`y` 定位即可。

### 许可证：能用，但要守仓库规则

TDesign 图标是 **MIT**，符合本仓库的白名单。但 `AGENTS.md` 有硬规则：

> **Every asset file must have a row in `ATTRIBUTION.md`**
> `cargo xtask assets` 会对 `assets/`、`docs/brand/`、`docs/images/` 下未登记的文件报错。

所以：

1. **不要**把图标放进 `assets/`（会触发 `cargo xtask assets` 失败）
2. 本目录 `examples/tdesign/` 不在扫描范围内，可以放
3. 如果要在别处用，在 `ATTRIBUTION.md` 加一行：

   ```
   | examples/tdesign/icons-common.figcraft | TDesign | tdesign-icons-vue-next 0.4.12 | MIT | 图标 path 原样提取 |
   ```

> 另一个考虑：如果你做的产品**不是用 TDesign**，混用它的图标会造成风格不一致。
> 建议只在确实用 TDesign 的项目里使用。

---

## 如果 TDesign 出了 Figma 版本

TDesign 官方有 Figma 资源（在其官网「资源」页）。如果你能拿到 `.fig` 文件：

```bash
figcraft-cli convert TDesign.fig TDesign.figcraft
figcraft-cli info TDesign.figcraft --json    # 看丢了什么
```

`.fig` 导入是 **best-effort**（`docs/gaps.md` 第 1 条，beta blocker），
导入后重点检查 `notes` 和 `missingFonts`。

---

## 复现/更新 token

TDesign 升级后 token 会变，重新提取：

```bash
curl -sSL https://registry.npmjs.org/tdesign-vue-next/-/tdesign-vue-next-<版本>.tgz -o td.tgz
tar xzf td.tgz
grep -oE '\-\-td-brand-color[a-z0-9-]*: *[^;]+;' package/dist/tdesign.css
```

然后把新值改进 `setup-tdesign.json` 重跑。
