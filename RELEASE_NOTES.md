# Release Notes

## v0.2.0 — 2026-10-25

多智能体适配、卡片布局与启动体验完善。源码运行版本，尚未打包分发。

### 新增

- 工具注册表扩展到 16 个：新增 Trae CN、Qoder CN、DeepSeek Harness；国内版与国际版应用包独立匹配，互不串扰。
- token 数据源从 3 个扩展到 9 个：新增 WorkBuddy、CodeBuddy、Grok、Kimi Code、Qoder、Hermes。
- Qoder / Qoder CN 接入 `active_turn_projection_items` 活跃轮次状态；Qoder CN 没有输出 token 字段，因此只显示状态。
- Trae / Trae CN 接入会话状态（生成中、等待确认、已完成），覆盖 macOS / Windows / Linux 状态库路径。
- 卡片展示「生成中 / 等待确认 / 已停止 / 运行中 / 空闲」等工作状态；仅状态或无数据源的工具分别标注「仅状态」「采集待适配」。
- 卡片布局重构：token 工具使用大号速度 + 2×2 指标网格；仅状态工具使用大号状态文字与来源说明，不再保留空的 token 统计位。
- 运行中卡片按「生成中 → 等待确认 → 有活跃任务 → 其他」排序，组内按最近状态 / 用量事件时间倒序，正在工作的智能体排最前。
- 状态时效调整为 Working 5 分钟、NeedsConfirmation 30 分钟；仅状态工具在状态新鲜期内计入活跃任务数，运行中但无新鲜状态显示「空闲」。
- 4 个工具完整显示两行，更多工具在 345px 网格内滚动；滚动提示独占一行并配渐隐遮罩，图例最多 6 项并显示剩余数量。
- 紧凑图标模式：标题栏按钮把主窗缩成 132×132 的 token 图标，状态光晕 + 合计速度；有 token 时显示 `t/s`，仅状态时显示「工作中 / 待确认 / 空闲」。单击展开、拖动移动，偏好持久化到 `~/.token-tracker/ui.json`。
- Qoder 读取本地 `chat_message.token_info` 历史，按 `rowid` 增量采集；Hermes 读取 `sessions` 汇总 token；两者均只读访问 SQLite。
- WorkBuddy / CodeBuddy 解析 trace 的 `modelInfo.totalOutputTokens` 与 `generation` span 耗时。
- Grok 解析 `turn_completed` 用量与 `apiDurationMs`；Kimi Code 解析 `turn.step.completed` 与 `llmStreamDurationMs`。
- 空闲电源判定把「生成中 / 等待确认」状态计入忙碌，避免长任务期间误触发休眠或关机。

### 修正

- WorkBuddy 实时速度改为在每次 `generation` span 结束时读取 `toolOutput[].usage.completion_tokens`，不再等待 trace 终态汇总；同一响应按 id 去重。
- 不再把 `totalTokens`（输入 + 输出 + 缓存）当作输出 token。
- CodeBuddy 异常 trace 总时长不再污染速度，优先使用 generation span 求和。
- macOS 带空格的 `.app` 进程路径（如 `Trae CN.app`、`Qoder CN.app`）按完整命令行匹配应用包，避免误判为国际版。
- 工作中状态每 60 秒续期，长任务不会因状态过期被误判为停止；Qoder CN 首次轮询会过滤陈旧活跃条目，但持续存在的长任务保持 Working。
- 紧凑模式启动不再闪烁：主窗改为 setup 中按持久化尺寸一次性创建，前端偏好由后端注入，避免先显示 640×600 再缩小；展开窗口使用显式坐标居中，不再受 `NSWindow.center()` 异常影响。
- 采集线程改为窗口创建并显示后再启动，首次全量扫描不再与 WebView 启动争抢资源。
- 滚动提示不再覆盖第三行卡片标题；仅状态卡片高度压缩到约 140px。

### 质量验证

- `cargo test`：91 个测试全部通过，覆盖 9 个 token 适配器、Trace 解析、SQLite 增量读取、活跃轮次状态、状态时效、检测与电源设置。
- 使用本机真实数据校验 WorkBuddy、Kimi、ZCode、Claude 的 30 天窗口统计结果。
- headless Chrome 精确视口回归：640×600 的 6/16 工具布局、图例上限与滚动提示、132×132 紧凑 token/状态视图、340×280 单工具小窗。
- macOS 实机验证：紧凑启动首帧 132×132、约 3 秒显示且保持稳定；展开为 640×600 并居中 (704,365)，再次缩回 132×132，偏好文件同步更新。

### 已知问题

- Trae、Trae CN、Qoder CN 暂无输出 token 数据源；OpenClaw、Gemini CLI、ChatGPT、DeepSeek Harness 仅有进程级运行状态。
- Qoder 旧版 token 记录停止更新后只能显示状态；Hermes 运行中会话由采集器每 60 秒续期 Working。
- 其余限制与 v0.1.0 相同，详见 `README.md` 与 `HANDOFF.md`。

### 升级说明

- 版本号从 0.1.0 升至 0.2.0；`bundle.active` 仍为 `false`，拉取代码后重新 `cargo build` 即可。
- 紧凑模式偏好保存在 `~/.token-tracker/ui.json`，电源设置与历史数据不受升级影响。

## v0.1.0 — 2026-10-24

