# Token Tracker

面向多开发智能体的桌面悬浮窗。自动检测本机安装并在运行的 CLI / Agent 工具，把它们的 token 生成速度、今日 / 7 天 / 30 天用量和任务数集中显示在一个常驻置顶的小窗里。

> **隐私边界**：不会主动读取会话正文，只提取 token、时间、耗时和状态字段。

Token Tracker 只解析本地日志里的用量元数据，不读取、不保存、不上传任何对话内容，也没有任何联网上报。

当前数据源覆盖 JSONL 会话日志、trace JSON / JSONL 和本地 SQLite 状态库，全部只读。

---

## 功能

- **自动检测**：启动时识别已安装的智能体；每 2 秒检测一次运行状态，只给正在运行的工具显示卡片。
- **活跃优先排序**：运行中的卡片按「生成中 → 等待确认 → 其他」分组，组内按最近状态或 token 事件时间倒序，正在干活的智能体始终排最前。
- **实时速度**：卡片展示当前 tok/s、最近一条、5 分钟均速，以及当前周期的均速与输出 token。
- **紧凑卡片**：token 工具用大号速度 + 2×2 指标网格；仅状态工具用大号状态文字，不保留空的 token 统计位。4 个工具完整显示两行，更多工具在固定高度内滚动并提示。
- **状态识别**：卡片区分「生成中」「等待确认」「已停止」「空闲」；工作中的状态 5 分钟内有效，确认状态 30 分钟内有效。仅状态工具在状态新鲜期内也会计入活跃任务数。没有 token 数据源的工具会标注「仅状态」或「采集待适配」，不会伪造数据。
- **多周期统计**：今日 / 7 天 / 30 天，按本地自然日划分。
- **任务数**：卡片头部显示最近 5 分钟的活跃任务数，统计区显示当前周期内产生过用量的去重任务数。
- **双速度口径**：每张卡片右上角可切换「单任务」与「并发」：
  - 单任务：窗口内 token 总量 ÷ 各任务生成耗时之和。
  - 并发：窗口内 token 总量 ÷ 合并重叠生成区间后的实际墙钟时长。
- **单工具小窗**：点击卡片打开该工具的 340×280 悬浮小窗，只显示这一个工具；再次点击会复用已存在的窗口。
- **紧凑图标模式**：标题栏的 token 图标按钮把主窗缩成 132×132 的常驻图标；图标用光晕表示生成中 / 等待确认，并叠加合计速度，单击展开、拖动移动。模式由后端持久化，启动时直接按上次尺寸创建窗口，不会先显示全尺寸再缩小。
- **30 分钟图表**：受支持且正在运行的工具按分钟堆叠展示，支持动态图例与峰值。
- **空闲电源管理**：所有运行中的工具都结束任务并持续空闲指定分钟后，可自动休眠或关机（默认关闭）。
- **无边框置顶**：透明、圆角、跨桌面显示、不占任务栏；主窗关闭或右键退出，小窗关闭 / 右键 / Esc 只关闭自身。

## 支持的智能体

16 个工具均支持安装与运行检测，其中 9 个已接入 token 数据源；Trae、Trae CN、Qoder CN 提供会话状态，OpenClaw / Gemini CLI / ChatGPT / DeepSeek Harness 目前只有进程级运行状态：

| 工具 | 检测 | 用量采集 | 状态采集 | 数据源（只读） |
| --- | --- | --- | --- | --- |
| Claude Code | 支持 | **已支持** | 运行态推断 | `~/.claude/projects/**/*.jsonl` |
| Codex CLI | 支持 | **已支持** | 运行态推断 | `~/.codex/sessions/**/*.jsonl` |
| ZCode | 支持 | **已支持** | 运行态推断 | `~/.zcode/cli/rollout/**/*.jsonl` |
| WorkBuddy | 支持 | **已支持** | 显式状态 | `~/.workbuddy/traces/**/trace_*.jsonl` |
| CodeBuddy | 支持 | **已支持** | 显式状态 | `~/.codebuddy/traces/**/trace_*.json` |
| Trae | 支持 | 仅状态 | 显式状态 | `User/{globalStorage,workspaceStorage}/state.vscdb`（macOS / Windows / Linux） |
| Trae CN | 支持 | 仅状态 | 显式状态 | `User/{globalStorage,workspaceStorage}/state.vscdb`（独立用户目录） |
| Qoder | 支持 | **已支持** | 运行态推断 + 活跃轮次 | `SharedClientCache/cache/db/local.db`、`active_turn_projection_items` |
| Qoder CN | 支持 | 仅状态 | 显式状态 | `chat-session-turn-payload-buffer.sqlite` 的活跃轮次表 |
| Grok | 支持 | **已支持** | 显式状态 | `~/.grok/sessions/**/*.jsonl` |
| OpenClaw | 支持 | — | 仅运行态 | 仅进程检测 |
| Hermes | 支持 | **已支持**（会话结束后汇总） | 显式状态 | `~/.hermes/state.db` 的 `sessions` 表 |
| Gemini CLI | 支持 | — | 仅运行态 | 仅进程检测 |
| Kimi Code | 支持 | **已支持** | 显式状态 | `~/.kimi-code/server/events/**/*.jsonl` |
| ChatGPT | 支持 | — | 仅运行态 | 仅进程检测 |
| DeepSeek Harness | 支持 | — | 仅运行态 | 仅进程检测 |

