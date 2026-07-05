pub const DASHBOARD_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>GitLab Work Runner Dashboard</title>
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
    .nav-item.active { background: #eef2ff; color: #2448df; font-weight: 650; }
    .nav-icon { width: 18px; text-align: center; color: inherit; }
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
    .metrics { display: grid; grid-template-columns: repeat(4, minmax(180px, 1fr)); gap: 18px; }
    .metric-card, .panel, .filter-panel { background: #fff; border: 1px solid var(--border); border-radius: 9px; box-shadow: 0 8px 20px rgba(15, 23, 42, .03); }
    .metric-card { min-height: 126px; padding: 24px 22px; display: grid; grid-template-columns: 58px 1fr 96px; gap: 18px; align-items: center; }
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
    .panel-header { height: 56px; display: flex; align-items: center; justify-content: space-between; border-bottom: 1px solid #e5eaf2; padding: 0 20px; }
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
    .hidden { display: none !important; }
    @media (max-width: 1200px) {
      .app { grid-template-columns: 1fr; }
      aside { display: none; }
      .content-grid, .bottom-grid, .metrics, .filter-panel { grid-template-columns: 1fr; }
      header, main { padding-left: 16px; padding-right: 16px; }
      table { display: block; overflow-x: auto; }
    }
  </style>
</head>
<body>
  <div class="app">
    <aside>
      <div class="brand">
        <div class="brand-mark">GL</div>
        <div><div class="brand-title">GitLab Work Runner</div><div class="brand-subtitle">MR Review Automation</div></div>
      </div>
      <nav id="nav">
        <div class="nav-item active" data-view="dashboard"><span class="nav-icon">⌂</span>Dashboard</div>
        <div class="nav-item" data-view="projects"><span class="nav-icon">□</span>Projects</div>
        <div class="nav-item" data-view="mrs"><span class="nav-icon">⌘</span>Merge Requests</div>
        <div class="nav-item" data-view="runs"><span class="nav-icon">▷</span>Review Runs</div>
        <div class="nav-item" data-view="findings"><span class="nav-icon">◌</span>Findings</div>
        <div class="nav-item" data-view="comments"><span class="nav-icon">◍</span>Comments</div>
        <div class="nav-item" data-view="system"><span class="nav-icon">▣</span>System</div>
      </nav>
      <div class="aside-footer">
        <strong>GitLab Work Runner</strong>
        <div>Dashboard</div>
        <a href="https://github.com/SwartzMss/GitLabWorkRunner" target="_blank" rel="noreferrer">View on GitHub ↗</a>
      </div>
    </aside>
    <div class="shell">
      <header>
        <div class="menu">≡</div>
        <div class="top-actions"><div class="healthy">✓ Healthy</div><button id="refresh" title="Refresh">↻</button><div id="utcNow">UTC --</div></div>
      </header>
      <main>
        <div><h1 id="pageTitle">Dashboard</h1><div class="subtitle" id="pageSubtitle">Overview of review automation and system activity</div></div>
        <div class="metrics" id="metrics"></div>
        <div class="filter-panel" id="filters">
          <label>Status<select id="status"><option value="">All Status</option><option value="running">Running</option><option value="completed">Completed</option><option value="failed">Failed</option></select></label>
          <label>Project ID<input id="project" placeholder="Enter project ID"></label>
          <label>MR IID<input id="mr" placeholder="Enter MR IID"></label>
          <button class="primary" id="apply">Apply</button>
          <button id="reset">Reset</button>
        </div>
        <div id="content"></div>
      </main>
    </div>
  </div>
  <script>
    const $ = (id) => document.getElementById(id);
    const state = { view: "dashboard", summary: null, findingSummary: null, runs: [], projects: [], mrs: [], findings: [], comments: [] };
    const titles = {
      dashboard: ["Dashboard", "Overview of review automation and system activity"],
      projects: ["Projects", "Review activity grouped by GitLab project"],
      mrs: ["Merge Requests", "Review activity grouped by merge request"],
      runs: ["Review Runs", "Manual review executions and task results"],
      findings: ["Findings", "Parsed AI and script findings"],
      comments: ["Comments", "Comments posted back to GitLab"],
      system: ["System", "Dashboard service and storage status"]
    };
    const json = async (url) => {
      const response = await fetch(url);
      if (!response.ok) throw new Error(await response.text());
      return response.json();
    };
    const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (ch) => ({ "&":"&amp;", "<":"&lt;", ">":"&gt;", '"':"&quot;", "'":"&#39;" }[ch]));
    const short = (value, n = 8) => value ? String(value).slice(0, n) : "";
    const fmtTime = (value) => value ? String(value).replace("T", " ").replace("+00:00", "").replace("Z", "") : "";
    const relative = (value) => {
      if (!value) return "-";
      const seconds = Math.max(0, Math.round((Date.now() - new Date(value).getTime()) / 1000));
      if (seconds < 60) return `${seconds}s ago`;
      if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
      if (seconds < 86400) return `${Math.round(seconds / 3600)} hr ago`;
      return `${Math.round(seconds / 86400)} days ago`;
    };
    const fmtMs = (value) => {
      if (value == null) return "-";
      const seconds = Math.max(0, Math.round(value / 1000));
      if (seconds < 60) return `${seconds}s`;
      return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
    };
    const badge = (status) => `<span class="badge ${esc(status || "unknown")}">${esc(status || "-")}</span>`;
    const empty = (cols, text = "No data") => `<tr><td class="empty" colspan="${cols}">${text}</td></tr>`;
    const row = (cells, attrs = "") => `<tr ${attrs}>${cells.map((cell) => `<td>${cell ?? ""}</td>`).join("")}</tr>`;
    const pct = (part, total) => total ? ((part / total) * 100).toFixed(1) : "0.0";

    function params(includeStatus = true) {
      const search = new URLSearchParams();
      if (includeStatus && $("status").value) search.set("status", $("status").value);
      if ($("project").value) search.set("project_id", $("project").value);
      if ($("mr").value) search.set("mr_iid", $("mr").value);
      return search.toString();
    }

    function spark(color) {
      return `<svg class="spark" viewBox="0 0 96 42" aria-hidden="true"><polyline fill="none" stroke="${color}" stroke-width="2" points="1,27 9,20 15,31 22,12 28,26 34,18 42,30 48,16 55,14 63,27 70,8 78,12 86,30 95,22"/></svg>`;
    }

    async function load() {
      $("utcNow").textContent = `UTC ${new Date().toISOString().slice(0, 19).replace("T", " ")}`;
      const runParams = params(true);
      const listParams = params(false);
      const [summary, findingSummary, runs, projects, mrs, findings, comments] = await Promise.all([
        json("/api/summary"),
        json("/api/finding-summary"),
        json(`/api/runs?${runParams}`),
        json("/api/projects"),
        json("/api/merge-requests"),
        json(`/api/findings?${listParams}`),
        json(`/api/comments?${listParams}`),
      ]);
      state.summary = summary;
      state.findingSummary = findingSummary;
      state.runs = runs.runs;
      state.projects = projects.projects;
      state.mrs = mrs.merge_requests;
      state.findings = findings.findings;
      state.comments = comments.comments;
      render();
    }

    function render() {
      const [title, subtitle] = titles[state.view];
      $("pageTitle").textContent = title;
      $("pageSubtitle").textContent = subtitle;
      document.querySelectorAll(".nav-item").forEach((item) => item.classList.toggle("active", item.dataset.view === state.view));
      renderMetrics();
      $("filters").classList.toggle("hidden", state.view === "system");
      const renderers = { dashboard: renderDashboard, projects: renderProjectsPage, mrs: renderMrsPage, runs: renderRunsPage, findings: renderFindingsPage, comments: renderCommentsPage, system: renderSystemPage };
      $("content").innerHTML = renderers[state.view]();
      bindContentClicks();
    }

    function renderMetrics() {
      const summary = state.summary || {};
      const successRate = pct(summary.completed_runs || 0, summary.total_runs || 0);
      const failureRate = pct(summary.failed_runs || 0, summary.total_runs || 0);
      $("metrics").innerHTML = [
        ["▶", "blue", "Total Runs", summary.total_runs || 0, "All time review runs", spark("#315bea")],
        ["◷", "sky", "Running", summary.running_runs || 0, "Currently running", spark("#1479d6")],
        ["✓", "green", "Completed", summary.completed_runs || 0, `Success rate&nbsp; <span style="color:#17975d">${successRate}%</span>`, spark("#17975d")],
        ["×", "red", "Failed", summary.failed_runs || 0, `Failure rate&nbsp; <span style="color:#dc3f3f">${failureRate}%</span>`, spark("#dc3f3f")],
      ].map(([icon, color, label, value, sub, graph]) => `<div class="metric-card"><div class="metric-icon ${color}">${icon}</div><div><div class="metric-label">${label}</div><div class="metric-value">${value}</div><div class="metric-sub">${sub}</div></div>${graph}</div>`).join("");
    }

    function renderDashboard() {
      return `<div class="content-grid">
        <section class="panel">
          <div class="panel-header"><div class="panel-title">☷ Recent Review Runs</div><button class="link" data-view-link="runs">View all runs →</button></div>
          ${runsTable(state.runs.slice(0, 8))}
        </section>
        <div class="side-stack">${serviceStatusPanel()}${findingSummaryPanel()}</div>
      </div>
      <div class="bottom-grid" style="margin-top:20px">
        <section class="panel"><div class="panel-header"><div class="panel-title">□ Projects Overview</div><button class="link" data-view-link="projects">Open projects →</button></div>${projectsTable(state.projects.slice(0, 8))}</section>
        <section class="panel"><div class="panel-header"><div class="panel-title">⌘ Merge Requests Overview</div><button class="link" data-view-link="mrs">Open MRs →</button></div>${mrsTable(state.mrs.slice(0, 8))}</section>
      </div>`;
    }

    function renderRunsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">▷ Review Runs</div><span>${state.runs.length} rows</span></div>${runsTable(state.runs)}</section>`;
    }

    function renderProjectsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">□ Projects</div><span>${state.projects.length} projects</span></div>${projectsTable(state.projects)}</section>`;
    }

    function renderMrsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">⌘ Merge Requests</div><span>${state.mrs.length} merge requests</span></div>${mrsTable(state.mrs)}</section>`;
    }

    function renderFindingsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">◌ Findings</div><span>${state.findings.length} rows</span></div>
        <table><thead><tr><th>Created</th><th>Severity</th><th>Project</th><th>MR</th><th>Path</th><th>Title</th><th>Run ID</th></tr></thead><tbody>${state.findings.length ? state.findings.map((finding) => row([
          fmtTime(finding.created_at), badge(finding.severity), esc(finding.project_id), `!${esc(finding.mr_iid)}`,
          `${esc(finding.path)}${finding.new_line ? `:${esc(finding.new_line)}` : ""}`,
          `<div class="wrap"><strong>${esc(finding.title)}</strong><br><span class="subtitle">${esc(finding.message)}</span></div>`,
          `<button class="link" data-run="${esc(finding.review_run_id)}">${esc(short(finding.review_run_id, 12))}</button>`
        ], `class="clickable" data-run="${esc(finding.review_run_id)}"`)).join("") : empty(7)}</tbody></table>
      </section>`;
    }

    function renderCommentsPage() {
      return `<section class="panel"><div class="panel-header"><div class="panel-title">◍ Comments</div><span>${state.comments.length} rows</span></div>
        <table><thead><tr><th>Created</th><th>Project</th><th>MR</th><th>Path</th><th>Rule</th><th>Discussion</th><th>Run ID</th></tr></thead><tbody>${state.comments.length ? state.comments.map((comment) => row([
          fmtTime(comment.created_at), esc(comment.project_id), `!${esc(comment.mr_iid)}`,
          `${esc(comment.path)}${comment.new_line ? `:${esc(comment.new_line)}` : ""}`,
          esc(comment.rule_id), esc(comment.discussion_id || comment.note_id || "-"),
          `<button class="link" data-run="${esc(comment.review_run_id)}">${esc(short(comment.review_run_id, 12))}</button>`
        ], `class="clickable" data-run="${esc(comment.review_run_id)}"`)).join("") : empty(7)}</tbody></table>
      </section>`;
    }

    function renderSystemPage() {
      return `<div class="content-grid"><section class="panel">${serviceStatusPanel()}</section><section class="panel">${findingSummaryPanel()}</section></div>`;
    }

    function runsTable(items) {
      return `<table><thead><tr><th>Started</th><th>Status</th><th>Project</th><th>MR</th><th>Commit</th><th>Tasks</th><th>Findings</th><th>Comments</th><th>Duration</th><th>Run ID</th></tr></thead><tbody>${items.length ? items.map((run) => {
        const totalTasks = run.total_task_runs || run.selected_ai_reviews + run.selected_script_tasks;
        const completedTasks = run.completed_task_runs || (run.status === "completed" ? totalTasks : 0);
        const findingColor = run.findings > 0 ? (run.status === "failed" ? "var(--red)" : "var(--amber)") : "var(--green)";
        return row([fmtTime(run.started_at), badge(run.status), esc(run.project_id), `!${esc(run.mr_iid)}`, `<code>${esc(short(run.commit_sha))}</code>`, `${completedTasks}/${totalTasks}`, `<span style="color:${findingColor}">${run.findings || "-"}</span>`, esc(run.comments), fmtMs(run.duration_ms), `<code>${esc(short(run.review_run_id, 12))}</code>`], `class="clickable" data-run="${esc(run.review_run_id)}"`);
      }).join("") : empty(10)}</tbody></table>`;
    }

    function projectsTable(items) {
      return `<table><thead><tr><th>Project</th><th>Runs</th><th>Running</th><th>Failed</th><th>Success Rate</th><th>Last Review</th></tr></thead><tbody>${items.length ? items.map((project) => {
        const rate = pct(project.total_runs - project.failed_runs, project.total_runs);
        return row([`<button class="link" data-project="${esc(project.project_id)}">${esc(project.project_id)}</button>`, esc(project.total_runs), esc(project.running_runs), esc(project.failed_runs), `${rate}% <span class="progress"><span style="width:${rate}%"></span></span>`, relative(project.last_review_at)], `class="clickable" data-project="${esc(project.project_id)}"`);
      }).join("") : empty(6)}</tbody></table>`;
    }

    function mrsTable(items) {
      return `<table><thead><tr><th>MR</th><th>Project</th><th>Status</th><th>Runs</th><th>Findings</th><th>Last Review</th></tr></thead><tbody>${items.length ? items.map((mr) => row([
        `<button class="link" data-project="${esc(mr.project_id)}" data-mr="${esc(mr.mr_iid)}">!${esc(mr.mr_iid)}</button>`, esc(mr.project_id), badge(mr.last_status), esc(mr.total_runs),
        mr.total_findings ? `<span style="color:var(--red)">${mr.total_findings}</span>` : `<span style="color:var(--green)">0</span>`, relative(mr.last_review_at)
      ], `class="clickable" data-project="${esc(mr.project_id)}" data-mr="${esc(mr.mr_iid)}"`)).join("") : empty(6)}</tbody></table>`;
    }

    function serviceStatusPanel() {
      const summary = state.summary || {};
      return `<section class="panel"><div class="panel-header"><div class="panel-title">✓ Service Status</div></div><div class="status-list">
        <div class="status-row"><span>Dashboard Service</span><span class="status-pill">Healthy</span></div>
        <div class="status-row"><span>Database</span><span class="status-pill">Healthy</span></div>
        <div class="status-row"><span>Last Review</span><span>${relative(summary.last_review_at)}</span></div>
        <div class="status-row"><span>Last Error</span><span>${summary.failed_runs > 0 ? `${summary.failed_runs} failed runs` : "-"}</span></div>
      </div></section>`;
    }

    function findingSummaryPanel() {
      const summary = state.findingSummary || { total: 0, error: 0, warning: 0, info: 0 };
      const total = summary.total || 0;
      const error = total ? (summary.error / total) * 360 : 0;
      const warning = error + (total ? (summary.warning / total) * 360 : 0);
      const info = warning + (total ? (summary.info / total) * 360 : 0);
      return `<section class="panel"><div class="panel-header"><div class="panel-title">◌ Findings Summary</div></div><div class="finding-body">
        <div class="donut" style="--error:${error}deg;--warning:${warning}deg;--info:${info}deg"><div class="donut-center"><div>${total}</div><span>Total</span></div></div>
        <div class="legend">
          <div class="legend-row"><span class="dot error"></span><span>Critical</span><span>${summary.error} (${pct(summary.error, total)}%)</span></div>
          <div class="legend-row"><span class="dot warning"></span><span>Major</span><span>${summary.warning} (${pct(summary.warning, total)}%)</span></div>
          <div class="legend-row"><span class="dot info"></span><span>Info</span><span>${summary.info} (${pct(summary.info, total)}%)</span></div>
        </div>
      </div></section>`;
    }

    async function openRunDetail(reviewRunId) {
      const detail = await json(`/api/runs/${encodeURIComponent(reviewRunId)}`);
      $("content").innerHTML = `<div class="detail-grid">
        <section class="panel"><div class="panel-header"><div class="panel-title">Review Run Detail</div><button class="link" id="backToRuns">Back to runs</button></div><div class="detail-list">
          <div class="detail-row"><span>Run ID</span><code>${esc(detail.run.review_run_id)}</code></div>
          <div class="detail-row"><span>Status</span>${badge(detail.run.status)}</div>
          <div class="detail-row"><span>Project / MR</span><span>${esc(detail.run.project_id)} / !${esc(detail.run.mr_iid)}</span></div>
          <div class="detail-row"><span>Commit</span><code>${esc(detail.run.commit_sha)}</code></div>
          <div class="detail-row"><span>Findings / Comments</span><span>${esc(detail.run.findings)} / ${esc(detail.run.comments)}</span></div>
        </div></section>
        <section class="panel"><div class="panel-header"><div class="panel-title">Tasks</div></div><table><thead><tr><th>Type</th><th>ID</th><th>Status</th><th>Findings</th><th>Comments</th><th>Error</th></tr></thead><tbody>${detail.tasks.length ? detail.tasks.map((task) => row([esc(task.task_type), esc(task.task_id), badge(task.status), esc(task.findings), esc(task.comments), `<span class="wrap">${esc(task.error || "-")}</span>`])).join("") : empty(6)}</tbody></table></section>
        <section class="panel"><div class="panel-header"><div class="panel-title">Findings</div></div><table><thead><tr><th>Severity</th><th>Path</th><th>Title</th><th>Message</th></tr></thead><tbody>${detail.findings.length ? detail.findings.map((finding) => row([badge(finding.severity), `${esc(finding.path)}${finding.new_line ? `:${esc(finding.new_line)}` : ""}`, esc(finding.title), `<span class="wrap">${esc(finding.message)}</span>`])).join("") : empty(4)}</tbody></table></section>
        <section class="panel"><div class="panel-header"><div class="panel-title">Comments</div></div><table><thead><tr><th>Rule</th><th>Path</th><th>Discussion</th><th>Note</th></tr></thead><tbody>${detail.comments.length ? detail.comments.map((comment) => row([esc(comment.rule_id), `${esc(comment.path)}${comment.new_line ? `:${esc(comment.new_line)}` : ""}`, esc(comment.discussion_id || "-"), esc(comment.note_id || "-")])).join("") : empty(4)}</tbody></table></section>
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
      $("content").innerHTML = `<section class="panel"><div class="empty">Load failed: ${esc(err.message)}</div></section>`;
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
