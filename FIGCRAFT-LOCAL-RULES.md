# 本项目的 FigCraft 使用规则（本地补充）

> 这份文件不是上游 FigCraft 仓库的内容，是本机/本团队的工作约定。
> 由 `CLAUDE.md` 引入，agent 每次会话都会读到。

## 画中文界面时（硬规则）

完整规则见 [`docs/cn-design-rules.md`](docs/cn-design-rules.md)。最关键的四条：

1. **每段含中文的文本都要有中文字体。** 默认字体是 Inter，没有中文字形，
   不指定会导致导出的 CSS 写成 `font-family: "Inter"`，交给开发就是错的。
   macOS 上首选 `Hiragino Sans GB`（实测可用）；`PingFang SC` 在本机扫描不到，别依赖。
2. **优先用库里预置的中文文本样式**（`CN/Display` … `CN/Label`），不要每次手写字号字重。
3. **中文字号偏大、行高偏松**：正文 ≥15px，行高 ≥ 1.6 × 字号。
4. **绝不开 `--no-system-fonts`**，也不设 `FIGCRAFT_NO_SYSTEM_FONTS=1` —— 中文会全变豆腐块。

交付前自检：

```bash
figcraft-cli info <file> --json     # missingFonts 应为 []
```

并在 MCP 里抽查 `inspect.code {id, language:"css"}`，`font-family` 不能是 `Inter`。

## 用组件库时（硬规则）

完整规则见 [`docs/custom-component-library.md`](docs/custom-component-library.md)。

1. **画任何 UI 前先 `library.list`。** 挂了库就用库里的组件（`library.place`），
   **不要自己从零拼按钮/输入框/卡片** —— 这是保证视觉统一和"对得上代码"的关键。
2. **新文件先挂库**：`library.add {path:"<库路径>"}`，拿到库 id 和组件清单再动手。
3. **库组件放上去是实例**：改文案用 `type_text`，改属性用 `set_properties`，
   换变体用 `library.swapAt`，不要手动解构实例。
4. **库里的样式必须用 `library.applyStyle`**，不是 `style.apply`
   （后者只认当前文件自己的样式，会报 `no style "xxx"`）。
5. **发布库的顺序不能反**：`library.publish` 之后**必须** `save_file`，
   否则发布信息不落盘，别的文件 `library.add` 会报 `has no published library`。

## 本机环境备注

- 从源码 `cargo build` 目前不可用：18 个 git 依赖（`storytold/egui`、`storytold/craft-libs` 等）
  在网络下拉取极慢/超时。**用官方 release 二进制**，不要改源码后期待能编译。
- 仓库 `Cargo.toml` 是 0.15.0，官方最新 release 是 0.16.0，注意版本差。
- 一次性建好中文排版系统 + 组件库骨架：

  ```bash
  python3 scripts/setup_cn_craft.py --out ~/figcraft/my-kit.figcraft
  ```
