# HANDOFF

## 当前在做什么

2026-10-25：完成第四轮打磨并发布 v0.2.0。主窗卡片重构（速度 + 2×2 指标、仅状态卡片压缩为大号状态）、状态 5 分钟时效与仅状态工具活跃任务计数、多工具固定高度滚动提示；紧凑模式改由后端 `ui.json` 持久化并在 setup 中直接按尺寸创建主窗（`create:false` + 初始化脚本），消除启动时先全尺寸再缩小的闪烁，展开窗口改为显式计算居中；采集线程移到窗口创建之后，减少首帧争抢。`cargo test` 91 项通过，`node --check ui/main.js` 通过。实机验证紧凑首帧 132×132、约 3 秒显示、6/18 秒保持紧凑，展开为 640×600 且定位 (704,365)；headless 验证 6/16 工具布局、紧凑 token/状态视图与 340×280 小窗。版本号已从 0.1.0 升至 0.2.0（`Cargo.toml`、`tauri.conf.json`、`Cargo.lock`）。

2026-10-24：完成第三轮适配。工具注册表扩展到 16 个（新增 Trae CN、Qoder CN、DeepSeek Harness），WorkBuddy 实时速度改为读取 `generation` span 的 `toolOutput[].usage.completion_tokens`，Qoder / Qoder CN 接入活跃轮次状态，前端增加活跃优先排序与 132×132 紧凑 token 图标模式。`cargo test` 86 项全部通过，`node --check ui/main.js` 通过；实机已验证 640×600 ↔ 132×132 缩放、紧凑图标拖动/单击展开，以及 WorkBuddy 实时速度与 Qoder CN 活跃状态。待做：长期观察多工具并发时的排序稳定性。

## 已完成什么

- 工具注册表 `src-tauri/src/tools.rs`
  - 统一维护 16 个工具：Claude Code、Codex CLI、ZCode、WorkBuddy、CodeBuddy、Trae、Trae CN、Qoder、Qoder CN、DeepSeek Harness、Grok、OpenClaw、Hermes、Gemini CLI、Kimi Code、ChatGPT。
  - `AdapterKind` 覆盖 13 种适配器；token 能力 `has_token_source()` 与状态能力 `has_status_source()` 分开声明。
  - 已接入 token：ClaudeCode、CodexCli、Zcode、WorkBuddy、CodeBuddy、Grok、Kimi、Qoder、Hermes。
  - 仅状态：Trae、Trae CN、QoderCn；仅运行态：OpenClaw、Gemini CLI、ChatGPT、DeepSeek Harness（`StatusOnly`）。
  - Trae 与 Trae CN、Qoder 与 Qoder CN 的应用包/数据目录完全分开，避免国内版误匹配国际版。
- 安装与运行检测 `src-tauri/src/detection.rs`
  - 安装检测：PATH 可执行文件（Unix 检查可执行位）、home 数据目录、macOS `/Applications` 与 `~/Applications`、Windows `LOCALAPPDATA/Programs`。
  - 运行检测：每 2 秒读取 `ps -axo command=`（Windows 为 `tasklist`），只匹配 argv[0] 或 node/bun/deno/python 的脚本路径，再按路径段与关键字边界匹配。
  - 含 `.app/` 的命令行先用完整行做应用包匹配并跳过后续关键字匹配，解决 `Trae CN.app`、`Qoder CN.app` 路径带空格被截断的问题。
