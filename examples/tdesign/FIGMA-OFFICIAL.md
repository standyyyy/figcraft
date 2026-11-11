# 用 TDesign 官方 Figma 文件（推荐路径）

> 你提供的：[TDesign for Web](https://www.figma.com/community/file/1053279236128724321/tdesign-for-web)
>
> **这是我之前不知道的信息，它改变了方案**：不用手工重建组件了。

## 结论

TDesign 官方在 Figma 社区有完整设计文件（组件、变体、样式全在里面）。
FigCraft 支持导入 Figma 数据，**有两条路**：

| 路径 | 需要什么 | 完整度 |
|---|---|---|
| **A. REST API JSON** | Figma 个人访问令牌 | ⭐⭐⭐ 最完整，推荐 |
| **B. `.fig` 本地文件** | 在 Figma 里「Save local copy」 | ⭐⭐ best-effort |

---

## 路径 A：Figma REST API（推荐）

### 为什么推荐

FigCraft 的 REST 导入器覆盖得很全（见 `docs/file-format-parity.md`）：

> pages, layer tree, geometry, paints, strokes, effects, text runs, vectors,
> **auto layout**, constraints, grids, **components, component sets, instances** with
> overrides, component properties, slots, **styles**, prototyping, **variables with modes**,
> aliases, scopes and bindings

也就是说：**你导入的是真正的组件和变体，不是一堆矩形。**

### 前置：获取 Figma token

1. 打开 Figma → 右上角头像 → **Settings**
2. **Security** 标签 → **Personal access tokens**
3. Generate new token，勾选权限：
   - `File content` 读（必需）
   - `Variables` 读（要导入变量就勾）
   - `Library content` 读（可选）
4. **复制保存**，只显示一次

### 步骤

**1. 先把 TDesign 文件「Duplicate to your drafts」**

社区文件是只读的，必须复制到你自己的 drafts 才有 file key。
在社区页面点 **Open in Figma** → **Duplicate**。

**2. 拿到 file key**

从 URL 里取：

```
https://www.figma.com/design/<FILE_KEY>/TDesign-for-Web
                              ^^^^^^^^^ 
```

社区链接里的 `1053279236128724321` **不是** file key，复制之后才会得到真正的新 key。

**3. 拉取 JSON**

```bash
# 文件内容（geometry=paths 让矢量带轮廓数据）
curl -H "X-Figma-Token: $FIGMA_TOKEN" \
  "https://api.figma.com/v1/files/$FILE_KEY?geometry=paths" \
  -o tdesign-web.json

# 变量（单独端点）
curl -H "X-Figma-Token: $FIGMA_TOKEN" \
  "https://api.figma.com/v1/files/$FILE_KEY/variables/local" \
  -o tdesign-variables.json
```

**4. 导入 FigCraft**

```bash
figcraft-cli convert tdesign-web.json tdesign-web.figcraft
figcraft-cli info tdesign-web.figcraft --json    # 看 notes / missingFonts / warnings
```

或者在 MCP 里：

```json
{"name":"open_file","arguments":{
  "path":"/path/tdesign-web.json",
  "variables":"/path/tdesign-variables.json"}}
```

> `variables` 参数就是给 REST 导入配变量 JSON 的。

### 预期会遇到的

- **字体**：TDesign 用 `PingFang SC`，导入后 `missingFonts` 可能非空。
  这是已知问题（macOS 上 PingFang 走 AssetsV2 下发，扫描不到），
  不影响结构，但渲染会回退。见 [`../../docs/cn-design-rules.md`](../../docs/cn-design-rules.md)。
- **文件可能很大**：TDesign for Web 组件很多，JSON 可能几十 MB。
  FigCraft 有预算保护（按文件大小算解码值上限），超大文件可能被拒或截断。
- **组件属性**：变体属性（如 `Size=large`、`Type=primary`）应该能带过来，
  导入后用 `inspect_document` 确认。

---

## 路径 B：`.fig` 本地文件

如果不想用 token：

1. 在 Figma 里打开复制后的 TDesign 文件
2. **File ▸ Save local copy**（Figma 桌面版或网页版都有）
3. 得到 `.fig` 文件

```bash
figcraft-cli convert tdesign.fig tdesign.figcraft
```

`.fig` 导入是 **best-effort**（`docs/gaps.md` 第 1 条，beta blocker）：

> 支持组件、变体、实例（含 overrides）、样式、变量、原型交互；
> 不支持的层会变成最接近的 frame/text/vector 并给 warning

两者差异：

| | REST JSON | `.fig` |
|---|---|---|
| 格式稳定性 | 官方公开 API，稳定 | 私有格式，靠逆向 |
| 组件/变体 | ✅ | ✅ |
| 变量 | ✅（需单独端点） | ✅ |
| 图像 | 需另外下载 | 在包里 |
| 风险 | token 泄露 | 格式变动 |

---

## 导入之后：变成可复用的库

导入只是第一步。要让 agent 每个项目都能用：

```json
{"command":"library.publish","params":{}}
```
```json
{"command":"save_file","params":{"path":"/path/tdesign-web.figcraft"}}
```

> ⚠️ `publish` 后**必须** `save_file`，否则别的文件 `library.add` 会报
> `has no published library`。这是实测踩过的坑。

然后每个新项目：

```json
{"command":"library.add","params":{"path":"/path/tdesign-web.figcraft"}}
```

---

## 和手工重建的关系

我之前做的 `setup-tdesign.json`（37 变量 + 8 样式 + 4 组件）**现在降级为备选**：

| 场景 | 用哪个 |
|---|---|
| 有 Figma token / 能拿 `.fig` | **官方文件**，完整、保真 |
| 拿不到文件，只要 token 值 | `setup-tdesign.json`，快速起步 |
| 只要颜色变量对齐代码 | `setup-tdesign.json` 的 37 个变量够用 |

两者可以共存：先用 `setup-tdesign.json` 拿到对齐的颜色变量，
再导入官方组件做真实 UI。

---

## 安全提醒

- **token 不要提交进仓库**，用环境变量 `FIGMA_TOKEN`
- token 等同账号权限，泄露后立即在 Figma 设置里 revoke
- `.gitignore` 里加 `*.figma-token`、`tdesign-web.json` 这类大文件

---

## 实测状态

| 项 | 状态 |
|---|---|
| Figma API 可达性 | ✅ `api.figma.com` 返回 400（缺 token），不是 403 |
| Figma 社区页面 | ❌ CloudFront 403（需要浏览器/登录） |
| FigCraft REST 导入器 | ✅ 代码完整，有 34 个单元测试 |
| REST 导入端到端 | 需你的 token 才能实测 |

**我无法独立完成这一步**，因为需要你的 Figma 账号授权。
你拿到 file key 后告诉我，我可以帮你跑导入并分析报告。
