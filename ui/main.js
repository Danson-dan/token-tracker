/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: 主窗与小窗前端逻辑。每秒读取一次后端快照，只渲染正在运行的智能体；
 * 支持今日/7 天/30 天切换、单任务与并发速度切换、点击卡片打开单工具小窗，
 * 以及空闲休眠/关机设置。界面只接触 token 与耗时字段，不读取会话正文。
 */

const REFRESH_INTERVAL_MS = 1_000;
const MINUTE_BUCKETS = 30;
const RECENT_WINDOW_MS = 5 * 60_000;
const STORAGE_PERIOD = "tokenTracker.period";
const STORAGE_MODE = "tokenTracker.rateMode";

const PERIODS = ["today", "7d", "30d"];
const STAT_LABELS = { today: "今日均", "7d": "7 日均", "30d": "30 日均" };
const OUTPUT_LABELS = { today: "今日输出", "7d": "7 天输出", "30d": "30 天输出" };
const TASK_LABELS = { today: "今日任务", "7d": "7 天任务", "30d": "30 天任务" };

const params = new URLSearchParams(window.location.search);
const miniToolId = params.get("mini") === "1" ? params.get("tool") : null;
const isMini = Boolean(miniToolId);

const widget = document.getElementById("widget");
const titleText = document.getElementById("titleText");
const runningCount = document.getElementById("runningCount");
const periodTabs = document.getElementById("periodTabs");
const toolGrid = document.getElementById("toolGrid");
const emptyState = document.getElementById("emptyState");
const emptyTitle = document.getElementById("emptyTitle");
const emptyHint = document.getElementById("emptyHint");
const installedTools = document.getElementById("installedTools");
const chartPanel = document.getElementById("chartPanel");
const chart = document.getElementById("chart");
const chartEmpty = document.getElementById("chartEmpty");
const chartLegend = document.getElementById("chartLegend");
const peakRate = document.getElementById("peakRate");
const updatedAt = document.getElementById("updatedAt");
const closeButton = document.getElementById("closeButton");
const settingsButton = document.getElementById("settingsButton");
const settingsPanel = document.getElementById("settingsPanel");
const settingsCloseButton = document.getElementById("settingsCloseButton");
const powerEnabled = document.getElementById("powerEnabled");
const powerMinutes = document.getElementById("powerMinutes");
const powerAction = document.getElementById("powerAction");
const settingsStatus = document.getElementById("settingsStatus");
const saveSettings = document.getElementById("saveSettings");

const tauriInvoke = window.__TAURI__?.core?.invoke ?? null;
const cards = new Map();

let snapshot = null;
let refreshing = false;
let period = readStored(STORAGE_PERIOD, PERIODS, "today");
let mode = readStored(STORAGE_MODE, ["single", "concurrent"], "single");

if (isMini) {
  document.body.classList.add("is-mini");
}

function readStored(key, allowed, fallback) {
  try {
    const value = window.localStorage.getItem(key);
    return allowed.includes(value) ? value : fallback;
  } catch {
    return fallback;
  }
}

function store(key, value) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // 本地存储不可用时仅保留当前会话内的选择
  }
}

function formatRate(value) {
  const rate = Number.isFinite(value) ? Math.max(0, value) : 0;
  if (rate >= 100) {
    return String(Math.round(rate));
  }
  return rate.toFixed(1);
}

function formatTokens(value) {
  const tokens = Number.isFinite(value) ? Math.max(0, value) : 0;
  if (tokens >= 1_000_000_000) {
    return `${trimDecimal(tokens / 1_000_000_000)}B`;
  }
  if (tokens >= 1_000_000) {
    return `${trimDecimal(tokens / 1_000_000)}M`;
  }
  if (tokens >= 10_000) {
    return `${trimDecimal(tokens / 1_000)}K`;
  }
  return Math.round(tokens).toLocaleString("en-US");
}

function trimDecimal(value) {
  return value >= 100 ? String(Math.round(value)) : value.toFixed(1).replace(/\.0$/, "");
}

function formatLastSeen(lastSeenMs, asOfMs) {
  if (!Number.isFinite(lastSeenMs)) {
    return { text: "暂无记录", ageMs: null };
  }
  const ageMs = Math.max(0, asOfMs - lastSeenMs);
  const seconds = Math.floor(ageMs / 1_000);
  if (seconds < 1) {
    return { text: "刚刚", ageMs };
  }
  if (seconds < 60) {
    return { text: `${seconds}s 前`, ageMs };
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return { text: `${minutes}m 前`, ageMs };
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return { text: `${hours}h 前`, ageMs };
  }
  return { text: `${Math.floor(hours / 24)}d 前`, ageMs };
}