- 采集层 `src-tauri/src/collector/`
  - 统一返回 `CollectBatch { events, statuses }`；单行解析用 `ParseResult { event, status }`，同一行可同时产出 token 与状态。
  - JSONL 文件：`~/.claude/projects/**/*.jsonl`、`~/.codex/sessions/**/*.jsonl`、`~/.zcode/cli/rollout/**/*.jsonl`、`~/.grok/sessions/**/*.jsonl`、`~/.kimi-code/server/events/**/*.jsonl`。
  - `JsonTraceCollector`：WorkBuddy 增量读取 `~/.workbuddy/traces/**/trace_*.jsonl`；CodeBuddy 按「修改时间 + 文件长度」指纹解析 `~/.codebuddy/traces/**/trace_*.json`。
  - `TraceParser`：WorkBuddy 在每次 `generation` span 结束时从 `toolOutput` 提取 `usage.completion_tokens` / `output_tokens`（按响应 id 去重）并立即上报；trace 终态 `modelInfo.totalOutputTokens` 仅作为回退。耗时优先累加同 trace 内 `generation` span，回退 trace 起止时间/`duration`；按 `traceId` 去重 token，按 `spanId` 去重耗时，按 trace 状态产出 Working / Stopped。
  - SQLite：`SqliteCollector` 只读打开。
    - Qoder：`chat_message.token_info` 的 `completion_tokens`（兼容 `output_tokens`），按 `rowid` 水位线增量读取；耗时用同一请求内上一条 assistant 消息 / 请求记录的 `gmt_create` 差值；任务 id 为 `session_id`。
    - Qoder / Qoder CN 活跃轮次：读取 `chat-session-turn-payload-buffer.sqlite` 的 `active_turn_projection_items`；首次观察时过滤已过期轮次，之后持续存在的长任务保持 Working，条目消失后上报 Stopped；该表没有输出 token 字段。
    - Hermes：`sessions` 表 `output_tokens`、`ended_at - started_at`，按会话 id 去重；运行中会话（`ended_at IS NULL`）只产出 Working 状态。
    - Trae / Trae CN：分别扫描各自 `User/globalStorage` 与 `workspaceStorage` 下的 `state.vscdb`，只读 `memento/icube-ai-agent-storage%` 的当前会话状态（`need_confirm`、`in_progress`、`completed` 等）；macOS / Windows / Linux 用户目录均已探测。
  - 回填截止为本地 0 点往前 29 天（含今天共 30 个自然日）；WorkBuddy/CodeBuddy/Qoder/Hermes 使用同一窗口。
  - 只提取时间、token、耗时、状态与会话标识；不打印会话正文，SQLite 查询不选消息内容列。
- 实时指标 `src-tauri/src/live/mod.rs`
  - `UsageEvent`、`StatusEvent`、`ToolSnapshot`、`LiveSnapshot`；乱序插入、未来事件不提前参与、按本地自然日保留 30 天。
  - 单任务口径：窗口 token / 各任务生成耗时之和；并发口径：合并重叠生成区间后的墙钟时长。
  - 状态引擎：Working 5 分钟内有效、NeedsConfirmation 30 分钟内有效；工作中状态每 60 秒由解析器续期；无显式状态的工具按最近 token 事件或活跃任务新鲜度推断。仅状态工具的 Working / NeedsConfirmation 在 5 分钟内计入 `active_tasks`；运行中但没有任何新鲜状态的工具返回 Idle，而不是 Stopped。
  - 快照通过 `snapshot_at_with_running(now_ms, &running_tools)` 合并进程检测结果。
- 桌面壳 `src-tauri/src/desktop.rs`
  - 主窗在 `tauri.conf.json` 中设为 `create:false`，由 setup 读取 `~/.token-tracker/ui.json` 后通过 `WebviewWindowBuilder::from_config` 按目标尺寸直接创建并显示；隐藏窗口上的 `set_size` 会被 macOS 忽略，这是此前启动闪烁的根因。
  - `initialization_script` 把后端 compact 状态注入 `window.__TT_INITIAL_COMPACT__`，前端以它为准，避免 WebView 遗留 localStorage 与后端偏好分叉。
  - 主窗定位不再依赖 `NSWindow.center()`（在本机会按旧尺寸/异常中心计算），改为用主显示器 position/size 显式计算 `LogicalPosition`；展开时按目标 640×600 重新定位，缩小时保持左上角。
  - 采集线程在窗口创建并显示后才启动，避免首次全量扫描与 WebView 启动争抢磁盘/CPU。
  - 后台线程每 1 秒采集、每 2 秒检测；前端每秒轮询 `get_snapshot`。
  - 空闲电源判定把「Working / NeedsConfirmation 状态」也视为忙碌，避免长任务期间误休眠；仍要求至少一个工具进程在运行。
  - `set_compact_mode` 在主窗 640×600 与 132×132 之间切换，并同步保存 `ui.json`。
  - 小窗、关闭、退出、电源设置命令保持不变。