「显式状态」表示解析器能从本地状态字段读出工作中 / 等待确认 / 已停止；「运行态推断」表示只能根据最近 5 分钟是否有 token 事件或活跃任务来区分生成与空闲；「仅运行态」表示只知道进程在运行。Qoder CN 的活跃轮次表没有输出 token 字段，因此只上报状态，不推断用量。无法确定 token 用量时统一显示「采集待适配」或「仅状态」，不会猜测数据。


## 工作原理

```
JSONL 会话日志 ──增量读取──┐
trace JSON/JSONL ──指纹/增量─┼─▶ 事件（token / 时间 / 耗时 / task_id / 状态）
本地 SQLite（只读）──水位线──┘
                                      │
                                      ▼
                            实时指标引擎（内存聚合 30 天）
                                      │
                                      ▼
                    Tauri 快照（每秒） ──▶ 主窗 / 单工具小窗
```

1. **安装检测**：检查 PATH 可执行文件、用户数据目录、macOS `.app` 包和 Windows `LOCALAPPDATA/Programs`。
2. **运行检测**：每 2 秒读取一次进程列表，只匹配 `argv[0]` 或解释器脚本路径，避免把 shell 参数中的关键字误判为运行中。
3. **增量采集**：JSONL 按字节偏移读取并缓存未写完的半行；trace JSON 按「修改时间 + 文件长度」指纹判断是否重读；SQLite 按 `rowid` 水位线或会话 id 去重；文件截断或轮转时自动复位。
4. **任务识别**：会话文件路径、trace 文件或会话 id 用稳定哈希生成 `task_id`，用来区分并发任务和统计去重任务数。
5. **状态解析**：有显式状态字段的工具会产出「生成中 / 等待确认 / 已停止」事件；长任务期间解析器每 60 秒续期，状态引擎容忍 5 分钟无新事件、确认状态容忍 30 分钟。
6. **实时聚合**：事件按本地自然日保留 30 天，乱序事件会插入正确位置，未来时间戳不会提前参与计算。

## 指标口径

| 指标 | 口径 |
| --- | --- |
| 当前速度 | 最近 60 秒内完成事件的 token ÷ 生成耗时 |
| 最近一条 | 最后一条事件的 token ÷ 该次生成耗时 |
| 5 分钟均 | 最近 300 秒内事件 |
| 今日 / 7 天 / 30 天均 | 对应本地自然日窗口，含今天 |
| 输出 token | 对应周期内 `output_tokens` 之和 |
| 周期任务数 | 对应周期内出现过的去重 `task_id` 数 |
| 活跃任务数 | 最近 5 分钟内出现事件的 `task_id` 数 |
| 30 分钟图表 | 30 个一分钟桶，由旧到新，最后一桶为当前分钟，可能不完整 |
| 并发口径 | 把同一窗口内所有任务的生成区间合并后作为分母，避免并发时重复累加耗时 |

所有速度都按「生成耗时」加权，不把空闲时间稀释进速度。

### 各数据源的 token 与耗时口径

统一只取**输出 token**，不把输入、缓存或 `totalTokens` 当作生成量。

