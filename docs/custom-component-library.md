# 在 FigCraft 里用你自己的组件库

> 让 agent 用你的组件库画稿。实测：`figcraft-cli 0.16.0`（macOS arm64）。

## 先分清三件事

问「我的组件库怎么用到 FigCraft 里」，答案取决于你的库现在是什么**形态**：

| 你的组件库是… | 走哪条路 | 自动化程度 |
|---|---|---|
| Figma 社区库 / `.fig` 文件 | 直接 `file.open` 导入 | ⭐⭐⭐ 全自动 |
| 网页代码（React/Vue/Tailwind 组件） | 转成 FigCraft 组件，见方案 C | ⭐⭐ 半自动 |
| 设计稿截图 / 只有视觉参考 | 见方案 C（要人写规格） | ⭐ 需人参与 |
| `.penpot` / `.sketch` 文件 | 直接 `file.open` 导入 | ⭐⭐⭐ 全自动 |

---

## 方案 A：从 Figma `.fig` 直接导入（最省事）

```sh
figcraft-cli convert my-library.fig my-library.figcraft
```

或 MCP：`open_file {path:"my-library.fig"}`

**注意**：`.fig` 导入是 **best-effort**（见 `docs/gaps.md` 第 1 条，beta blocker），
导入后务必检查：

```sh
figcraft-cli info my-library.figcraft --json
```

看 `notes` / `missingFonts` / `warnings` 字段。丢掉的东西主要可能是：
effects 的部分类型、原型连接、非常规填充。

---

## 方案 B：把你的库变成 FigCraft 本地库（推荐）

这是**最贴合 agent 工作流**的做法。一次设置，之后每个新文件自动可用。

### B.1 准备库文件

库文件就是一个普通 `.figcraft`，里面放着你想复用的东西：

- **组件**（`component.create`）
- **组件集 / 变体**（`component.combineVariants`）
- **颜色、文字、效果样式**（`style.create`）
- **变量**（`variable.createCollection` + `variable.create`）

### B.2 发布

```json
{"name":"run_command","arguments":{"command":"library.publish","params":{}}}
```

返回 `{components, styles, variables, collections, id, version}`，然后**必须保存**：

```json
{"name":"save_file","arguments":{"path":"/path/to/my-kit.figcraft"}}
```

> ⚠️ 实测踩坑：`library.publish` 之后**不 save**，发布信息不落盘，
> 别的文件 `library.add` 会报 `has no published library`。

### B.3 在目标文件里挂载

```json
{"name":"run_command","arguments":{"command":"library.add",
  "params":{"path":"/path/to/my-kit.figcraft"}}}
```

返回库的 `id` 以及里面的 `components` / `styles` / `variables` 清单。
**agent 拿到这个清单就知道你有什么组件了。**

### B.4 使用

```json
{"name":"run_command","arguments":{"command":"library.place",
  "params":{"library":"<库id>","component":"MyButton","x":100,"y":100}}}
```

返回 `{id, component, parent}`。放上去的是 **INSTANCE**（实例），
实测确认可以覆盖文本、改属性，和 Figma 用法一致。

配套命令：

| 命令 | 用途 |
|---|---|
| `library.swapAt` | 把已有实例换成库里的另一个组件 |
| `library.import` | 导入某个库组件到当前文件（变成可编辑的本地组件） |
| `library.applyStyle` | **应用库里的样式（注意不是 `style.apply`）** |
| `library.importVariable` | 导入库变量 |
| `library.bindVariable` | 绑定库变量 |
| `library.updates` / `library.acceptUpdates` | 库更新后同步 |

### ⚠️ 最容易踩的坑：库样式要用 `library.applyStyle`

实测：库挂载后 `library.list` 列出了 `CN/Display`，但

```
style.apply {style:"CN/Display"}                    → ❌ no style "CN/Display"
library.applyStyle {style:"CN/Display", library:id} → ✅ 成功，字体字号行高全部带过来
```

`style.apply` **只认当前文件自己的样式**。库里的样式和组件、变量一样，要走 `library.*` 那套命令。

---

## 方案 C：从代码组件库生成（Tailwind / shadcn / Ant Design 等）

如果你的库是**代码**而不是设计稿，没有自动转换器。可行做法：

1. **agent 读你的代码**（组件 props、variant 定义、token 常量）
2. agent 按这些规格**在 FigCraft 里重建**组件
3. 确认后走方案 B 发布成库

给 agent 的提示词模板：

```
我的组件库是 <路径>。请：
1. 读 src/components/ 下每个组件的 props 和 variant 定义
2. 读 tailwind.config.js（或 tokens 文件）里的颜色/字号/圆角
3. 用 FigCraft 的 component.create + component.combineVariants 逐个重建
4. 变体属性名要和代码里的 props 名一致
5. 全部做完后 library.publish，然后 save_file
最后给我一份清单：建了哪些组件、哪些变体、对应哪个源文件
```

> 这条路的价值在于：**组件属性名和代码对齐**后，
> `inspect.code` 导出的 React 代码能直接对上你的真实组件，交付给开发不用二次翻译。

---

## 方案 D：让 agent 每次现查库内容（不改你的库）

如果库已经在 FigCraft 里挂载，最简单的做法是让 agent **先查后画**：

```
画之前先 library.list 看挂了哪些库，
再 library.add 拿到组件清单，
然后用 library.place 放置，不要自己从零画按钮。
```

这条要写进 CLAUDE.md 或项目规则，否则 agent 默认会自己画矩形拼按钮。

---

## 推荐的 agent 规则（直接抄进项目 CLAUDE.md）

```markdown
## FigCraft 组件使用规则

- 画任何 UI 前，先 `library.list`。有库就用库里的组件（`library.place`），
  不要自己从零拼按钮/输入框/卡片。
- 新文件先 `library.add {path:"<你的库路径>"}` 挂载。
- 库组件放上去是实例，改文本用 `type_text`，改属性用 `set_properties`。
- 需要变体时用 `library.swapAt`，不要手动改。
```

---

## 实测记录（本机，2026-11-09）

| 步骤 | 结果 |
|---|---|
| `component.create` 建 Button 组件 | ✅ |
| `library.publish` | ✅ `{components:1, version:1}` |
| `save_file` 后 `library.add`（新文件） | ✅ 返回组件清单 |
| `library.place` 放置到画布 | ✅ 得到 INSTANCE，`mainComponentName` 正确 |
| 实例内文本可改 | ✅ |

**唯一关键坑**：`library.publish` 后必须 `save_file`，否则库信息丢失。