- 前端 `ui/`
  - `describeStatus()` 按后端 `status`、`statusSupported`、活跃任务与最后活跃时间综合展示：生成中 / 等待确认 / 刚刚完成 / 已停止 / 运行中 / 空闲 / 等待活动；仅状态工具的结束事件超过 5 分钟后显示「空闲」而不是「已停止」。
  - `sortByActivity()` 顺序为「生成中 → 等待确认 → 有活跃任务 → 其他」，组内按 `max(lastSeenMs, statusAtMs)` 倒序，正在工作的智能体排最前。
  - `buildCard/updateCard`：token 工具使用大号速度 + 2×2 指标网格 + 单行脚注；仅状态工具使用 `.status-hero` 大号状态和来源说明，不再渲染空的时间/速度统计。卡片高约 140–151px。
  - 工具网格固定高度 345px（外层 360px），4 个工具完整两行，更多工具滚动；滚动提示是网格下方的独立一行，配渐隐遮罩，不覆盖第三行标题。
  - 图表图例最多 6 项，超出显示「还有 N 个」。
  - 紧凑模式：标题栏 token 图标按钮、`body.is-compact`、状态光晕与合计速度徽标；单击 / Enter 展开，拖动调用 `startDragging()`；有 token 工具时显示 `t/s`，仅状态时显示「工作中 / 待确认 / 空闲」并隐藏单位。状态以后端 `ui.json` 为准，同时写入 localStorage 作为浏览器预览回退。
  - 周期切换、单任务/并发切换、30 分钟图表、小窗交互保持不变。
- 测试
  - `cargo test`：91 个测试全部通过，新增 Working 5 分钟时效、仅状态活跃任务计数、运行中无状态返回 Idle、Qoder 首次轮询过滤/长任务保持 Working、Hermes 心跳等用例。
  - `node --check ui/main.js`：语法通过。
  - headless Chrome 精确视口验证：640×600 下 6 工具（grid 345/scroll 473，提示 404–416，第三行标题 390–403 被遮罩淡出）、16 工具（legend 6 项 + 还有 10 个）、132×132 紧凑 token（87.1 t/s）与仅状态（工作中，单位隐藏）、340×280 WorkBuddy 小窗。
  - 实机验证：紧凑启动首帧 132×132，进程启动约 3 秒显示，6/18 秒后仍为 132×132；通过点击紧凑图标展开为 640×600 且位置 (704,365)，再次缩回 132×132，`ui.json` 同步为 true/false。
  - 用真实 WorkBuddy 数据做过校验：最新 trace 含 132 个 span、其中 12 个 generation span，逐个带 usage；Qoder CN 活跃轮次库在采集时观察到 2 个会话、274 条活跃条目持续更新，国际版活跃库为空。

## 指标与状态口径

- 所有速度按「生成耗时加权」，不按墙钟时间稀释空闲。
- 当前速度：最近 60 秒内完成的事件；并发口径在此基础上按合并区间计算。
- 最近一条：最后一条事件的 token / 生成耗时。
- 5 分钟均：最近 300 秒内事件。
- 今日/7 天/30 天：本地自然日窗口，含今天（30 天 = 今天 + 前 29 天）。
- 分钟桶：30 个桶由旧到新，最后一桶为当前分钟，可能不完整。
- 活跃任务：最近 5 分钟内出现事件的 `task_id` 数。
- 周期任务数：对应窗口内出现过的去重 `task_id` 数。
- 输出 token 只取各工具的 output / completion 字段；`totalTokens`、输入 token、缓存 token 一律不计入。
- 卡片仅展示运行中的工具；统计值来自已回填的 30 天日志，因此工具刚打开时可能立即带出历史数据。

## 卡在哪里 / 已知限制