| 工具 | 输出 token 字段 | 生成耗时字段 |
| --- | --- | --- |
| Claude Code | `message.usage.output_tokens` | `api-request` → `assistant`；旧日志回退到上一条 `user` |
| Codex CLI | `payload.info.last_token_usage.output_tokens` | `task_started` 到该次 `token_count` 的间隔（估算） |
| ZCode | `response.usage.outputTokens` / `usage.output_tokens` | `durationMs`，否则 `completedAt - startedAt` |
| WorkBuddy | `generation` span `toolOutput[].usage.completion_tokens`，旧 trace 回退 `modelInfo.totalOutputTokens` | 同一 trace 内 `generation` span 耗时之和 |
| CodeBuddy | `modelInfo.totalOutputTokens` | 同一 trace 内 `generation` span 耗时之和 |
| Grok | `turn_completed.usage.outputTokens` | `apiDurationMs`；按 `_meta.eventId` 去重 |
| Kimi Code | `turn.step.completed.payload.usage.output` | `llmStreamDurationMs` |
| Qoder | `chat_message.token_info.completion_tokens`（兼容 `output_tokens`） | 同一请求内相邻 assistant 消息 / 请求记录的 `gmt_create` 间隔 |
| Hermes | `sessions.output_tokens` | `ended_at - started_at`（仅已结束会话） |

### 工作状态

| 状态 | 含义 |
| --- | --- |
| 生成中 | 解析器读到工作状态且未超过 5 分钟；无显式状态的工具看最近 5 分钟是否有 token 事件或活跃任务 |
| 等待确认 | 解析器读到权限确认 / 计划确认等阻塞状态，30 分钟内保持有效 |
| 已停止 / 刚刚完成 | 解析器读到结束状态；结束事件在 5 分钟内显示「刚刚完成」，更早显示「已停止」 |
| 空闲 | 进程运行但无法判断具体阶段；仅状态工具的结束事件超过 5 分钟后也显示「空闲」 |
| 运行中 | 只有进程级检测、没有状态来源的工具 |

> 「已停止」表示当前没有在生成，不代表进程已经退出；卡片只会在工具进程仍运行时展示。仅状态工具在 Working / NeedsConfirmation 新鲜期内同样计入活跃任务数。

## 快速开始

### 环境要求

- Rust stable（使用 2021 edition）
- macOS 或 Windows
- macOS 需要 Xcode Command Line Tools；Windows 需要 WebView2（Windows 10/11 通常已内置）

### 运行

```bash
cd src-tauri
cargo run
```

首次启动后打开 Claude Code、Codex CLI、ZCode、WorkBuddy、Qoder 等已适配工具并产生一次模型调用，对应卡片会自动出现。没有检测到运行中的工具时会显示空状态和已安装工具列表。

### 测试与构建

```bash
cd src-tauri
cargo test
cargo build --release
```

`cargo test` 覆盖指标引擎、JSONL / trace JSON / SQLite 采集、各适配器解析、状态时效、安装/运行检测和电源设置，共 91 个测试。

> 当前 `bundle.active` 为 `false`，不会生成 `.app` / `.dmg` / 安装包；发布版直接运行 `target/release/token-tracker`。分发打包不在 v0.2.0 范围内。

## 配置与数据

| 内容 | 位置 |
| --- | --- |
| 电源设置 | `~/.token-tracker/power.json` |
| 周期与速度口径偏好 | WebView `localStorage`（`tokenTracker.period` / `tokenTracker.rateMode`） |
| 紧凑图标模式 | `~/.token-tracker/ui.json`（启动时同时注入前端，优先于 WebView `localStorage`） |
| 采集数据 | 只读现有工具的本地日志与 SQLite，不复制、不写入 |
| 运行日志 | 输出到启动它的终端，默认 `info` 级别 |

各工具的数据源均为只读访问：JSONL 与 trace 文件只读取新增字节，SQLite 以只读模式打开并仅查询 token、时间、状态字段。Qoder 的 `token_info`、Hermes 的 `sessions` 汇总和 Trae 的 `state.vscdb` 状态字段都会被严格限制在元数据范围内；Trae 状态库本身包含会话内容，程序只解析状态字段，不提取、不记录正文。

应用自身不创建数据库，也不保存会话内容。清除 `localStorage` 会重置周期与速度口径；紧凑模式以 `ui.json` 为准，删除它会恢复展开界面。删除 `power.json` 会恢复默认电源设置。

## 空闲电源管理

在主窗右上角 ⚙ 打开设置：

- **默认关闭**，只有手动开启后才会执行电源动作。
- 可设置 **1–1440 分钟**的空闲阈值。
- 动作可选 **系统休眠** 或 **自动关机**。