首个公开预览版本。Token Tracker 以无边框置顶悬浮窗的形式，集中展示本机正在运行的开发智能体的 token 生成速度、周期用量与任务数。

### 新增功能

**多智能体悬浮窗**

- 启动时自动检测本机安装的 13 个 CLI / Agent 工具。
- 每 2 秒检测运行状态，只给正在运行的工具显示卡片；没有运行中的工具时展示空状态与已安装列表。
- 主窗 640×600，无边框、透明、置顶、跨桌面显示、不占任务栏。

**实时指标**

- 当前速度（tok/s）、最近一条、5 分钟均速。
- 今日 / 7 天 / 30 天均速、输出 token 与去重任务数。
- 卡片头部显示最近 5 分钟的活跃任务数，与周期任务数分开统计。
- 单任务 / 并发两种速度口径，按卡片切换并持久化：
  - 单任务：token 总量 ÷ 各任务生成耗时之和。
  - 并发：token 总量 ÷ 合并重叠生成区间后的墙钟时长。
- 30 分钟堆叠柱状图，按工具着色，动态图例与峰值。

**单工具小窗**

- 点击卡片打开 340×280 的单工具悬浮窗，只显示该工具。
- 重复点击复用已有窗口，不会重复创建。
- 小窗关闭按钮、右键、Esc 只关闭自身；主窗关闭按钮与右键退出应用。

**空闲电源管理**

- 可选「系统休眠」或「自动关机」，默认关闭。
- 空闲阈值支持 1–1440 分钟，设置持久化到 `~/.token-tracker/power.json`。
- 仅当仍有工具运行且所有任务结束后开始计时；同一段空闲期只触发一次。
- macOS 使用固定 `osascript` 命令，Windows 使用 `rundll32` / `shutdown`，不接受任意命令输入。

**数据采集**

- 增量读取 JSONL 日志，缓存半行，文件截断或轮转时复位。
- 按稳定哈希生成 `task_id`，支持并发任务识别与去重统计。
- 回填最近 30 个本地自然日的数据。
- 乱序事件按时间插入，未来事件不提前参与计算。
- 只提取 token、时间、耗时和任务标识，不读取会话正文。

### 支持的智能体

| 工具 | 检测 | 用量采集 |
| --- | --- | --- |
| Claude Code | 支持 | 已支持 |
| Codex CLI | 支持 | 已支持 |
| ZCode | 支持 | 已支持 |
| WorkBuddy | 支持 | 待适配 |
| CodeBuddy | 支持 | 待适配 |
| Trae | 支持 | 待适配 |
| Qoder | 支持 | 待适配 |
| Grok | 支持 | 待适配 |
| OpenClaw | 支持 | 待适配 |
| Hermes | 支持 | 待适配 |
| Gemini CLI | 支持 | 待适配 |
| Kimi Code | 支持 | 待适配 |
| ChatGPT | 支持 | 待适配 |

### 隐私与安全

- 不读取、不保存、不上传会话正文；仅使用 token、时间、耗时与会话标识。
- 不包含任何联网上报或遥测。
- 日志不输出密码、JWT、access token、完整 openid 或手机号。
- 电源操作只执行固定的系统命令，不拼接用户输入。
- 前端 CSP 限制为本地资源与 Tauri IPC。

### 质量验证

- `cargo test`：62 个测试全部通过，覆盖指标聚合、JSONL 增量读取、Claude / Codex / ZCode 解析、安装与运行检测、电源状态机。
- macOS 实机验证：启动检测到 13 个已安装工具；模拟 8 个运行中工具；主窗 4 项统计与 30 分钟图表正常渲染。
- 实机验证 Claude Code 与 ZCode 单工具小窗的创建、复用与关闭。
- 使用真实 `ui/index.html` + Tauri API 桩验证卡片点击、周期切换、速度口径切换、设置读取/保存、非法输入拦截与窗口关闭。

### 已知问题

- 10 个工具尚未确认 token 日志格式，统一显示「采集待适配」。
- 首次打开工具时会回填最近 30 天历史数据；尚未严格实现「首次打开后才开始记录」。
- Codex 缺少逐响应耗时，速度使用事件间隔估算，长任务执行时可能偏低。
- Claude 旧日志缺少 `api-request` 时可能把排队时间计入生成耗时。
- 日志截断或轮转后会从头重读；跨重启事件去重尚未实现。
- 超过 4 个运行中工具时卡片区域需要滚动。
- 所有工具进程退出后不会触发自动休眠 / 关机。
- 窗口位置记忆、开机自启、安装包与 CI 尚未提供。
- 仅在 macOS 完成实机验证；Windows 检测与电源命令尚未在真实设备验证。
- 当前未执行 `rustfmt` / `clippy`（本机工具链缺少对应组件）。

### 安装与升级

本版本为源码运行版本，暂不提供安装包：

```bash
cd src-tauri
cargo run
```

`bundle.active` 为 `false`，不会生成 `.app` / `.dmg` / 安装程序。升级时拉取新代码后重新 `cargo build` 即可；电源设置保存在用户目录，不会被覆盖。

### 下一版本计划

- 优先适配 WorkBuddy、CodeBuddy、Trae、Qoder 等工具的 token 数据源。
- 支持严格「首次打开后才记录」。
- 跨重启事件去重与文件偏移持久化。
- 5 个以上运行工具的紧凑布局。
- 窗口位置记忆、开机自启、打包与 CI。