function describeStatus(tool, ageMs) {
  if (!tool.supported) {
    return { text: "采集待适配", active: false };
  }
  if (tool.activeTasks > 0) {
    return { text: "生成中", active: true };
  }
  if (ageMs !== null && ageMs <= RECENT_WINDOW_MS) {
    return { text: "刚刚活跃", active: false };
  }
  return { text: ageMs === null ? "等待活动" : "空闲", active: false };
}

function periodMetrics(tool) {
  if (period === "7d") {
    return {
      avg: mode === "concurrent" ? tool.avg7dConcurrent : tool.avg7d,
      output: tool.output7d,
      tasks: tool.tasks7d,
    };
  }
  if (period === "30d") {
    return {
      avg: mode === "concurrent" ? tool.avg30dConcurrent : tool.avg30d,
      output: tool.output30d,
      tasks: tool.tasks30d,
    };
  }
  return {
    avg: mode === "concurrent" ? tool.avgTodayConcurrent : tool.avgToday,
    output: tool.outputToday,
    tasks: tool.tasksToday,
  };
}

function withAlpha(hex, alpha) {
  const clean = String(hex || "").replace("#", "");
  if (!/^[0-9a-fA-F]{6}$/.test(clean)) {
    return `rgba(148, 163, 184, ${alpha})`;
  }
  const red = Number.parseInt(clean.slice(0, 2), 16);
  const green = Number.parseInt(clean.slice(2, 4), 16);
  const blue = Number.parseInt(clean.slice(4, 6), 16);
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`;
}

function formatClock(timestampMs) {
  const timestamp = Number.isFinite(timestampMs) ? timestampMs : Date.now();
  return new Date(timestamp).toLocaleTimeString("zh-CN", { hour12: false });
}

function field(card, name) {
  return card.querySelector(`[data-field="${name}"]`);
}

function buildCard(tool) {
  const card = document.createElement("article");
  card.className = "tool-card";
  card.dataset.tool = tool.tool;
  card.style.setProperty("--accent", tool.accent || "#94a3b8");
  card.tabIndex = 0;
  card.setAttribute("role", "button");
  card.setAttribute("aria-label", `${tool.displayName} 详情`);
  if (!isMini) {
    card.title = "点击打开单工具小窗";
  }
  card.innerHTML = `
    <div class="tool-head">
      <div class="tool-identity">
        <span class="tool-name">
          <span class="tool-dot" aria-hidden="true"></span>
          <span class="tool-label"></span>
        </span>
        <span class="tool-sub">
          <span data-field="status">等待活动</span>
          <span class="dot-sep" aria-hidden="true">·</span>
          <span data-field="tasks">0 个任务</span>
        </span>
      </div>
      <div class="tool-mode" role="group" aria-label="速度口径">
        <button type="button" class="mode-option" data-mode="single">单任务</button>
        <button type="button" class="mode-option" data-mode="concurrent">并发</button>
      </div>
    </div>
    <div class="tool-hero">
      <span class="tool-current" data-field="current">0.0</span>
      <span class="tool-unit">tok/s</span>
    </div>
    <div class="tool-caption">
      <span data-field="caption">当前速度</span>
      <span class="unsupported-badge" data-field="unsupported" hidden>采集待适配</span>
    </div>
    <div class="tool-stats">
      <div class="stat">
        <span class="stat-label">最近一条</span>
        <span class="stat-value" data-field="latest">0.0</span>
      </div>
      <div class="stat">
        <span class="stat-label">5 分钟均</span>
        <span class="stat-value" data-field="avg5m">0.0</span>
      </div>
      <div class="stat">
        <span class="stat-label" data-field="periodLabel">今日均</span>
        <span class="stat-value" data-field="periodAvg">0.0</span>
      </div>
      <div class="stat">
        <span class="stat-label" data-field="tasksPeriodLabel">今日任务</span>
        <span class="stat-value" data-field="tasksPeriod">0</span>
      </div>
    </div>
    <div class="tool-foot">
      <div class="foot-item">
        <span class="foot-label" data-field="outputLabel">今日输出</span>
        <span class="foot-value" data-field="output">0</span>
      </div>
      <div class="foot-item foot-item-end">
        <span class="foot-label">最后活跃</span>
        <span class="foot-value muted" data-field="lastSeen">暂无记录</span>
      </div>
    </div>
  `;

  card.querySelectorAll(".mode-option").forEach((button) => {
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      setMode(button.dataset.mode);
    });
  });

  card.addEventListener("click", () => {
    if (!isMini) {
      void openToolWindow(tool.tool);
    }
  });
  card.addEventListener("keydown", (event) => {
    if (isMini) {
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      void openToolWindow(tool.tool);
    }
  });

  return card;
}

function updateCard(card, tool, asOfMs) {
  card.querySelector(".tool-label").textContent = tool.displayName;
  field(card, "tasks").textContent = `${tool.activeTasks} 个活跃任务`;

  const lastSeen = formatLastSeen(tool.lastSeenMs, asOfMs);
  const status = describeStatus(tool, lastSeen.ageMs);
  field(card, "status").textContent = status.text;
  card.classList.toggle("is-active", status.active);

  const lastSeenNode = field(card, "lastSeen");
  lastSeenNode.textContent = lastSeen.text;
  lastSeenNode.classList.toggle("muted", lastSeen.ageMs === null);

  const metrics = periodMetrics(tool);
  field(card, "periodLabel").textContent = STAT_LABELS[period];
  field(card, "outputLabel").textContent = OUTPUT_LABELS[period];
  field(card, "tasksPeriodLabel").textContent = TASK_LABELS[period];
  field(card, "tasksPeriod").textContent = String(metrics.tasks ?? 0);

  card.querySelectorAll(".mode-option").forEach((button) => {
    button.classList.toggle("is-on", button.dataset.mode === mode);
  });

  if (!tool.supported) {
    field(card, "current").textContent = "—";
    field(card, "caption").textContent = "暂无可靠 token 数据源";
    field(card, "unsupported").hidden = false;
    field(card, "latest").textContent = "—";
    field(card, "avg5m").textContent = "—";
    field(card, "periodAvg").textContent = "—";
    field(card, "tasksPeriod").textContent = "—";
    field(card, "output").textContent = "—";
    return;
  }

  field(card, "unsupported").hidden = true;
  field(card, "current").textContent = formatRate(
    mode === "concurrent" ? tool.currentConcurrentRate : tool.currentRate,
  );
  field(card, "caption").textContent =
    mode === "concurrent" ? "当前速度 · 全部任务并发" : "当前速度 · 单任务平均";
  field(card, "latest").textContent = formatRate(tool.latestRate);
  field(card, "avg5m").textContent = formatRate(
    mode === "concurrent" ? tool.avg5mConcurrent : tool.avg5m,
  );
  field(card, "periodAvg").textContent = formatRate(metrics.avg);
  field(card, "output").textContent = formatTokens(metrics.output);
}

function syncCards(tools) {
  toolGrid.querySelectorAll(".mini-missing").forEach((node) => node.remove());
  const ids = new Set(tools.map((tool) => tool.tool));
  for (const [id, card] of cards) {
    if (!ids.has(id)) {
      card.remove();
      cards.delete(id);
    }
  }

  tools.forEach((tool, index) => {
    let card = cards.get(tool.tool);
    if (!card) {
      card = buildCard(tool);
      cards.set(tool.tool, card);
    }
    const current = toolGrid.children[index];
    if (current !== card) {
      toolGrid.insertBefore(card, current || null);
    }
  });
}

function renderEmptyState() {
  const installed = Array.isArray(snapshot?.installedTools) ? snapshot.installedTools : [];
  const byTool = new Map((snapshot?.tools || []).map((tool) => [tool.tool, tool]));

  emptyTitle.textContent = "当前没有运行中的智能体";
  emptyHint.textContent =
    "打开 Claude Code、Codex CLI、ZCode 等工具后，卡片会自动出现；仅运行中的工具会被统计。";
  installedTools.replaceChildren();

  const detected = installed.map((id) => byTool.get(id)).filter(Boolean);
  if (detected.length === 0) {
    const hint = document.createElement("span");
    hint.className = "installed-chip";
    hint.textContent = "未检测到已安装的智能体";
    installedTools.appendChild(hint);
    return;
  }

  const label = document.createElement("span");
  label.className = "installed-chip";
  label.textContent = "已安装";
  installedTools.appendChild(label);

  for (const tool of detected) {
    const chip = document.createElement("span");
    chip.className = "installed-chip";
    chip.textContent = tool.supported ? tool.displayName : `${tool.displayName}（待适配）`;
    if (!tool.supported) {
      chip.classList.add("is-unsupported");
    }
    installedTools.appendChild(chip);
  }
}

function renderChart(tools) {
  chart.querySelectorAll(".chart-column").forEach((column) => column.remove());
  chartLegend.replaceChildren();

  const series = tools
    .filter((tool) => tool.supported)
    .map((tool) => ({
      tool,
      rates: Array.from({ length: MINUTE_BUCKETS }, (_, index) => {
        const value = Array.isArray(tool.minuteRates) ? tool.minuteRates[index] : 0;
        return Number.isFinite(value) ? Math.max(0, value) : 0;
      }),
    }));

  const columns = Array.from({ length: MINUTE_BUCKETS }, (_, minute) =>
    series.reduce((total, item) => total + item.rates[minute], 0),
  );
  const peak = columns.reduce((max, value) => Math.max(max, value), 0);
  const scale = Math.max(peak, 1);

  for (let minute = 0; minute < MINUTE_BUCKETS; minute += 1) {
    const column = document.createElement("div");
    column.className = "chart-column";
    if (columns[minute] <= 0) {
      column.classList.add("is-zero");
    } else {
      for (const item of series) {
        const rate = item.rates[minute];
        if (rate <= 0) {
          continue;
        }
        const segment = document.createElement("div");
        segment.className = "chart-segment";
        segment.style.height = `${Math.min((rate / scale) * 100, 100)}%`;
        segment.style.background = `linear-gradient(180deg, ${item.tool.accent}, ${withAlpha(
          item.tool.accent,
          0.45,
        )})`;
        segment.title = `${item.tool.displayName} ${formatRate(rate)} tok/s`;
        column.appendChild(segment);
      }
    }
    chart.insertBefore(column, chartEmpty);
  }

  for (const item of series) {
    const legend = document.createElement("span");
    legend.className = "legend-item";
    const dot = document.createElement("span");
    dot.className = "legend-dot";
    dot.style.background = item.tool.accent;
    const text = document.createElement("span");
    text.textContent = item.tool.shortName;
    legend.append(dot, text);
    chartLegend.appendChild(legend);
  }

  chartEmpty.hidden = peak > 0;
  peakRate.textContent = formatRate(peak);
}

function render() {
  if (!snapshot || !Array.isArray(snapshot.tools)) {
    return;
  }

  const asOfMs = Number.isFinite(snapshot.asOfMs) ? snapshot.asOfMs : Date.now();
  const byTool = new Map(snapshot.tools.map((tool) => [tool.tool, tool]));
  const running = (snapshot.runningTools || []).map((id) => byTool.get(id)).filter(Boolean);
  const displayTools = isMini
    ? [byTool.get(miniToolId)].filter(Boolean)
    : running;

  if (isMini && displayTools.length === 0) {
    cards.forEach((card) => card.remove());
    cards.clear();
    const message = document.createElement("div");
    message.className = "mini-missing";
    message.textContent = "未找到该智能体";
    toolGrid.replaceChildren(message);
    updatedAt.textContent = "等待数据";
    return;
  }

  if (isMini && displayTools.length > 0) {
    const tool = displayTools[0];
    titleText.textContent = tool.displayName;
    document.title = `${tool.displayName} · 模型生成速度`;
  }

  syncCards(displayTools);
  for (const tool of displayTools) {
    updateCard(cards.get(tool.tool), tool, asOfMs);
  }

  const hasRunning = displayTools.length > 0;
  emptyState.hidden = hasRunning;
  chartPanel.hidden = !hasRunning || isMini;
  if (!isMini) {
    toolGrid.hidden = !hasRunning;
    renderEmptyState();

    const activeTasks = running.reduce((total, tool) => total + tool.activeTasks, 0);
    runningCount.textContent =
      running.length === 0
        ? "0 个运行中"
        : activeTasks > 0
          ? `${running.length} 个运行中 · ${activeTasks} 个任务`
          : `${running.length} 个运行中`;
    if (hasRunning) {
      renderChart(running);
    }
  }

  widget.classList.remove("is-offline");
  updatedAt.textContent = `数据更新 ${formatClock(asOfMs)}`;
}

function markOffline(message) {
  widget.classList.add("is-offline");
  updatedAt.textContent = message;
}

async function refresh() {
  if (!tauriInvoke) {
    markOffline("未连接桌面运行时");
    return;
  }
  if (refreshing) {
    return;
  }

  refreshing = true;
  try {
    snapshot = await tauriInvoke("get_snapshot");
    render();
  } catch (error) {
    console.error("【界面】获取实时快照失败", error);
    markOffline("采集连接中断");
  } finally {
    refreshing = false;
  }
}

async function openToolWindow(toolId) {
  if (!tauriInvoke) {
    return;
  }
  try {
    await tauriInvoke("open_tool_window", { toolId });
  } catch (error) {
    console.error("【界面】打开单工具小窗失败", error);
  }
}

async function closeSelf() {
  if (!tauriInvoke) {
    window.close();
    return;
  }
  try {
    await tauriInvoke("close_window");
  } catch (error) {
    console.error("【界面】关闭窗口失败", error);
    window.close();
  }
}

async function quitApp() {
  if (!tauriInvoke) {
    window.close();
    return;
  }
  try {
    await tauriInvoke("quit_app");
  } catch (error) {
    console.error("【界面】退出应用失败", error);
  }
}

function setPeriod(nextPeriod) {
  if (!PERIODS.includes(nextPeriod) || nextPeriod === period) {
    return;
  }
  period = nextPeriod;
  store(STORAGE_PERIOD, period);
  updatePeriodTabs();
  render();
}

function updatePeriodTabs() {
  periodTabs.querySelectorAll(".period-tab").forEach((button) => {
    button.classList.toggle("is-active", button.dataset.period === period);
  });
}

function setMode(nextMode) {
  if (!["single", "concurrent"].includes(nextMode) || nextMode === mode) {
    return;
  }
  mode = nextMode;
  store(STORAGE_MODE, mode);
  render();
}

function setSettingsStatus(message, state) {
  settingsStatus.textContent = message;
  settingsStatus.classList.toggle("is-error", state === "error");
  settingsStatus.classList.toggle("is-ok", state === "ok");
}

async function loadPowerSettings() {
  if (!tauriInvoke) {
    setSettingsStatus("未连接桌面运行时", "error");
    return;
  }
  try {
    const settings = await tauriInvoke("get_power_settings");
    powerEnabled.checked = Boolean(settings.enabled);
    powerMinutes.value = String(settings.idleMinutes ?? 15);
    powerAction.value = settings.action === "shutdown" ? "shutdown" : "sleep";
    setSettingsStatus("", null);
  } catch (error) {
    console.error("【界面】读取电源设置失败", error);
    setSettingsStatus("读取设置失败", "error");
  }
}

async function savePowerSettings() {
  const minutes = Number.parseInt(powerMinutes.value, 10);
  if (!Number.isInteger(minutes) || minutes < 1 || minutes > 1440) {
    setSettingsStatus("空闲分钟需在 1 到 1440 之间", "error");
    return;
  }

  const settings = {
    enabled: powerEnabled.checked,
    idleMinutes: minutes,
    action: powerAction.value === "shutdown" ? "shutdown" : "sleep",
  };

  try {
    await tauriInvoke("update_power_settings", { settings });
    setSettingsStatus("已保存", "ok");
  } catch (error) {
    console.error("【界面】保存电源设置失败", error);
    setSettingsStatus(String(error?.message || error || "保存失败"), "error");
  }
}

periodTabs.addEventListener("click", (event) => {
  const button = event.target.closest(".period-tab");
  if (button) {
    setPeriod(button.dataset.period);
  }
});

closeButton.addEventListener("click", () => {
  if (isMini) {
    void closeSelf();
  } else {
    void quitApp();
  }
});

settingsButton.addEventListener("click", () => {
  settingsPanel.hidden = false;
  void loadPowerSettings();
});

settingsCloseButton.addEventListener("click", () => {
  settingsPanel.hidden = true;
});

saveSettings.addEventListener("click", () => {
  void savePowerSettings();
});

document.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  if (isMini) {
    void closeSelf();
  } else {
    void quitApp();
  }
});

window.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") {
    return;
  }
  if (isMini) {
    void closeSelf();
  } else if (!settingsPanel.hidden) {
    settingsPanel.hidden = true;
  }
});

window.addEventListener("storage", (event) => {
  if (event.key === STORAGE_PERIOD && PERIODS.includes(event.newValue)) {
    period = event.newValue;
    updatePeriodTabs();
    render();
  }
  if (event.key === STORAGE_MODE && ["single", "concurrent"].includes(event.newValue)) {
    mode = event.newValue;
    render();
  }
});

updatePeriodTabs();
void refresh();
window.setInterval(() => {
  void refresh();
}, REFRESH_INTERVAL_MS);
