# TDesign tokens（提取自 tdesign-vue-next 1.20.11）

> 从 `tdesign-vue-next@1.20.11` 的 `dist/tdesign.css` 提取的真实 CSS 自定义属性。
> 这是 TDesign 官方 token 值，不是估计。
>
> 用途：在 FigCraft 里重建 TDesign 组件时，颜色/字号/圆角/尺寸都能对上代码，
> 这样 `inspect.code` 导出的 CSS 交给开发可以直接用。

## 品牌色（Brand）

TDesign 品牌色是 10 级色阶，`--td-brand-color` 指向第 7 级。

| Token | 值 | 用途 |
|---|---|---|
| `brand-color-1` | `#F2F3FF` | 最浅底 |
| `brand-color-2` | `#D9E1FF` | focus |
| `brand-color-3` | `#B5C7FF` | disabled |
| `brand-color-4` | `#8EABFF` | |
| `brand-color-5` | `#618DFF` | |
| `brand-color-6` | `#366EF4` | hover |
| **`brand-color-7`** | **`#0052D9`** | **主色（默认）** |
| `brand-color-8` | `#003CAB` | active |
| `brand-color-9` | `#002A7C` | |
| `brand-color-10` | `#001A57` | 最深 |

## 语义色

| 类型 | 1（最浅） | 主色参考 |
|---|---|---|
| success | `#E3F9E9` | `#2BA471` |
| warning | `#FFF1E9` | `#E37318` |
| error | `#FFF0ED` | `#D54941` |

> 完整 10 级色阶在 `tdesign.css` 的 `--td-{success,warning,error}-color-N`。

## 文字色

TDesign 用透明度做文字层级（不是固定灰色）：

| Token | 值 |
|---|---|
| `text-color-primary` | `rgba(0,0,0,0.9)` |
| `text-color-secondary` | `rgba(0,0,0,0.6)` |
| `text-color-placeholder` | `rgba(0,0,0,0.4)` |
| `text-color-disabled` | `rgba(0,0,0,0.26)` |
| `text-color-anti` | `#FFFFFF` |

## 字体

**官方字体栈：**

```
--td-font-family: PingFang SC, Microsoft YaHei, Arial Regular;
--td-font-family-medium: PingFang SC, Microsoft YaHei, Arial Medium;
```

> ⚠️ **这就是上一轮中文字体问题的官方印证**：TDesign 第一款字体就是 `PingFang SC`。
> 在 FigCraft 里必须显式设置，否则导出会写成 `Inter`（Inter 无中文字形）。
> 见 [`../docs/cn-design-rules.md`](../docs/cn-design-rules.md)。
>
> macOS 上 `PingFang SC` 实测扫描不到（走 AssetsV2 下发），
> **在 FigCraft 里用 `Hiragino Sans GB` 代替渲染**，但导出的 CSS 保持 `PingFang SC` 原值。

## 字号 / 行高（成对定义）

| Token | 字号 | 行高 |
|---|---|---|
| `body-small` | 12px | 20px |
| `body-medium` | 14px | 22px |
| `body-large` | 16px | 24px |
| `title-small` | 14px | 22px |
| `title-medium` | 16px | 24px |
| `title-large` | 18px | 28px |
| `headline-small` | 24px | 32px |
| `link-small` | 12px | 20px |
| `link-medium` | 14px | 22px |
| `link-large` | 16px | 24px |

## 圆角

| Token | 值 |
|---|---|
| `radius-small` | `2px` |
| **`radius-default`** | **`3px`** |
| `radius-medium` | `6px` |
| `radius-large` | `9px` |
| `radius-round` | `999px` |
| `radius-circle` | `50%` |

## 尺寸刻度（`--td-size-N`）

TDesign 的间距/尺寸基于这套刻度，组件尺寸从它派生：

| N | px | N | px |
|---|---|---|---|
| 1 | 2 | 9 | 28 |
| 2 | 4 | 10 | 32 |
| 3 | 6 | 11 | 36 |
| 4 | 8 | 12 | 40 |
| 5 | 12 | 13 | 48 |
| 6 | 16 | 14 | 56 |
| 7 | 20 | 15 | 64 |
| 8 | 24 | 16 | 72 |

## 组件尺寸（`--td-comp-size-*`）

| Token | 派生自 | 实际值 |
|---|---|---|
| `xxs` | size-7 | 20px |
| `xs` | size-8 | 24px |
| `s` | size-9 | 28px |
| **`m`** | **size-10** | **32px**（按钮默认高度） |
| `l` | size-11 | 36px |
| `xl` | size-12 | 40px |
| `xxl` | size-13 | 48px |

## 组件内边距

| Token | 派生自 | 实际值 |
|---|---|---|
| `paddingLR-s` | size-4 | 8px |
| `paddingLR-m` | size-5 | 12px |
| `paddingLR-l` | size-6 | 16px |
| `paddingTB-m` | size-5 | 12px |
| `paddingTB-l` | size-6 | 16px |

按钮（`base` / `outline` / `dashed`）的实测样式：

```css
height: 32px;                      /* var(--td-comp-size-m) */
border-radius: 3px;                /* var(--td-radius-default) */
font: 14px/22px;                   /* var(--td-font-body-medium) */
padding-left: 15px;                /* calc(paddingLR-l - 1px) */
border-width: 1px;
```

## 怎么用这些数值

1. 在 FigCraft 里建 **颜色变量集合**（`variable.createCollection` + `variable.create`），
   把上面每个 token 建一个变量
2. 建 **文本样式**：`TD/Body M`（14/22）、`TD/Title M`（16/24）…… 字体设 `Hiragino Sans GB`
3. 建 **组件**：按钮按 `height:32px, radius:3px, padding:0 15px` 的实测规格
4. 发布成库（`library.publish` + `save_file`）

生成脚本见 [`setup-tdesign.json`](setup-tdesign.json)，用仓库原生方式运行：

```bash
figcraft-cli run examples/tdesign/setup-tdesign.json --out examples/tdesign/tdesign-kit.figcraft
```

## 来源

- 包：`tdesign-vue-next@1.20.11` → `dist/tdesign.css`（518,253 字节）
- 官网：<https://tdesign.tencent.com/>
- 许可：TDesign 用 MIT；字体 `PingFang SC` 是 Apple 系统字体，**不随项目分发**，
  只在设计稿里作为字体名引用
