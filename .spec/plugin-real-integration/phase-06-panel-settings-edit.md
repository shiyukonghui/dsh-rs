# 阶段 6 · panel-settings-edit（设置编辑）— ✅ 通过（首个动作面阶段）

- 功能清单：表单卡；fieldsFrom `settings/describe`（nsSelect，默认 pick ui-theme）
  + 动作 save → `settings/update`（mode=settings-update，带 expectedRevision 乐观锁）。
- 静态链路：宿主特判臂 `settings.describe/update` 在场（web.rs canonical + 臂表）。
- 浏览器实测（verify-action-form.mjs 模板首跑，**7/7 全绿**，console 零错）：
  1. field-found：select `preference` cur=system，备选 light；
  2. set-value → DOM 值=light；
  3. save1 → 卡内反馈 **「✓ 已保存」**；
  4. **写可见**：重载后「设置概览」卡行 `ui-theme preference light`（浏览器二确）；
  5. save-restore → 回存 system ✓；
  6. **乐观锁浏览器级证明**：不重载立即再存（壳刻意保留 stale revision）→
     `✗ settings namespace "ui-theme" changed since it was read
     (expected revision 3, now 4)（code=SETTINGS_CONFLICT）`；
  7. **复原二确**：重载后概览 `ui-theme preference system`。
- RPC 权威面对账：describe `user.preference="system"` + revision 演进一致。
- 判定：编辑→保存→生效→冲突保护→复原全链浏览器真实发挥作用；
  写侧「真写+回滚」纪律首次落地（R2）。
- 基建沉淀：动作面模板 `verify-action-form.mjs`（设值→点击→act 断言→重载二确→
  冲突探针→复原；--edit-title/--field/--row-ns 参数化，后续表单卡复用）。
