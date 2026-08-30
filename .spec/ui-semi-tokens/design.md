# 设计结论 · canvas.css Semi 令牌化（阶段 2：系统设计）

## 令牌映射表（现变量 → Semi 语义 → 浅色值 / 深色值）

| 现变量 | Semi 语义 | 浅色 | 深色([theme-mode=dark]) |
|---|---|---|---|
| `--bg` | bg-0（页面底） | `#fff`(bg-1 侧栏面 grey-0 `#F9F9F9`) | `#16161A`（侧栏 `#1E1F24`≈bg-1） |
| `--panel` | bg-1/bg-2（卡面） | `#fff` | `#232429` |
| `--line` | color-border | `rgba(28,31,35,.08)` | `rgba(255,255,255,.08)` |
| `--text` | text-0 | `#1C1F23` | `rgba(249,249,249,1)` |
| `--dim` | text-2 | `rgba(28,31,35,.62)` | `rgba(249,249,249,.62)` |
| `--dim3`(新) | text-3 | `rgba(28,31,35,.35)` | `.35` 同式 |
| `--accent` | primary | `rgb(0,100,250)` blue-5 | `rgb(84,169,255)` dark blue-5 |
| `--accent-h`(新) | primary-hover | `rgb(0,98,214)` blue-6 | `rgb(127,193,255)` |
| `--accent-bg`(新) | primary-light-default | `rgb(234,245,255)` blue-0 | `rgb(5,49,112)` dark blue-0 |
| `--ok` | success | `rgb(59,179,70)` | `rgb(93,194,100)` |
| `--warn` | warning | `rgb(252,136,0)` | `rgb(255,174,67)` |
| `--err` | danger | `rgb(249,57,32)` | `rgb(252,114,90)` |
| `--fill-0/1`(新) | fill | `rgba(28,31,35,.05/.09)` | `rgba(255,255,255,.12/.16)` |
| radius | radius-medium/small | 卡 8→**6px**；控件 6px；小徽标 3px | 同 |

**不动**：`--col/--row/--gap`（D-181 几何契约）、字体栈改为 Semi 官方顺序
（Inter→-apple-system→PingFang SC→Hiragino Sans GB→Microsoft YaHei→Segoe UI，
R3 系统栈回退不加体积）、行高 1.5（≈Semi 1.52）。

## 质感层细节（R2 授权范围内）
- **浅色基调反转**：页面底→侧栏 grey-0→卡白底发丝边（原深底深卡全部翻转）。
- **控件**：input/select/textarea 白底 + 1px border + 6px 圆角 +
  focus 时 `border-color: var(--accent)` + 2px primary 30% 外圈；
  主按钮 primary 底白字 hover=accent-h；次按钮 fill-0 底 text-1 hover=fill-1。
- **表格**：表头 fill-0 底 text-2；行 hover fill-0；row-action 次按钮化
  （删除 hover 危险=err 底保留，语义不变）。
- **Tag/badge**：primary-light-default 底 + primary 字（原版「预览版」徽章同款）。
- **聊天气泡**：user=accent-bg 底 primary 系文字右靠；assistant=fill-0 左靠。
- **侧栏**：active=accent 字 + accent-bg 底 + 右侧 2px primary 条（原版导航同款语言）。
- **状态色**：ok/warn/err 三语义全走令牌（原硬编码 #d9a94b→--warn 等）。
- **focus-hl**：accent 35% 外圈（不变，仅色值令牌化）。

## 实现与回滚
单文件重写 `crates/dsh-cli/assets/canvas/canvas.css`（include_str! 内嵌 →
bin 重建 + serve 重启）；深色 = `[theme-mode="dark"]` 覆盖块（原版 harness
同款机制，非媒体查询——用户可参数控制；**默认=浅色**）。DOM/结构零改动，
`.card/.cap/.ltable/.srow/.chat-*` 选择器全部保留。回滚=revert 单文件。

## 验收执行序
重建 serve → 13 卡 + 五视图逐一截图（浅色默认）→ 深色开关截图 →
审计 T0-T17 回归 → console 零错 → 对比图存 `.spec/ui-semi-tokens/shots/` →
DECISIONS D-224 → 提交。
