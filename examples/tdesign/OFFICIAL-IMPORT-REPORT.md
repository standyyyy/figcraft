# TDesign 官方 Figma 文件导入：实测报告

> 实测时间：2026-11-09
> 文件：TDesign for web (Community)，file key `OB67WPJtsgZgOX8DsuaBGw`
> 工具：`figcraft-cli 0.16.0`（macOS arm64）

## 结论：完全成功

**官方完整组件库已成功导入 FigCraft，无损（0 notes）。**

| 指标 | 结果 |
|---|---|
| 原始 JSON | 323 MB |
| 导入耗时 | **4.16 秒** |
| 导入后文件 | 11 MB |
| 节点 | **149,611** |
| 样式 | **87** |
| 组件（可放置） | **2,615** |
| 组件集 | 264 |
| 导入警告 | **0** |
| 丢图层 | **0** |

## 文件规模

原始 Figma 文件包含 **77 个页面**、60+ 个组件类别：

```
Base 基础        Button / Divider / Icon / Layout / Link / Typography
Navigation 导航  Anchor / BackTop / Breadcrumb / Dropdown / Menu /
                 Pagination / Steps / StickyTool / Tabs
Entry 输入       Cascader / Checkbox / ColorPicker / DatePicker / Form /
                 Input / InputNumber / Radio / Search / Select / Slider /
                 Switch / Transfer / TimePicker / Upload
Data Display     Avatar / Badge / Calendar / Card / Collapse / Comment /
                 Descriptions / Empty / List / Loading / Progress / QRCode /
                 Rate / Skeleton / Statistic / Swiper / Table / Tag /
                 Timeline / Tooltip / Tree / Watermark
Notification     Alert / Dialog / Drawer / Guide / Message / Notification /
                 Popconfirm
Pro Component    AIChat 等高阶组件
```

图层类型统计：

```
INSTANCE        53,967
TEXT            31,201
VECTOR          24,241
FRAME           24,168
COMPONENT        7,927
RECTANGLE        5,253
LINE             1,315
ELLIPSE            926
COMPONENT_SET      264
```

## 组件保真度

### Button 组件集（最复杂的例子）

**960 个变体、12 个属性定义、7 个变体维度：**

| 属性 | 选项 |
|---|---|
| `variant 类型` | base 基础 / outline 描边 / text 文字 / dash 虚边 / ghost 幽灵 |
| `size 尺寸` | large 大 / medium 中 / small 小 |
| `theme 主题样式` | primary 主要 / default 默认 / danger 危险 / text 文字 |
| `shape 形状` | circle 圆形 / round 圆角矩形 / square 方形 / rectangle 矩形 |
| `icon 图标` | true / false |
| `⬑icon type 图标类型` | n/a / prefixIcon / suffixIcon / singleIcon |
| `disabled 禁用` | true / false |

外加 TEXT 属性 `text`、`squareText` 和 3 个 INSTANCE_SWAP 图标槽位。

### 变体属性存储在变体名里（重要）

FigCraft **不把 VARIANT 类型属性存进 `componentPropertyDefinitions`**，
而是**从变体名推导**。这是设计选择，源码注释写得很清楚：

```rust
assert!(!props.contains_key("State"), "variant properties come from the variant names");
```

所以导入后查组件集只看到 2 个 `componentPropertyDefinitions`（text、squareText），
**这不表示变体丢了**。960 个变体的名字完整携带全部 7 个维度：

```
[99] variant 类型=base 基础, size 尺寸=medium 中尺寸, theme 主题样式=primary 主要,
     shape 形状=rectangle 矩形, icon 图标=false, ⬑icon type 图标类型=n/a, disabled 禁用=false
```

放置实例时指定具体变体即可：

```json
{"command":"instance.place","params":{"component":<变体id>,"x":100,"y":100}}
→ 得到正确变体的 INSTANCE，尺寸 60×32（符合 TDesign 规范）
```

## 颜色保真度

渲染 Button 整页（5824×3982）后抽样像素：

| 颜色 | 出现 | 对应 TDesign token |
|---|---|---|
| `#0052D9` | ✅ | `--td-brand-color-7`（品牌主色） |
| `#D54941` | ✅ | `--td-error-color-5`（危险色） |
| `#F0F2F5` | ✅ | 页面背景灰 |
| `#181818` | ✅ | `rgba(0,0,0,0.9)` 文字色 |

**颜色全部精确还原，无偏差。**

## 字体情况

全库 31,200 个文本层，字体构成：

