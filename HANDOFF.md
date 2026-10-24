# HANDOFF

## 当前在做什么

2026-10-24：多智能体桌面悬浮窗已完成并实机验证（主窗、单工具小窗、电源设置）。目前 Claude Code、Codex CLI、ZCode 有可靠的 token 数据适配器，其余已安装/运行的工具先以“采集待适配”占位，不伪造数据。同日补齐 `README.md`、`RELEASE_NOTES.md`，并将项目初始化为 Git 仓库准备推送到 GitHub。

## 已完成什么

- 工具注册表 `src-tauri/src/tools.rs`
  - 统一维护 13 个工具：Claude Code、Codex CLI、ZCode、WorkBuddy、CodeBuddy、Trae、Qoder、Grok、OpenClaw、Hermes、Gemini CLI、Kimi Code、ChatGPT。
  - 每个工具包含显示名、短名、强调色、进程关键字、命令名、macOS `.app` 名、数据目录和适配器类型。
  - 适配器 `ClaudeCode` / `CodexCli` / `Zcode` 已实现；其余为 `Unsupported`，界面显示“采集待适配”。
- 安装与运行检测 `src-tauri/src/detection.rs`
  - 安装检测：PATH 可执行文件（Unix 检查可执行位）、home 数据目录、macOS `/Applications` 与 `~/Applications`、Windows `LOCALAPPDATA/Programs`。
  - 运行检测：每 2 秒读取 `ps -axo command=`（Windows 为 `tasklist`），只匹配 argv[0] 或 node/bun/deno/python 的脚本路径，再按路径段与关键字边界匹配，避免把 shell 命令文本误判为运行中。
  - 启动时计算一次已安装列表；运行列表按 2 秒轮询变化并写日志。
- 采集层 `src-tauri/src/collector/`
  - 支持 `~/.claude/projects/**/*.jsonl`、`~/.codex/sessions/**/*.jsonl`、`~/.zcode/cli/rollout/**/*.jsonl`。
  - 按字节偏移增量读取，缓存半行，文件截断/轮转时复位。
  - 每个文件按稳定哈希生成 `task_id`，事件写入时由采集层补上；用于区分并发任务。
  - ZCode 解析 `model_io` 记录，按 `requestId` 去重，输出 token 取 `response.usage.outputTokens` 或 Anthropic `usage.output_tokens`，耗时优先 `durationMs`，否则 `completedAt - startedAt`。
  - 回填截止为本地 0 点往前 29 天（含今天共 30 个自然日），启动时跳过更旧文件。
  - 只提取时间、token、耗时和任务标识；不读取、不打印会话正文。
- 实时指标 `src-tauri/src/live/mod.rs`
  - `UsageEvent`、`ToolSnapshot`、`LiveSnapshot`；乱序插入、未来事件不提前参与、按本地自然日保留 30 天。
  - 单任务口径：窗口 token / 各任务生成耗时之和。
  - 并发口径：窗口 token / 合并重叠生成区间后的墙钟时长，避免并发时重复累加耗时。
  - 指标：当前速度、最近一条、5 分钟均、今日/7 天/30 天均与输出、各周期去重任务数（今日/7 天/30 天）、活跃任务数（最近 5 分钟出现过的 `task_id` 数）、30 个分钟桶、30 分钟峰值、最后活跃时间。
- 桌面壳 `src-tauri/src/desktop.rs`
  - 后台线程每 1 秒采集并刷新快照，每 2 秒检测运行工具；前端每秒通过 `get_snapshot` 轮询。
  - 主窗 640×600，无边框、透明、置顶、跨桌面、不占任务栏。
  - `open_tool_window(toolId)` 创建或复用 `mini-<id>` 窗口：340×280、无边框、透明、置顶、居中，页面 URL 为 `index.html?tool=<id>&mini=1`。
  - `close_window` 关闭当前窗口；`quit_app` 退出应用；主窗关闭按钮和右键均退出，小窗关闭按钮、右键、Esc 只关闭自身。
  - 电源设置命令：`get_power_settings`、`update_power_settings`（校验 1–1440 分钟、持久化、重置空闲计时）。
- 空闲电源管理 `src-tauri/src/power.rs`
  - 设置持久化到 `~/.token-tracker/power.json`，默认关闭、15 分钟、休眠。
  - 状态机：没有运行中的工具或存在活跃任务时不计时；连续空闲达到阈值只触发一次，直到重新出现活动才重新武装。
  - macOS 使用 `osascript` 固定脚本休眠/关机；Windows 使用 `rundll32` / `shutdown /s /t 30`；只执行固定命令，不拼接用户输入。
- 前端 `ui/`
  - 只渲染 `runningTools` 中的卡片；小窗只渲染 URL 指定的单个工具。
  - 今日/7 天/30 天切换，标签、均速、输出与去重任务数同步变化；单任务/并发切换按卡片生效并写入 `localStorage`，多窗口通过 `storage` 事件同步。
  - 卡片头部显示最近 5 分钟的活跃任务数，统计区显示当前周期内的去重任务数，避免把“正在生成”和“周期内做过多少任务”混为一谈。
  - 卡片点击打开单工具小窗；小窗内禁用卡片点击。
  - 30 分钟堆叠柱状图只展示受支持且运行中的工具，图例和峰值动态生成。
  - 无运行工具时显示空状态和已安装工具标签（未适配的显式标注）。
  - 设置面板读取/保存电源设置，非法分钟数在前端直接拦截。