触发条件：

1. 至少有一个工具仍在运行；
2. 所有运行中的工具都没有活跃任务，并持续空闲达到阈值；
3. 同一段空闲期只触发一次，直到重新出现任务活动后才重新计时。

> 如果所有工具进程都退出，应用不会触发休眠或关机。
>
> Windows 关机命令为 `shutdown /s /t 30`，执行后 30 秒内可在终端运行 `shutdown /a` 取消。macOS 通过 `osascript` 调用系统休眠 / 关机。**请先在自己的机器上确认阈值和动作，再长期开启。**

## 项目结构

```
token-tracker/
├── src-tauri/
│   ├── src/
│   │   ├── collector/      # JSONL / trace JSON / SQLite 采集与各工具解析器
│   │   ├── live/           # 实时指标聚合、状态判定与快照
│   │   ├── desktop.rs      # Tauri 命令、采集循环、小窗创建
│   │   ├── detection.rs    # 安装与运行检测
│   │   ├── power.rs        # 空闲状态机与系统电源命令
│   │   ├── tools.rs        # 16 个智能体的注册表与能力声明
│   │   └── main.rs         # 程序入口
│   ├── capabilities/       # 主窗与小窗权限
│   └── tauri.conf.json     # 窗口、透明度、置顶与 CSP
├── ui/                     # 无框架 HTML / CSS / JS 界面
├── HANDOFF.md              # 开发交接与已知问题
├── RELEASE_NOTES.md        # 版本说明
└── README.md
```

## 已知限制

- Trae、Trae CN、Qoder CN 目前只有状态数据，没有可靠的输出 token 记录；OpenClaw、Gemini CLI、ChatGPT、DeepSeek Harness 只有进程级运行状态，尚未确认本地 token 数据源。
- Qoder 旧版 `local.db` 的 token 记录已停止更新时，只能依赖活跃轮次表判断状态；Qoder CN 的活跃轮次表不含输出 token，因此不会显示速度。`token_info` 字段依赖 Qoder 版本，升级后可能需要重新适配。
- Hermes 在会话结束后才写入汇总 token，运行中的会话只能提供状态；采集器每 60 秒为活跃会话续期 Working，结束后转为已停止。
- CodeBuddy / WorkBuddy 的 `generation` span 缺失时会回退到 trace 整体耗时，长工具等待会让速度偏低；WorkBuddy 的实时速度在每次 generation span 结束时更新，流式生成过程中不会逐 token 变化。
- Grok 只统计 `turn_completed`，被取消但未产生 usage 的轮次不会计入 token。
- 「只有打开后才会开始记录」当前实现为「只有运行中才显示卡片」：首次打开时仍会回填最近 30 天的历史日志。
- Codex 缺少逐响应耗时字段，使用 `task_started` 到上一条 `token_count` 的间隔估算，长工具执行会让速度偏低。
- Claude 旧日志缺少 `api-request` 时回退到最近的 `user` 时间，可能把排队时间算进耗时。
- 日志被截断或轮转后会从头重读；跨重启的全局事件去重尚未实现。
- 运行工具超过 4 个时卡片区域需要滚动。
- 紧凑图标模式只展示合计速度，不区分子工具；窗口位置记忆、开机自启、安装包与 CI 尚未实现。
- 仅在 macOS 完成实机验证；Windows 的检测、数据目录与电源命令已实现但未在真实机器上验证，Linux 仅实现了数据目录探测。
- 本机工具链缺少 `rustfmt` / `clippy`，格式化与 lint 未纳入当前流程。

## 路线图

1. 确认 Trae 的 token 数据源，并评估 OpenClaw、Gemini CLI、ChatGPT 的本地用量记录。
2. 支持严格「首次打开后才记录」，按首次发现运行时间过滤历史事件。
3. 增加跨重启事件去重（持久化文件偏移、SQLite 水位线与事件指纹）。
4. 为 Qoder / Hermes 补充更细的工作状态来源，减少对 token 新鲜度的依赖。
5. 为 5 个以上运行工具提供紧凑布局或分组滚动。
6. 补齐窗口位置记忆、开机自启、打包与 CI。

## 参与开发

提交前请至少完成：

```bash
cd src-tauri
cargo test
cargo build
```

UI 改动需要在真实窗口或浏览器桩环境中验证交互，并在 `HANDOFF.md` 中更新当前进度、已知问题与下一步计划。

---

作者：Danson zheng