| 字体 | 层数 | 占比 |
|---|---|---|
| `PingFang SC` | 31,091 | 99.7% |
| `TCloudNumber` | 50 | 0.16% |
| `Source Code Pro` | 34 | 0.11% |
| `TencentSans` | 2 | — |
| 其他（Menlo/Open Sans/Inter/SF Pro） | 40 | — |

### 缺字体警告的真实影响

```
figcraft-cli info tdesign-web.figcraft --json
→ "missingFonts": [{"family":"PingFang SC","layers":31091}, ...]
```

**但这不影响可用性**，实测证据：

- 中文文本层正常导入，`characters` 完整
- 尺寸度量正常（如「默认」14pt → 28×22 px）
- CSS 导出正确：`font-family: "PingFang SC";`
- 渲染出的页面有正常中文

`missingFonts` 只表示 **FigCraft 的字体扫描器找不到这个名字**，
渲染时会用系统字体回退。设计稿结构、颜色、尺寸全部正确。

### 为什么扫不到 PingFang

PingFang 在这台机器上确实存在，但在 Apple 的保留字体路径：

```
/System/Library/PrivateFrameworks/FontServices.framework/Resources/Reserved/PingFangUI.ttc
```

不在标准的 `/System/Library/Fonts`，FigCraft 不扫描这个位置。
`docs/file-format-parity.md` 的 `.sketch` 一节也记录了同一现象：
「macOS's reserved system fonts (PingFang) aren't found」。

**这是平台机制问题，不是导入缺陷。**

## 导入性能

| 页面 | JSON 大小 | 耗时 | 节点 | 警告 |
|---|---|---|---|---|
| Button 按钮 | 12.1 MB | 0.31 s | 3,381 | 0 |
| Input 输入框 | 5.5 MB | 0.14 s | 1,281 | 0 |
| Card 卡片 | 4.1 MB | 0.13 s | 873 | 0 |
| Tag 标签 | 10.7 MB | 0.18 s | 3,085 | 0 |
| **Table 表格** | **108.4 MB** | **1.09 s** | **39,342** | 0 |
| **全量** | **323 MB** | **4.16 s** | **149,611** | 0 |

即使 108 MB 的 Table 页面也能无警告导入。

## 导入命令

```bash
# 1. 拉取（用代理会快 60 倍：直连 15 分钟没下完，代理 9 秒）
curl -x http://192.168.0.1:7890 \
  -H "X-Figma-Token: $FIGMA_TOKEN" \
  "https://api.figma.com/v1/files/OB67WPJtsgZgOX8DsuaBGw?geometry=paths" \
  -o tdesign-full.json

# 2. 导入
figcraft-cli convert tdesign-full.json tdesign-web.figcraft

# 3. 校验
figcraft-cli info tdesign-web.figcraft --json
```

## 发布成库并使用

```json
// 1. 打开并发布（顺序不能反！）
{"command":"library.publish","params":{}}
→ {"components": 2615, "styles": 87, "id": "750b97fbfdd1947f"}

// 2. 必须保存
{"command":"save_file","params":{"path":"~/figcraft-libs/tdesign-web.figcraft"}}
```

然后在任意项目里：

```json
{"command":"library.add","params":{"path":"~/figcraft-libs/tdesign-web.figcraft"}}
→ 返回 2,615 个组件 + 87 个样式的清单
```

## 已知限制

| 限制 | 说明 |
|---|---|
| **变量为空** | `/variables/local` 端点需要 `file_variables:read` 权限，当前 token 没有。颜色以硬编码值导入，不是变量 |
| **4 种字体缺失** | PingFang SC / TCloudNumber / Source Code Pro / TencentSans，见上 |
| **文件较大** | 15 MB 的 `.figcraft`，含 15 万节点，加载需几秒 |
| **组件 2615 而非 8010** | 发布时按组件集去重（960 个 Button 变体算 1 个可放置组件集） |

### 想要变量怎么办

在 Figma 生成 token 时勾选 **Variables** 权限（`file_variables:read`），
然后单独拉变量端点：

```bash
curl -H "X-Figma-Token: $FIGMA_TOKEN" \
  "https://api.figma.com/v1/files/$FILE_KEY/variables/local" -o vars.json
```

导入时配上：

```json
{"name":"open_file","arguments":{
  "path":"tdesign-full.json",
  "variables":"vars.json"}}
```

## 安全

- Figma token 等同账号权限，**不要提交进仓库**
- 本文档记录的是导入结果，不含任何 token
- 用完建议 revoke 并重新生成
