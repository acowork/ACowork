# Mobile App 原型（IM 形态 v1）

`mobile-im-v1.html` — 单文件交互设计稿，无构建依赖，浏览器直接打开。

```bash
open docs/prototypes/mobile-im-v1.html
```

## 验证情况

> ⚠️ 下列数字来自一次性验证运行，其驱动脚本位于 `dev/tmp/` 且**未入库**。
> 当前提交只包含设计稿本身；在此重跑前，这些数字不可复现，也不应被当作
> 持续通过的 CI 门禁引用。

- 语法：9 个 `<script>` 块全部通过解析（提交时已复验：`node --check` 通过）
- 行为：jsdom 驱动 60 项断言（6 项 IA 需求 + 多会话 + 权限矩阵）全通过
- 布局：WKWebView 实测 390×844 下无横向溢出（scrollWidth == clientWidth），
  TabBar 贴底，会话行 66px，无小于 32px 的点击目标
- 视觉：13 张快照见 `_shots/`（已 gitignore，仅本地核对用）

## 与 Desktop 的映射

| Mobile | Desktop 源 |
| --- | --- |
| 底部 Tab Bar | `components/layout/NavBar.tsx`（Harness/Extensions 不上 App） |
| 聊天列表 | `agent-list/AgentList.tsx` + `user-list/UserList.tsx` 合并为 IM 会话流 |
| 聊天详情 | `chat/ChatPanel.tsx` |
| 左滑抽屉 | `right-panel/RightPanel.tsx`（RightNavBar 的 6 个 tab） |
| 项目 | `views/ProjectsView.tsx` + `views/pm/*` |
| 文档 | `views/DocsView.tsx` + `views/doc/*` |
| 设置 | `components/settings/SettingsPage.tsx`（4 个二级页） |
