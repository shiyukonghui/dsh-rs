# 设计结论：面板改写 #6 —— panel-settings（宿主小扩展 + 单元只读卡）

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-192**。唯一新点：宿主投影器加只读 arm；
单元侧仍是改写型复制（第六次）。

## 1. 宿主扩展（remote_host.rs）

- `namespace_view`（web.rs）改 `pub(crate)`——**一个视图函数两处用**（杜绝双源漂移）。
- `RemoteHost` 加字段 `settings: Option<Rc<RefCell<dsh_settings::SettingsProvider>>>`；
  `new` 加第 4 参（4 构造点同步：serve 传 `Some(boot.settings.clone())`，测试按需要）。
- `get` 加 arm `"settingsDescribe"`：None → `{ok:false,error:{code:"no-settings",…}}`；
  Some → `{ok:true, value:{writable:true, hasDocument, namespaces:[namespace_view…]}}`
  （与原生 arm 逐字段同形）。

## 2. 单元 `list`

1. `get("settingsDescribe")`；`ok!=true` 透传；
2. rows：`namespaces[]` 逐个：`value` 为对象 → 每顶层键一行 `{ns, field, value}`；
   否则一行 `{ns, field:"—", value}`；
3. 失败透传，不伪造空表。声明：cardId `panel-settings.list`、type `config`、size 4×4、
   columns `[{ns,"命名空间"},{field,"字段"},{value,"值"}]`、rowsPath items、
   emptyText「没有已注册的设置」。

## 3. 测试（红→绿）

- 宿主（remote_host.rs tests mod）：`settings_describe_projection_matches_native_shape`
  （注册一个 namespace → 投影含 ns/applies/revision/value）+
  `settings_describe_without_reference_is_honest`（None → no-settings）。
- m38：契约 4 测 + 拍平 2 测（对象 ns + 非对象 ns + 失败透传）。
- 清单联动：第七卡（type config）。

## 4. 回滚点
remote_host 三处 + 单元目录 + m38 + 清单一行断言 = 回到 `36fa730`。