- 测试与验证
  - `cargo test`：62 个测试全部通过（引擎、采集、检测、电源、ZCode 解析等）。
  - 实机验证：启动检测到 13 个已安装工具；模拟运行环境出现 8 个运行中工具；点击卡片成功创建 `Claude Code` 小窗（340×280），小窗内容只含该工具，点击 × 可关闭。
  - 前端交互用真实 `ui/index.html` + Tauri API 桩验证：卡片点击参数为 `{toolId}`、周期切换、单任务/并发切换、设置读取与保存 payload `{settings:{enabled,idleMinutes,action}}`、非法输入拦截、主窗 `quit_app`、小窗 `close_window`。

## 指标口径

- 所有速度按“生成耗时加权”，不按墙钟时间稀释空闲。
- 当前速度：最近 60 秒内完成的事件；并发口径在此基础上按合并区间计算。
- 最近一条：最后一条事件的 token / 生成耗时。
- 5 分钟均：最近 300 秒内事件。
- 今日/7 天/30 天：本地自然日窗口，含今天。
- 分钟桶：30 个桶由旧到新，最后一桶为当前分钟，可能不完整。
- 活跃任务：最近 5 分钟内出现事件的 `task_id` 数；并发口径使用同一批事件。
- 周期任务数：今日/7 天/30 天窗口内出现过的去重 `task_id` 数。
- 卡片仅展示运行中的工具；统计值来自已回填的 30 天日志，因此一个工具刚打开时，卡片可能立即带出历史数据（见“待确认的产品口径”）。

## 卡在哪里 / 已知限制

- 其余 10 个工具没有确认过 token 日志结构，统一显示“采集待适配”；需要逐个确认数据源字段后再实现适配器。
- “软件只有打开后才会开始记录”目前实现为“只有运行中才显示卡片”，历史回填仍会读取 30 天日志；若要求严格从首次打开时刻开始统计，需要记录首次发现运行时间并过滤更早事件。
- Codex 缺少逐响应耗时，仍用 `task_started` 与上一条 `token_count` 间隔估算，长工具执行会把分母拉大、速率偏低。
- Claude 旧日志没有 `api-request` 时回退到最近 `user` 时间，可能把排队时间计入耗时。
- 文件被截断/轮转后会从头重读，进程内可能重复送事件；跨重启的全局事件去重尚未实现。
- 运行工具超过 4 个时卡片区域需要滚动（固定 350px 高度，与 30 分钟图表共享窗口高度）。
- 自动休眠/关机只在“仍有运行中的工具、但所有任务都结束”时计时；如果所有工具进程都退出，不会触发关机。
- 窗口位置记忆、开机自启、CI 尚未实现（README 与发布说明已补齐）。
- 本机 rustup 镜像缺少 rustfmt/clippy，格式化与 lint 未执行。

## 待确认的产品口径

1. 历史数据是否应出现在“今日/7 天/30 天”和 30 分钟图表中？当前会回填最近 30 天，只控制卡片是否展示。
2. 并发口径是否应包含已结束任务的重叠区间？当前按事件生成区间合并。
3. 空闲计时阈值按“最近一条事件”还是“活跃任务窗口（5 分钟）”？当前使用活跃任务数。

## 文档与仓库

- `README.md`：功能、支持矩阵、工作原理、指标口径、构建运行、配置路径、电源管理、限制与路线图。
- `RELEASE_NOTES.md`：v0.1.0（2026-10-24）首个公开预览版本的完整变更与已知问题。
- Git 仓库已初始化（`main`，首个提交 `bba75fe`），`.gitignore` 排除 `/src-tauri/target/`、`/src-tauri/gen/` 与 `.DS_Store`；远程已配置为 `https://github.com/Danson-dan/token-tracker.git`。
- 推送阻塞：本机没有可用的 GitHub 凭据（无 credential helper、Keychain 无 github.com 条目、无 `gh`、无 `GITHUB_TOKEN`）；匿名 HTTPS 推送被 GitHub 拒绝，SSH 22 端口可连通但握手超时。需要在本地配置 PAT 或 SSH 后再执行 `git push -u origin main`。

## 下一步计划

1. 网络恢复后完成 GitHub 首次推送，并确认远程默认分支为 `main`。
2. 逐个确认其余工具的数据源结构，优先 WorkBuddy、CodeBuddy、Trae、Qoder。
3. 如需要严格执行“打开后才记录”，在检测层记录首次运行时间，并按该时间过滤指标与图表。
4. 为 5 个以上运行工具提供紧凑卡片布局或分组滚动提示。
5. 增加跨重启事件去重（持久化文件偏移与事件指纹）。
6. 补窗口位置记忆、开机自启与 CI。

## 踩过的坑

- `ToolId` 定义在 `tools` 模块时曾以私有方式导入，需要 `use crate::tools::ToolId;`。
- 检测最初扫描整行命令文本，导致 shell 历史/参数里的关键字误判；改为只检查 argv[0] 和解释器脚本路径。
- Codex `session_meta` 记录没有 `type` 字段，`CodexPayload.kind` 必须是 `Option<String>`，否则每条元数据都报解析失败。
- Tauri 的 `data-tauri-drag-region` 裸属性只对“直接点击该元素”生效；卡片自身带 `tabindex` 会被判定为可点击元素，因此卡片点击不会被窗口拖动吞掉。
- 卡片使用 `overflow: hidden` 时，网格自动行高会被压缩导致内容截断；设置 `grid-auto-rows: max-content` 后恢复。
- 判断截图坐标时注意 `screencapture -R` 的区域单位是屏幕点，普通窗口截图未必等于 Retina 像素尺寸。
- 用 System Events 的 AXPress 点击带 `role=button` 的卡片不会触发 DOM click，需要用 CGEvent 发送真实鼠标事件验证。
- WebKit 本地存储是 UTF-16LE，直接 `strings` 读取看不到明文，需要按十六进制或 `sqlite3 + hex()` 检查。