- Trae、Trae CN、Qoder CN 只有状态，没有可靠输出 token 记录；OpenClaw、Gemini CLI、ChatGPT、DeepSeek Harness 只有进程级运行态。
- Qoder 旧版 `local.db` 的 token 记录已停止更新时只能显示状态；Qoder CN 活跃轮次表没有输出 token 字段；`token_info` 结构依赖 Qoder 版本。
- Hermes 会话结束后才写入汇总 token；运行中会话由采集器每 60 秒发送 Working 心跳，结束后转为 Stopped。
- CodeBuddy / WorkBuddy 缺少 `generation` span 时会回退到 trace 整体耗时，长工具等待会拉低速度；WorkBuddy 实时速度在每次 generation span 结束时更新，流式生成过程中不会逐 token 变化。
- 紧凑模式只显示合计速度，不区分子工具；拖动使用 `startDragging()`，单击展开已在实机验证。
- Grok 仅统计 `turn_completed`，取消但无 usage 的轮次不计 token。
- 「软件只有打开后才会开始记录」目前实现为「只有运行中才显示卡片」，历史回填仍读取 30 天日志。
- Codex 缺少逐响应耗时，仍用事件间隔估算，长工具执行会让速度偏低。
- Claude 旧日志没有 `api-request` 时回退到最近 `user` 时间，可能把排队时间计入耗时。
- 文件被截断/轮转后会从头重读；跨重启的全局事件去重尚未实现。
- 运行工具超过 4 个时卡片区域需要滚动（网格 345px、外层 360px，滚动提示占独立一行）。
- 自动休眠/关机只在「仍有运行中的工具、但没有活跃任务或忙碌状态」时计时；所有工具进程退出后不会触发。
- 窗口位置记忆、开机自启、CI、安装包尚未实现。
- 仅在 macOS 完成实机验证；Windows 的检测、数据目录与电源命令已实现但未验证，Linux 仅实现数据目录探测。
- 本机 rustup 镜像缺少 rustfmt/clippy，格式化与 lint 未执行。

## 待确认的产品口径

1. 历史数据是否应出现在「今日/7 天/30 天」和 30 分钟图表中？当前会回填最近 30 天，只控制卡片是否展示。
2. 并发口径是否应包含已结束任务的重叠区间？当前按事件生成区间合并。
3. 空闲计时阈值按「最近一条事件」还是「活跃任务窗口（5 分钟）」？当前使用活跃任务数，并把新鲜的 Working / NeedsConfirmation 状态视作忙碌。
4. 无显式状态且有 token 源的导入工具，运行中 5 分钟无 token、也无活跃任务时显示「运行中」；仅状态工具则显示「空闲」。是否需要用更明确的「无输出」文案区分？

## 文档与仓库

- `README.md`：功能、支持矩阵、工作原理、各数据源 token/耗时口径、状态语义、构建运行、配置路径、电源管理、限制与路线图，已同步 16 工具、WorkBuddy span usage、Qoder 活跃轮次、卡片布局、5 分钟状态时效与紧凑模式启动行为。
- `RELEASE_NOTES.md`：已生成 `v0.2.0 — 2026-10-25` 条目，v0.1.0 历史版本保持不变。
- Git 仓库 `main` 分支，远程 `https://github.com/Danson-dan/token-tracker.git`；本轮改动正在提交并推送。
- Fine-grained PAT 需要 Repository access 选中该仓库，并把 Contents 设为 Read and write；`Public repositories (read-only)` 只能读取，推送会返回 403。

## 下一步计划

1. 长期观察 10 个工具同时运行时的排序稳定性、卡片滚动与紧凑图标置顶表现。
2. 确认 Trae、Trae CN、Qoder CN 的 token 数据源；评估 OpenClaw、Gemini CLI、ChatGPT、DeepSeek Harness 的本地用量记录。
3. 如需要严格执行「打开后才记录」，在检测层记录首次运行时间，并按该时间过滤指标与图表。
4. 增加跨重启事件去重（持久化文件偏移、SQLite 水位线与事件指纹）。
5. 为 Qoder / Qoder CN / Hermes 寻找更细的状态来源，减少对 token 新鲜度的依赖。
6. 在多显示器 / Stage Manager 环境验证显式居中与置顶行为。
7. 补窗口位置记忆、开机自启、CI 与安装包，并更新 RELEASE_NOTES。

## 踩过的坑

