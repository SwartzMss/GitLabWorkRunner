pub const DASHBOARD_HTML: &str = r##"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>GitLab Work Runner 仪表盘</title>
  <style>
    :root {
      color-scheme: light;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: #f7f9fc;
      color: #111827;
      --border: #d9e0ec;
      --muted: #607087;
      --blue: #315bea;
      --green: #17975d;
      --red: #dc3f3f;
      --amber: #f59e0b;
    }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; background: #f7f9fc; }
    .app { display: grid; grid-template-columns: 285px 1fr; min-height: 100vh; }
    aside { background: #fff; border-right: 1px solid var(--border); display: flex; flex-direction: column; padding: 18px 16px; gap: 18px; }
    .brand { display: flex; align-items: center; gap: 12px; padding: 0 4px 18px; border-bottom: 1px solid #edf0f5; }
    .brand-mark { width: 38px; height: 38px; border-radius: 12px; display: grid; place-items: center; background: linear-gradient(135deg, #ff7a1a, #ef3b2d); color: #fff; font-weight: 800; font-size: 18px; }
    .brand-title { font-size: 19px; font-weight: 750; letter-spacing: 0; }
    .brand-subtitle { color: var(--muted); font-size: 14px; margin-top: 2px; }
    nav { display: grid; gap: 7px; }
    .nav-item { display: flex; align-items: center; gap: 12px; height: 44px; padding: 0 14px; color: #25324a; border-radius: 7px; font-size: 14px; cursor: pointer; user-select: none; }
    .nav-item:hover { background: #f4f7fb; }
    .nav-item.active { background: #fff1df; color: #f05a1a; font-weight: 650; }
    .nav-icon { width: 20px; height: 20px; display: inline-grid; place-items: center; flex: 0 0 20px; color: #526176; }
    .nav-item.active .nav-icon { color: #ff5a1f; }
    .nav-icon svg { width: 19px; height: 19px; display: block; stroke: currentColor; stroke-width: 2.2; stroke-linecap: round; stroke-linejoin: round; fill: none; }
    .aside-footer { margin-top: auto; border: 1px solid #e1e6f0; border-radius: 8px; padding: 15px; font-size: 13px; color: var(--muted); }
    .aside-footer strong { display: block; color: #111827; margin-bottom: 7px; }
    .aside-footer a { color: #2448df; text-decoration: none; display: inline-block; margin-top: 10px; }
    .shell { min-width: 0; display: flex; flex-direction: column; }
    header { height: 68px; display: flex; align-items: center; justify-content: space-between; gap: 18px; background: #fff; border-bottom: 1px solid var(--border); padding: 0 28px; }
    .menu { font-size: 22px; color: #526176; }
    .top-actions { display: flex; align-items: center; gap: 18px; color: #526176; }
    .healthy { display: inline-flex; align-items: center; gap: 7px; background: #eaf8ef; color: #11804d; border: 1px solid #d2efdc; border-radius: 8px; padding: 8px 14px; font-size: 13px; font-weight: 650; }
    button { border: 1px solid #cfd7e6; background: #fff; color: #1f2a44; border-radius: 6px; padding: 9px 14px; cursor: pointer; font-weight: 600; }
    button.primary { background: #315bea; border-color: #315bea; color: #fff; }
    button.link { border: 0; padding: 0; background: transparent; color: #2448df; }
    main { padding: 28px 32px 24px; display: grid; gap: 20px; }
    h1 { margin: 0; font-size: 26px; letter-spacing: 0; }
    h2 { margin: 0; font-size: 16px; letter-spacing: 0; }
    .subtitle { color: var(--muted); font-size: 14px; margin-top: 6px; }
    .metrics { display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 18px; }
    .metric-card, .panel, .filter-panel { background: #fff; border: 1px solid var(--border); border-radius: 9px; box-shadow: 0 8px 20px rgba(15, 23, 42, .03); }
    .metric-card { min-height: 126px; padding: 24px 22px; display: grid; grid-template-columns: 58px minmax(0, 1fr) auto; gap: 18px; align-items: center; }
    .metric-card > div { min-width: 0; }
    .metric-icon { width: 56px; height: 56px; border-radius: 14px; color: #fff; display: grid; place-items: center; font-size: 26px; font-weight: 800; }
    .metric-icon.blue { background: linear-gradient(135deg, #5867f1, #315bea); }
    .metric-icon.sky { background: linear-gradient(135deg, #1aa4f5, #1479d6); }
    .metric-icon.green { background: linear-gradient(135deg, #26b06f, #12834e); }
    .metric-icon.red { background: linear-gradient(135deg, #f35d5d, #d93636); }
    .metric-label { color: #3f4b63; font-size: 14px; }
    .metric-value { margin-top: 7px; font-size: 28px; font-weight: 800; }
    .metric-sub { color: var(--muted); font-size: 13px; margin-top: 3px; }
    .spark { width: 96px; height: 42px; }
    .filter-panel { padding: 18px; display: grid; grid-template-columns: 230px 1fr 1fr auto auto; gap: 14px; align-items: end; }
    label { display: grid; gap: 7px; color: #1f2a44; font-size: 13px; font-weight: 650; }
    select, input { height: 38px; border: 1px solid #cfd7e6; border-radius: 6px; background: #fff; padding: 0 12px; color: #1f2a44; }
    .content-grid { display: grid; grid-template-columns: minmax(0, 1fr) 430px; gap: 20px; }
    .bottom-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }
    .panel-header { min-height: 56px; display: flex; align-items: center; justify-content: space-between; gap: 12px; border-bottom: 1px solid #e5eaf2; padding: 12px 20px; flex-wrap: wrap; }
    .panel-title { display: inline-flex; align-items: center; gap: 10px; font-weight: 750; }
    table { width: 100%; border-collapse: collapse; font-size: 13px; }
    th, td { padding: 12px 20px; border-bottom: 1px solid #edf1f6; text-align: left; white-space: nowrap; vertical-align: top; }
    th { color: #526176; font-weight: 700; background: #fbfcfe; }
    td code { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; }
    .badge { display: inline-flex; align-items: center; gap: 5px; border-radius: 6px; padding: 3px 8px; font-size: 12px; font-weight: 700; }
    .badge.completed { background: #e7f8ed; color: #10804c; }
    .badge.failed, .badge.error { background: #fdecec; color: #cf3131; }
    .badge.running { background: #eef3ff; color: #315bea; }
    .badge.warning { background: #fff7e6; color: #c77800; }
    .badge.info { background: #eaf3ff; color: #2468c9; }
    .badge.unknown { background: #eef1f6; color: #526176; }
    .clickable { cursor: pointer; }
    .clickable:hover { background: #f8fafc; }
    .side-stack, .detail-grid { display: grid; gap: 20px; }
    .status-list, .detail-list { padding: 18px 20px; display: grid; gap: 13px; font-size: 14px; }
    .status-row, .detail-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
    .status-pill { background: #e7f8ed; color: #10804c; font-weight: 700; border-radius: 6px; padding: 4px 10px; font-size: 12px; }
    .finding-body { padding: 20px; display: grid; grid-template-columns: 150px 1fr; align-items: center; gap: 18px; }
    .donut { --error: 0deg; --warning: 0deg; --info: 0deg; width: 126px; height: 126px; border-radius: 50%; background: conic-gradient(#ef4444 0 var(--error), #f59e0b var(--error) var(--warning), #2f80ed var(--warning) var(--info), #e6ebf2 var(--info) 360deg); display: grid; place-items: center; }
    .donut-center { width: 74px; height: 74px; border-radius: 50%; background: #fff; display: grid; place-items: center; text-align: center; font-weight: 800; font-size: 18px; line-height: 1.1; }
    .donut-center span { display: block; color: var(--muted); font-size: 12px; font-weight: 600; }
    .legend { display: grid; gap: 8px; font-size: 13px; color: #3f4b63; }
    .legend-row { display: grid; grid-template-columns: 12px 1fr auto; gap: 8px; align-items: center; }
    .dot { width: 10px; height: 10px; border-radius: 3px; }
    .dot.error { background: #ef4444; }
    .dot.warning { background: #f59e0b; }
    .dot.info { background: #2f80ed; }
    .progress { width: 82px; height: 5px; background: #e8edf5; border-radius: 999px; overflow: hidden; display: inline-block; vertical-align: middle; margin-left: 8px; }
    .progress span { display: block; height: 100%; background: #16a05d; }
    .empty { padding: 20px; color: var(--muted); }
    .wrap { white-space: normal; max-width: 520px; }
    .failure { margin: 0 20px 18px; padding: 14px; border-left: 3px solid #cf3131; background: #fff7f7; display: grid; gap: 8px; }
    .failure-message { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; color: #5f2930; font: 12px/1.55 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
    .hidden { display: none !important; }
    @media (max-width: 1500px) {
      table { display: block; overflow-x: auto; }
    }
    @media (max-width: 1200px) {
      .app { grid-template-columns: 1fr; }
      aside { display: none; }
      .content-grid, .bottom-grid, .metrics, .filter-panel { grid-template-columns: 1fr; }
      header, main { padding-left: 16px; padding-right: 16px; }
    }
    @media (max-width: 720px) {
      header { height: auto; min-height: 64px; padding-top: 12px; padding-bottom: 12px; }
      .top-actions { gap: 10px; flex-wrap: wrap; justify-content: flex-end; }
      main { padding-top: 20px; }
      .metric-card { grid-template-columns: 48px minmax(0, 1fr); padding: 18px; gap: 14px; min-height: 104px; }
      .metric-icon { width: 46px; height: 46px; border-radius: 10px; font-size: 22px; }
      .spark { display: none; }
      .filter-panel { padding: 14px; }
      .finding-body { grid-template-columns: 1fr; justify-items: start; }
      .status-row, .detail-row { align-items: flex-start; }
    }
  </style>
</head>
<body>
  <div class="app">
    <aside>
      <div class="brand">
        <div class="brand-mark">GL</div>
        <div><div class="brand-title">GitLab Work Runner</div><div class="brand-subtitle">MR Review 自动化</div></div>
      </div>
      <nav id="nav">
        <div class="nav-item active" data-view="dashboard"><span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 11.5 12 4l9 7.5"/><path d="M5.5 10.5V20h13v-9.5"/><path d="M9.5 20v-5h5v5"/></svg></span>仪表盘</div>
        <div class="nav-item" data-view="projects"><span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="4.5" y="5.5" width="15" height="13" rx="2"/><path d="M8 9h8M8 13h5"/></svg></span>项目</div>
        <div class="nav-item" data-view="mrs"><span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="7" cy="6" r="2"/><circle cx="17" cy="6" r="2"/><circle cx="12" cy="18" r="2"/><path d="M7 8v3a4 4 0 0 0 4 4h1"/><path d="M17 8v3a4 4 0 0 1-4 4h-1"/><path d="M12 15v1"/></svg></span>合并请求</div>
        <div class="nav-item" data-view="runs"><span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m8 5 11 7-11 7z"/></svg></span>Review 运行</div>
        <div class="nav-item" data-view="findings"><span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8"/><path d="M12 8v4"/><path d="M12 16h.01"/></svg></span>问题</div>
        <div class="nav-item" data-view="system"><span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="5" y="5" width="14" height="14" rx="2"/><path d="M9 9h6v6H9z"/></svg></span>系统</div>
      </nav>
      <div class="aside-footer">
        <strong>GitLab Work Runner</strong>
        <div>仪表盘</div>
        <a href="https://github.com/SwartzMss/GitLabWorkRunner" target="_blank" rel="noreferrer">在 GitHub 查看 ↗</a>
      </div>
    </aside>
    <div class="shell">
      <header>
        <div class="menu">≡</div>
        <div class="top-actions"><div class="healthy">✓ 健康</div><button id="refresh" title="刷新">↻</button><div id="localNow">--</div></div>
      </header>
      <main>
        <div><h1 id="pageTitle">仪表盘</h1><div class="subtitle" id="pageSubtitle">Review 自动化与系统活动概览</div></div>
        <div class="metrics" id="metrics"></div>
        <div class="filter-panel" id="filters">
          <label>状态<select id="status"><option value="">全部状态</option><option value="running">运行中</option><option value="completed">已完成</option><option value="failed">失败</option></select></label>
          <label>项目<input id="project" placeholder="输入项目名称、路径或 ID"></label>
          <label>MR IID<input id="mr" placeholder="输入 MR IID"></label>
          <button class="primary" id="apply">应用</button>
          <button id="reset">重置</button>
        </div>
        <div id="content"></div>
      </main>
    </div>
  </div>
  <script>
    const $ = (id) => document.getElementById(id);
    const state = { view: "dashboard", summary: null, findingSummary: null, runs: [], projects: [], mrs: [], findings: [] };
    const titles = {
      dashboard: ["仪表盘", "Review 自动化与系统活动概览"],
      projects: ["项目", "按 GitLab 项目汇总的 Review 活动"],
      mrs: ["合并请求", "按合并请求汇总的 Review 活动"],
      runs: ["Review 运行", "手动 Review 执行与任务结果"],
      findings: ["问题", "AI Review 解析出的结果"],
      system: ["系统", "仪表盘服务与存储状态"]
    };
    const json = async (url) => {
      const response = await fetch(url);
      if (!response.ok) throw new Error(await response.text());
      return response.json();
    };
    const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (ch) => ({ "&":"&amp;", "<":"&lt;", ">":"&gt;", '"':"&quot;", "'":"&#39;" }[ch]));
    const short = (value, n = 8) => value ? String(value).slice(0, n) : "";
    const pad = (value) => String(value).padStart(2, "0");
    const fmtDateTime = (date) => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
    const fmtClockTime = (date) => `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
    const fmtTime = (value) => {
      if (!value) return "";
      const date = new Date(value);
      return Number.isNaN(date.getTime()) ? String(value) : fmtDateTime(date);
    };
    const relative = (value) => {
      if (!value) return "-";
      const seconds = Math.max(0, Math.round((Date.now() - new Date(value).getTime()) / 1000));
      if (seconds < 60) return `${seconds} 秒前`;
      if (seconds < 3600) return `${Math.round(seconds / 60)} 分钟前`;
      if (seconds < 86400) return `${Math.round(seconds / 3600)} 小时前`;
      return `${Math.round(seconds / 86400)} 天前`;
    };
    const fmtMs = (value) => {
      if (value == null) return "-";
      const seconds = Math.max(0, Math.round(value / 1000));
      if (seconds < 60) return `${seconds} 秒`;
      const minutes = Math.floor(seconds / 60);
      if (minutes < 60) return `${minutes} 分 ${seconds % 60} 秒`;
      return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分 ${seconds % 60} 秒`;
    };
    const statusText = (status) => ({ running: "运行中", completed: "已完成", failed: "失败", error: "错误" }[status] || status || "-");
    const severityText = (severity) => ({ error: "严重", warning: "主要", info: "提示" }[severity] || severity || "-");
    const badge = (status) => `<span class="badge ${esc(status || "unknown")}">${esc(statusText(status))}</span>`;
    const severityBadge = (severity) => `<span class="badge ${esc(severity || "unknown")}">${esc(severityText(severity))}</span>`;
    const projectLabel = (item) => item.project_label || `#${item.project_id}`;
    const empty = (cols, text = "暂无数据") => `<tr><td class="empty" colspan="${cols}">${text}</td></tr>`;
    const row = (cells, attrs = "") => `<tr ${attrs}>${cells.map((cell) => `<td>${cell ?? ""}</td>`).join("")}</tr>`;
    const pct = (part, total) => total ? ((part / total) * 100).toFixed(1) : "0.0";
    const fmtBytes = (value) => value == null ? "-" : value < 1024 ? `${value} B` : `${(value / 1024).toFixed(1)} KB`;
    const fmtLimit = (value) => value === 0 ? "不限制" : esc(value ?? "-");
    const coverageReason = (reason) => ({
      max_batches_reached: "达到批次上限",
      single_file_diff_truncated: "单文件 Diff 超过批次限制",
      batch_execution_failed: "批次执行失败"
    }[reason] || reason || "-");
    const errorCodeText = (code) => ({
      gitlab_api_timeout: "GitLab API 超时", gitlab_api_failed: "GitLab API 失败",
      archive_download_timeout: "Archive 下载超时", archive_download_failed: "Archive 下载失败",
      archive_extract_failed: "Archive 解压失败", archive_limit_exceeded: "Archive 超出限制",
      ai_request_timeout: "AI 请求超时", ai_request_failed: "AI 请求失败",
      ai_tool_loop_timeout: "AI 工具循环超时", ai_response_parse_failed: "AI 响应解析失败",
      review_run_timeout: "Review 整体超时", gitlab_comment_failed: "GitLab 发布失败", permission_denied: "权限不足",
      invalid_configuration: "配置无效", internal: "内部错误"
    }[code] || code || "未分类错误");
    const executionModeText = (mode) => ({ context: "Context", diff_only_fallback: "Diff-only 降級" }[mode] || mode || "");
    const fallbackReasonText = (reason) => ({
      archive_limit_exceeded: "Archive 超出限制",
      review_run_timeout: "Context 整体超时",
      ai_request_timeout: "AI 请求超时",
      ai_tool_loop_timeout: "AI 工具循环超时"
    }[reason] || reason || "");
    const renderFailure = (failure) => failure ? `<div class="failure"><div><span class="badge failed">${esc(errorCodeText(failure.code))}</span>${failure.code ? ` <code>${esc(failure.code)}</code>` : ""}</div><pre class="failure-message">${esc(failure.message || "-")}</pre></div>` : "";

    function renderExecutionMetadata(task) {
      const rows = [];
      if (task.execution_mode != null) rows.push(`<div class="detail-row"><span>执行模式</span><span>${esc(executionModeText(task.execution_mode))}</span></div>`);
      if (task.fallback_reason != null) rows.push(`<div class="detail-row"><span>降級原因</span><span>${esc(fallbackReasonText(task.fallback_reason))}</span></div>`);
      if (task.context_elapsed_ms != null) rows.push(`<div class="detail-row"><span>Context 阶段</span><span>${fmtMs(task.context_elapsed_ms)}</span></div>`);
      if (task.fallback_elapsed_ms != null) rows.push(`<div class="detail-row"><span>Diff-only 阶段</span><span>${fmtMs(task.fallback_elapsed_ms)}</span></div>`);
      if (task.context_elapsed_ms != null || task.fallback_elapsed_ms != null) {
        const total = (task.context_elapsed_ms ?? 0) + (task.fallback_elapsed_ms ?? 0);
        rows.push(`<div class="detail-row"><span>AI 总耗时</span><span>${fmtMs(total)}</span></div>`);
      }
      if (!rows.length) return "";
      return `<div class="detail-list">${rows.join("")}</div>`;
    }

    function renderTaskCoverage(task) {
      const metadata = renderExecutionMetadata(task);
      if (task.coverage_total_files == null) {
        const failure = renderFailure(task.error ? { code: task.error_code, message: task.error } : null);
        return `<div class="detail-list">
          <div class="detail-row"><span>任务</span><span>${esc(task.title)}</span></div>
          <div class="detail-row"><span>状态</span><span>${badge(task.status)}</span></div>
          <div class="detail-row"><span>问题</span><span>${esc(task.findings ?? 0)}</span></div>
        </div>${metadata}${failure}`;
      }
      const batchStats = `${task.coverage_completed_batches ?? 0} 已用 / ${fmtLimit(task.coverage_max_batches)} 上限`;
      const toolCallStats = `${task.tool_calls_used ?? 0} 单批峰值 / ${fmtLimit(task.max_tool_calls)} 每批上限`;
      if (task.status === "failed" && (task.coverage_completed_batches === 0 || task.coverage_reviewed_diff_bytes === 0)) {
        return `<div class="detail-list">
          <div class="detail-row"><span>覆盖情况</span><span></span></div>
          <div class="detail-row"><span>批次</span><span>${batchStats}</span></div>
          <div class="detail-row"><span>工具调用</span><span>${toolCallStats}</span></div>
        </div>${metadata}`;
      }
      const state = task.coverage_complete ? "完整" : "部分";
      const files = task.incomplete_files || [];
      const incomplete = files.length ? `<table><thead><tr><th>文件</th><th>状态</th><th>原因</th><th>已审查 Diff</th><th>总 Diff</th></tr></thead><tbody>${files.map((file) => row([esc(file.path), file.status === "partial" ? "部分" : "未审查", coverageReason(file.reason), fmtBytes(file.reviewed_diff_bytes), fmtBytes(file.total_diff_bytes)])).join("")}</tbody></table>` : "";
      return `<div class="detail-list">
        <div class="detail-row"><span>覆盖情况</span><span class="badge ${task.coverage_complete ? "completed" : "warning"}">${state}</span></div>
        <div class="detail-row"><span>文件</span><span>${task.coverage_fully_reviewed_files} 完整 / ${task.coverage_partially_reviewed_files} 部分 / ${task.coverage_unreviewed_files} 未审查 / ${task.coverage_total_files} 总计</span></div>
        <div class="detail-row"><span>Diff</span><span>${fmtBytes(task.coverage_reviewed_diff_bytes)} / ${fmtBytes(task.coverage_total_diff_bytes)} (${pct(task.coverage_reviewed_diff_bytes, task.coverage_total_diff_bytes)}%)</span></div>
        <div class="detail-row"><span>批次</span><span>${batchStats}</span></div>
        <div class="detail-row"><span>工具调用</span><span>${toolCallStats}</span></div>
      </div>${metadata}${incomplete}`;
    }

    function params(includeStatus = true) {
      const search = new URLSearchParams();
      if (includeStatus && $("status").value) search.set("status", $("status").value);
      if ($("project").value) search.set("project", $("project").value);
      if ($("mr").value) search.set("mr_iid", $("mr").value);
      return search.toString();
    }

    function spark(color) {
      return `<svg class="spark" viewBox="0 0 96 42" aria-hidden="true"><polyline fill="none" stroke="${color}" stroke-width="2" points="1,27 9,20 15,31 22,12 28,26 34,18 42,30 48,16 55,14 63,27 70,8 78,12 86,30 95,22"/></svg>`;
    }

    async function load() {
      $("localNow").textContent = fmtClockTime(new Date());
      const runParams = params(true);
      const listParams = params(false);
      const [summary, findingSummary, runs, projects, mrs, findings] = await Promise.all([
        json("/api/summary"),
        json("/api/finding-summary"),
        json(`/api/runs?${runParams}`),
        json("/api/projects"),
        json("/api/merge-requests"),
        json(`/api/findings?${listParams}`),
      ]);
      state.summary = summary;
      state.findingSummary = findingSummary;
      state.runs = runs.runs;
      state.projects = projects.projects;
      state.mrs = mrs.merge_requests;
      state.findings = findings.findings;
      render();
    }

    function render() {
      const [title, subtitle] = titles[state.view];
      $("pageTitle").textContent = title;
      $("pageSubtitle").textContent = subtitle;
      document.querySelectorAll(".nav-item").forEach((item) => item.classList.toggle("active", item.dataset.view === state.view));
      renderMetrics();
      $("filters").classList.toggle("hidden", state.view === "system");
      const renderers = { dashboard: renderDashboard, projects: renderProjectsPage, mrs: renderMrsPage, runs: renderRunsPage, findings: renderFindingsPage, system: renderSystemPage };
      $("content").innerHTML = renderers[state.view]();
      bindContentClicks();
    }

    function renderMetrics() {
      const summary = state.summary || {};
      const successRate = pct(summary.completed_runs || 0, summary.total_runs || 0);
      const failureRate = pct(summary.failed_runs || 0, summary.total_runs || 0);
      $("metrics").innerHTML = [
        ["▶", "blue", "总运行数", summary.total_runs || 0, "全部 Review 运行", spark("#315bea")],
        ["◷", "sky", "运行中", summary.running_runs || 0, "当前正在执行", spark("#1479d6")],
        ["✓", "green", "已完成", summary.completed_runs || 0, `成功率&nbsp; <span style="color:#17975d">${successRate}%</span>`, spark("#17975d")],
        ["×", "red", "失败", summary.failed_runs || 0, `失败率&nbsp; <span style="color:#dc3f3f">${failureRate}%</span>`, spark("#dc3f3f")],
      ].map(([icon, color, label, value, sub, graph]) => `<div class="metric-card"><div class="metric-icon ${color}">${icon}</div><div><div class="metric-label">${label}</div><div class="metric-value">${value}</div><div class="metric-sub">${sub}</div></div>${graph}</div>`).join("");
    }

    function renderDashboard() {
      return `<div class="content-grid">
        <section class="panel">
          <div class="panel-header"><div class="panel-title">☷ 最近 Review 运行</div><button class="link" data-view-link="runs">查看全部运行 →</button></div>
          ${runsTable(state.runs.slice(0, 8))}
        </section>
        <div class="side-stack">${serviceStatusPanel()}${findingSummaryPanel()}</div>
      </div>
      <div class="bottom-grid" style="margin-top:20px">
        <section class="panel"><div class="panel-header"><div class="panel-title">□ 项目概览</div><button class="link" data-view-link="projects">打开项目 →</button></div>${projectsTable(state.projects.slice(0, 8))}</section>
        <section class="panel"><div class="panel-header"><div class="panel-title">⌘ 合并请求概览</div><button class="link" data-view-link="mrs">打开 MR →</button></div>${mrsTable(state.mrs.slice(0, 8))}</section>
      </div>`;
    }

    function renderRunsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">▷ Review 运行</div><span>${state.runs.length} 行</span></div>${runsTable(state.runs)}</section>`;
    }

    function renderProjectsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">□ 项目</div><span>${state.projects.length} 个项目</span></div>${projectsTable(state.projects)}</section>`;
    }

    function renderMrsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">⌘ 合并请求</div><span>${state.mrs.length} 个合并请求</span></div>${mrsTable(state.mrs)}</section>`;
    }

    function renderFindingsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">◌ 问题</div><span>${state.findings.length} 行</span></div>
        <table><thead><tr><th>创建时间</th><th>级别</th><th>项目</th><th>MR</th><th>路径</th><th>标题</th></tr></thead><tbody>${state.findings.length ? state.findings.map((finding) => row([
          fmtTime(finding.created_at), severityBadge(finding.severity), esc(projectLabel(finding)), `!${esc(finding.mr_iid)}`,
          `${esc(finding.path)}${finding.new_line ? `:${esc(finding.new_line)}` : ""}`,
          `<div class="wrap"><strong>${esc(finding.title)}</strong><br><span class="subtitle">${esc(finding.message)}</span></div>`
        ], `class="clickable" data-run="${esc(finding.review_run_id)}"`)).join("") : empty(6)}</tbody></table>
      </section>`;
    }

    function renderSystemPage() {
      return `<div class="content-grid"><section class="panel">${serviceStatusPanel()}</section><section class="panel">${findingSummaryPanel()}</section></div>`;
    }

    function runsTable(items) {
      return `<table><thead><tr><th>开始时间</th><th>状态</th><th>项目</th><th>MR</th><th>Commit</th><th>问题</th><th>耗时</th></tr></thead><tbody>${items.length ? items.map((run) => {
        const findingColor = run.findings > 0 ? (run.status === "failed" ? "var(--red)" : "var(--amber)") : "var(--green)";
        return row([fmtTime(run.started_at), badge(run.status), esc(projectLabel(run)), `!${esc(run.mr_iid)}`, `<code>${esc(short(run.commit_sha))}</code>`, `<span style="color:${findingColor}">${run.findings || "-"}</span>`, fmtMs(run.duration_ms)], `class="clickable" data-run="${esc(run.review_run_id)}"`);
      }).join("") : empty(7)}</tbody></table>`;
    }

    function projectsTable(items) {
      return `<table><thead><tr><th>项目</th><th>运行数</th><th>运行中</th><th>失败</th><th>成功率</th><th>最近 Review</th></tr></thead><tbody>${items.length ? items.map((project) => {
        const rate = pct(project.total_runs - project.failed_runs, project.total_runs);
        const label = projectLabel(project);
        return row([`<button class="link" data-project="${esc(label)}">${esc(label)}</button>`, esc(project.total_runs), esc(project.running_runs), esc(project.failed_runs), `${rate}% <span class="progress"><span style="width:${rate}%"></span></span>`, relative(project.last_review_at)], `class="clickable" data-project="${esc(label)}"`);
      }).join("") : empty(6)}</tbody></table>`;
    }

    function mrsTable(items) {
      return `<table><thead><tr><th>MR</th><th>项目</th><th>状态</th><th>运行数</th><th>问题</th><th>最近 Review</th></tr></thead><tbody>${items.length ? items.map((mr) => row([
        `<button class="link" data-project="${esc(projectLabel(mr))}" data-mr="${esc(mr.mr_iid)}">!${esc(mr.mr_iid)}</button>`, esc(projectLabel(mr)), badge(mr.last_status), esc(mr.total_runs),
        mr.total_findings ? `<span style="color:var(--red)">${mr.total_findings}</span>` : `<span style="color:var(--green)">0</span>`, relative(mr.last_review_at)
      ], `class="clickable" data-project="${esc(projectLabel(mr))}" data-mr="${esc(mr.mr_iid)}"`)).join("") : empty(6)}</tbody></table>`;
    }

    function serviceStatusPanel() {
      const summary = state.summary || {};
      return `<section class="panel"><div class="panel-header"><div class="panel-title">✓ 服务状态</div></div><div class="status-list">
        <div class="status-row"><span>仪表盘服务</span><span class="status-pill">健康</span></div>
        <div class="status-row"><span>数据库</span><span class="status-pill">健康</span></div>
        <div class="status-row"><span>最近 Review</span><span>${relative(summary.last_review_at)}</span></div>
        <div class="status-row"><span>最近错误</span><span>${summary.failed_runs > 0 ? `${summary.failed_runs} 次失败运行` : "-"}</span></div>
      </div></section>`;
    }

    function findingSummaryPanel() {
      const summary = state.findingSummary || { total: 0, error: 0, warning: 0, info: 0 };
      const total = summary.total || 0;
      const error = total ? (summary.error / total) * 360 : 0;
      const warning = error + (total ? (summary.warning / total) * 360 : 0);
      const info = warning + (total ? (summary.info / total) * 360 : 0);
      return `<section class="panel"><div class="panel-header"><div class="panel-title">◌ 问题汇总</div></div><div class="finding-body">
        <div class="donut" style="--error:${error}deg;--warning:${warning}deg;--info:${info}deg"><div class="donut-center"><div>${total}</div><span>总数</span></div></div>
        <div class="legend">
          <div class="legend-row"><span class="dot error"></span><span>严重</span><span>${summary.error} (${pct(summary.error, total)}%)</span></div>
          <div class="legend-row"><span class="dot warning"></span><span>主要</span><span>${summary.warning} (${pct(summary.warning, total)}%)</span></div>
          <div class="legend-row"><span class="dot info"></span><span>提示</span><span>${summary.info} (${pct(summary.info, total)}%)</span></div>
        </div>
      </div></section>`;
    }

    async function openRunDetail(reviewRunId) {
      const detail = await json(`/api/runs/${encodeURIComponent(reviewRunId)}`);
      $("content").innerHTML = `<div class="detail-grid">
        <section class="panel"><div class="panel-header"><div class="panel-title">Review 运行详情</div><button class="link" id="backToRuns">返回运行列表</button></div><div class="detail-list">
          <div class="detail-row"><span>运行 ID</span><code>${esc(detail.run.review_run_id)}</code></div>
          <div class="detail-row"><span>状态</span>${badge(detail.run.status)}</div>
          <div class="detail-row"><span>项目 / MR</span><span>${esc(projectLabel(detail.run))} / !${esc(detail.run.mr_iid)}</span></div>
          <div class="detail-row"><span>Commit</span><code>${esc(detail.run.commit_sha)}</code></div>
          <div class="detail-row"><span>问题</span><span>${esc(detail.run.findings)}</span></div>
        </div>${renderFailure(detail.failure)}</section>
        <section class="panel"><div class="panel-header"><div class="panel-title">Review 覆盖</div></div>${detail.tasks.length ? detail.tasks.map(renderTaskCoverage).join("") : `<div class="empty">暂无任务数据</div>`}</section>
        <section class="panel"><div class="panel-header"><div class="panel-title">问题</div></div><table><thead><tr><th>级别</th><th>路径</th><th>标题</th><th>消息</th></tr></thead><tbody>${detail.findings.length ? detail.findings.map((finding) => row([severityBadge(finding.severity), `${esc(finding.path)}${finding.new_line ? `:${esc(finding.new_line)}` : ""}`, esc(finding.title), `<span class="wrap">${esc(finding.message)}</span>`])).join("") : empty(4)}</tbody></table></section>
      </div>`;
      $("backToRuns").addEventListener("click", () => setView("runs"));
    }

    function setView(view) {
      state.view = view;
      window.location.hash = view;
      render();
    }

    function bindContentClicks() {
      document.querySelectorAll("[data-view-link]").forEach((el) => el.addEventListener("click", () => setView(el.dataset.viewLink)));
      document.querySelectorAll("[data-run]").forEach((el) => el.addEventListener("click", (event) => { event.stopPropagation(); openRunDetail(el.dataset.run).catch(showError); }));
      document.querySelectorAll("[data-project]").forEach((el) => el.addEventListener("click", (event) => {
        event.stopPropagation();
        $("project").value = el.dataset.project || "";
        $("mr").value = el.dataset.mr || "";
        setView("runs");
        load().catch(showError);
      }));
    }

    function showError(err) {
      $("content").innerHTML = `<section class="panel"><div class="empty">加载失败：${esc(err.message)}</div></section>`;
    }

    document.querySelectorAll(".nav-item").forEach((item) => item.addEventListener("click", () => setView(item.dataset.view)));
    $("refresh").addEventListener("click", () => load().catch(showError));
    $("apply").addEventListener("click", () => load().catch(showError));
    $("reset").addEventListener("click", () => { $("status").value = ""; $("project").value = ""; $("mr").value = ""; load().catch(showError); });
    const initialView = window.location.hash.replace("#", "");
    if (titles[initialView]) state.view = initialView;
    load().catch(showError);
  </script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_shell_is_localized_for_chinese_users() {
        assert!(DASHBOARD_HTML.contains(r#"<html lang="zh-CN">"#));
        assert!(DASHBOARD_HTML.contains("仪表盘"));
        assert!(DASHBOARD_HTML.contains("项目"));
        assert!(DASHBOARD_HTML.contains("fmtClockTime(new Date())"));
        assert!(!DASHBOARD_HTML.contains("UTC --"));
        assert!(!DASHBOARD_HTML.contains("Dashboard</div>"));
    }

    #[test]
    fn dashboard_shell_keeps_findings_without_comments_ui() {
        assert!(DASHBOARD_HTML.contains(r#"data-view="findings""#));
        assert!(DASHBOARD_HTML.contains("问题"));
        assert!(!DASHBOARD_HTML.contains(r#"data-view="comments""#));
        assert!(!DASHBOARD_HTML.contains("renderCommentsPage"));
        assert!(!DASHBOARD_HTML.contains("/api/comments"));
        assert!(!DASHBOARD_HTML.contains("评论"));
    }

    #[test]
    fn dashboard_copy_is_ai_review_only() {
        assert!(DASHBOARD_HTML.contains("AI Review 解析出的结果"));
        assert!(!DASHBOARD_HTML.contains("AI 与脚本解析出的结果"));
        assert!(!DASHBOARD_HTML.contains("script_task_failed"));
    }

    #[test]
    fn run_detail_shows_ai_review_coverage_without_comments() {
        assert!(DASHBOARD_HTML.contains("detail.findings"));
        assert!(DASHBOARD_HTML.contains("detail.tasks"));
        assert!(DASHBOARD_HTML.contains("Review 覆盖"));
        assert!(DASHBOARD_HTML.contains("coverage_completed_batches === 0"));
        assert!(DASHBOARD_HTML.contains("达到批次上限"));
        assert!(DASHBOARD_HTML.contains("单文件 Diff 超过批次限制"));
        assert!(DASHBOARD_HTML.contains("批次执行失败"));
    }

    #[test]
    fn run_detail_renders_generic_summary_for_legacy_task_without_coverage() {
        let legacy_task = serde_json::json!({
            "title": "Historical Task",
            "status": "failed",
            "findings": 3,
            "error_code": null,
            "error": "legacy failure",
            "coverage_total_files": null
        });

        assert_eq!(legacy_task["coverage_total_files"], serde_json::Value::Null);
        assert!(DASHBOARD_HTML.contains("${esc(task.title)}"));
        assert!(DASHBOARD_HTML.contains("${badge(task.status)}"));
        assert!(DASHBOARD_HTML.contains("${esc(task.findings ?? 0)}"));
        assert!(DASHBOARD_HTML.contains(
            "renderFailure(task.error ? { code: task.error_code, message: task.error } : null)"
        ));
        assert!(!DASHBOARD_HTML.contains(
            r#"return `<div class="detail-list"><div class="detail-row"><span>覆盖情况</span><span></span></div></div>`;"#
        ));
    }

    #[test]
    fn run_detail_renders_execution_metadata_and_human_durations() {
        assert!(
            DASHBOARD_HTML.contains(r#"context: "Context", diff_only_fallback: "Diff-only 降級""#)
        );
        assert!(DASHBOARD_HTML.contains(r#"archive_limit_exceeded: "Archive 超出限制""#));
        assert!(DASHBOARD_HTML.contains(r#"review_run_timeout: "Context 整体超时""#));
        assert!(DASHBOARD_HTML.contains(r#"ai_request_timeout: "AI 请求超时""#));
        assert!(DASHBOARD_HTML.contains(r#"ai_tool_loop_timeout: "AI 工具循环超时""#));
        assert!(DASHBOARD_HTML.contains("<span>Context 阶段</span>"));
        assert!(DASHBOARD_HTML.contains("<span>Diff-only 阶段</span>"));
        assert!(DASHBOARD_HTML.contains("<span>AI 总耗时</span>"));
        let total_seconds = (2_400_000 + 386_000) / 1_000;
        assert_eq!(total_seconds / 60, 46);
        assert_eq!(total_seconds % 60, 26);
        assert!(DASHBOARD_HTML.contains(
            "const total = (task.context_elapsed_ms ?? 0) + (task.fallback_elapsed_ms ?? 0)"
        ));
        assert!(DASHBOARD_HTML.contains("`${minutes} 分 ${seconds % 60} 秒`"));
        assert!(DASHBOARD_HTML
            .contains("`${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分 ${seconds % 60} 秒`"));
        assert!(DASHBOARD_HTML
            .contains("task.context_elapsed_ms != null || task.fallback_elapsed_ms != null"));
    }

    #[test]
    fn run_detail_escapes_unknown_execution_metadata_and_hides_null_rows() {
        assert!(DASHBOARD_HTML.contains("esc(executionModeText(task.execution_mode))"));
        assert!(DASHBOARD_HTML.contains("esc(fallbackReasonText(task.fallback_reason))"));
        assert!(DASHBOARD_HTML.contains("if (!rows.length) return \"\";"));
        assert!(DASHBOARD_HTML.contains("task.context_elapsed_ms != null"));
        assert!(DASHBOARD_HTML.contains("task.fallback_elapsed_ms != null"));
    }

    #[test]
    fn run_detail_renders_legacy_failures_without_codes() {
        assert!(DASHBOARD_HTML.contains(r#"}[code] || code || "未分类错误""#));
        assert!(DASHBOARD_HTML.contains("failure.code ?"));
        assert!(DASHBOARD_HTML.contains("renderFailure(detail.failure)"));
    }

    #[test]
    fn run_id_and_task_count_are_hidden_from_lists() {
        assert!(!DASHBOARD_HTML.contains("<th>运行 ID</th>"));
        assert!(!DASHBOARD_HTML.contains("<th>任务</th>"));
        assert!(!DASHBOARD_HTML.contains("${completedTasks}/${totalTasks}"));
        assert!(DASHBOARD_HTML.contains(
            r#"<div class="detail-row"><span>运行 ID</span><code>${esc(detail.run.review_run_id)}</code></div>"#
        ));
    }

    #[test]
    fn dashboard_css_has_responsive_card_and_table_layouts() {
        assert!(DASHBOARD_HTML.contains("repeat(auto-fit, minmax(260px, 1fr))"));
        assert!(DASHBOARD_HTML.contains("@media (max-width: 1500px)"));
        assert!(DASHBOARD_HTML.contains("table { display: block; overflow-x: auto; }"));
        assert!(DASHBOARD_HTML.contains("@media (max-width: 720px)"));
        assert!(DASHBOARD_HTML.contains("grid-template-columns: 48px minmax(0, 1fr);"));
        assert!(DASHBOARD_HTML.contains(".spark { display: none; }"));
    }
}