- WorkBuddy / CodeBuddy 的 `totalTokens` 是输入 + 输出（含缓存），不能当作输出；必须取 `modelInfo.totalOutputTokens`。
- WorkBuddy 最新 trace 的 `trace_end` 可能没有 `modelInfo` 或 `totalTokens=0`，不能只等终态；generation span 的 `toolOutput` 里已经有逐响应 `usage.completion_tokens`，应即时上报。`toolOutput` 既可能是 JSON 字符串，也可能是对象/数组，反序列化必须用 `serde_json::Value`。
- CodeBuddy 部分 trace 的 `duration` 是异常大值（曾见 143,457,615ms），必须优先用 `generation` span 求和，不能直接采信 trace 总时长。
- Qoder 桌面版数据不在 `~/.qoder`，而在 `Application Support/Qoder/SharedClientCache/cache/db/local.db`；国内版目录为 `QoderCN`。
- Qoder / Qoder CN 新版流式轮次在 `Application Support/{Qoder,QoderCN}/.../chat-session-turn-payload-buffer.sqlite` 的 `active_turn_projection_items`；轮次结束会删行，适合做实时状态，但没有输出 token 字段。
- macOS `ps` 输出中 `.app` 路径含空格；按空白分词会把 `Trae CN.app` 截成 `Trae`，必须先对整行做应用包匹配并跳过关键字回退。
- Qoder 的 token 记录按 `chat_message.rowid` 增量读取最稳；`gmt_create` 只适合做窗口过滤，不适合做去重水位线。
- Qoder 同一请求会写多条 assistant 消息，耗时要用同请求上一条消息的 `gmt_create` 差值，否则首条消息没有起点。
- Grok 的用量只在 `turn_completed` 的 `usage.outputTokens`，耗时 `apiDurationMs` 可能为 0，需要过滤；同一事件会重复出现，必须按 `_meta.eventId` 去重。
- Kimi 日志外层是 `kind=event`，真正类型在 `envelope.type`；输出 token 在 `payload.usage.output`，耗时是 `llmStreamDurationMs`。
- Trae 的 `state.vscdb` 是 VS Code 全局状态库，`ItemTable.value` 里是完整 memento JSON；只取 `list[].isCurrent`、`updatedAt` 和消息 `status`，不要落盘或打印内容。
- 状态事件必须定期续期：Working 有效期 5 分钟，解析器每 60 秒刷新一次；确认状态保留 30 分钟。
- 隐藏的 Tauri 窗口上调用 `set_size` 会被 macOS 忽略，`show()` 后仍是配置尺寸；必须在 setup 里用 `WebviewWindowBuilder::from_config(...).inner_size(...).visible(true)` 一次性按目标尺寸创建主窗（配置 `create:false`）。
- `NSWindow.center()` 在本机会出现按旧尺寸或异常中心计算（640×600 曾被放到 y=205/322）；不要依赖它，改用主显示器 position/size 计算 `LogicalPosition`。
- 后端偏好与 WebView localStorage 是两份状态；启动时用 `initialization_script` 注入 `window.__TT_INITIAL_COMPACT__` 以前者为准，否则 WebView 数据未清理时会把紧凑窗口重新放大。
- 本机未授予终端屏幕录制权限，`screencapture` 只能拍到壁纸与菜单栏，Tauri 窗口内容会被系统隐去；实机验证以 AppleScript/AX 几何、日志和 CGEvent 交互为准，视觉验证用 headless Chrome 精确视口截图。
- `ToolId` 定义在 `tools` 模块时曾以私有方式导入，需要 `use crate::tools::ToolId;`。
- 检测最初扫描整行命令文本，导致 shell 历史/参数里的关键字误判；改为只检查 argv[0] 和解释器脚本路径。
- Codex `session_meta` 记录没有 `type` 字段，`CodexPayload.kind` 必须是 `Option<String>`，否则每条元数据都报解析失败。
- Tauri 的 `data-tauri-drag-region` 裸属性只对「直接点击该元素」生效；卡片自身带 `tabindex` 会被判定为可点击元素，因此卡片点击不会被窗口拖动吞掉。
- 卡片使用 `overflow: hidden` 时，网格自动行高会被压缩导致内容截断；设置 `grid-auto-rows: max-content` 后恢复。
- 判断截图坐标时注意 `screencapture -R` 的区域单位是屏幕点，普通窗口截图未必等于 Retina 像素尺寸。
- 用 System Events 的 AXPress 点击带 `role=button` 的卡片不会触发 DOM click，需要用 CGEvent 发送真实鼠标事件验证。
- WebKit 本地存储是 UTF-16LE，直接 `strings` 读取看不到明文，需要按十六进制或 `sqlite3 + hex()` 检查。
