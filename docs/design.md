# GitLab MR 自动 Review 服务设计

## 目标

用 Rust 实现一个 GitLab 专用的 Merge Request 自动 Review 服务。第一版不做通用 CI Runner，也不复刻完整 reviewdog CLI，而是先跑通一个稳定闭环：

1. GitLab Webhook 触发 MR review。
2. 服务校验事件并做去重。
3. 通过 GitLab API 获取 MR diff。
4. 解析 diff，建立文件、hunk、行号映射。
5. 执行配置文件定义的 AI Review 和可选脚本任务。
6. 在 GitLab MR Discussion 中发布行级或 MR 级评论。
7. 记录已处理 commit、评论 note id 和 MR 状态，避免重复评论。

## 非目标

第一版不实现以下能力：

- 不实现完整 GitLab Runner 协议。
- 不自动执行用户仓库中的 CI 脚本；只执行 `rules.toml` 显式配置的脚本任务。
- 不支持 GitHub、Bitbucket、Gitea 等其他平台。
- 不兼容 reviewdog 的所有输入格式、reporter 和 linter adapter。
- 不做容器沙箱、任务隔离、分布式调度。
- 不提供通用 LLM 插件系统；当前只通过 `[[ai_reviews]]` 支持 OpenAI-compatible AI Review。

## 推荐方案

采用“Webhook 实时触发 + GitLab API 获取 Diff + SQLite 状态存储 + 配置化 Review 任务 + GitLab MR Discussion 输出”的服务化架构。

这个方案比直接做插件系统更可控。AI Review 和脚本任务都放在 `rules.toml` 中：AI Review 调用 OpenAI-compatible API；复杂检查可以通过独立的 `script_tasks` 下载 MR head 快照后执行。脚本任务默认可以保持 `auto_enabled = false`，后续需要时再启用或手动触发。

## 系统架构

```text
GitLab Merge Request / MR Note Event
  -> Webhook Server
  -> Event Scheduler / Deduplicator
  -> GitLab API Client
  -> Diff Fetcher
  -> Diff Parser
  -> Review Engine
  -> Comment Builder
  -> GitLab Discussion Publisher
  -> State Store
```

## 模块设计

### Webhook Server

职责：

- 暴露 HTTP endpoint，例如 `POST /webhooks/gitlab`。
- 校验 `X-Gitlab-Token`。
- 接受 Merge Request event 和 MR comment note event。
- 从 MR event payload 中提取 `project_id`、`merge_request.iid`、source branch、target branch、last commit sha、event action。
- 从 MR note event payload 中提取 `project_id`、`merge_request.iid`、note id、note 内容和 last commit sha。
- 返回快速响应，不在 HTTP 请求中长时间执行 review。

第一版可以同步执行 review，但代码边界上要保留异步任务调度入口，便于后续接入队列。

### Event Scheduler / Deduplicator

职责：

- 判断当前 MR commit 是否已经处理。
- 对同一个 `project_id + mr_iid + commit_sha` 保证幂等。
- 对单进程内运行中的 review run 做全局并发限制。
- 支持可选轮询补偿任务，后续用于处理 Webhook 漏事件。

第一版去重标准：

```text
project_id + mr_iid + commit_sha + ruleset_hash
```

加入 `ruleset_hash` 是为了规则变更后可以重新 review 同一个 commit。

### GitLab API Client

职责：

- 封装 GitLab REST API。
- 获取 MR changes 或 diff refs。
- 创建 MR discussion。
- 更新标签或 MR 状态信息时保留扩展入口。

第一版需要的 API：

- `GET /projects/:id/merge_requests/:merge_request_iid/changes`
- `POST /projects/:id/merge_requests/:merge_request_iid/discussions`

认证方式：

- 使用 `config.toml` 中的 `[gitlab].token`。
- token 需要 `api` scope。

### Diff Fetcher

职责：

- 从 GitLab API 返回的 MR changes 中提取每个文件的 diff。
- 记录 `old_path`、`new_path`、`new_file`、`renamed_file`、`deleted_file`。
- 将 GitLab 返回结构转换成内部统一 diff model。

第一版只处理文本 diff。二进制文件、删除文件可以跳过并记录日志。

### Diff Parser

职责：

- 解析 unified diff。
- 建立 hunk 和行号映射。
- 支持定位新增行，用于发布 GitLab 行级评论。

核心数据结构：

```rust
struct DiffFile {
    old_path: String,
    new_path: String,
    hunks: Vec<DiffHunk>,
}

struct DiffHunk {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    lines: Vec<DiffLine>,
}

struct DiffLine {
    kind: DiffLineKind,
    old_line: Option<u32>,
    new_line: Option<u32>,
    content: String,
}

enum DiffLineKind {
    Context,
    Added,
    Removed,
}
```

AI Review 和脚本任务只对当前 MR diff 中能够定位的新增行发布行级评论，避免对历史代码产生噪音。

### Review Finding

AI Review 和脚本任务都会转换成统一的 finding：

匹配结果结构：

```rust
struct Finding {
    rule_id: String,
    severity: Severity,
    path: String,
    new_line: Option<u32>,
    title: String,
    message: String,
}
```

### Script Task Runner

职责：

- 读取 `rules.toml` 中的 `[[script_tasks]]`。
- 自动触发时，只选择 `auto_enabled = true` 且匹配 `when_changed` 的任务。
- 手动触发时，根据 MR 评论中的 `@task_id` 精确选择任务，允许执行 `auto_enabled = false` 的任务，并忽略 `when_changed`。
- 通过 GitLab archive API 下载 MR 当前 head commit。
- 解压到 `work/script_tasks/<project_id>/<mr_iid>/<commit_sha>/<task_id>/source`。
- 在 runner 可执行文件所在目录执行 `command`，相对脚本路径不绑定目标 GitLab 仓库。
- 将 stdout 和 stderr 合并写入 `run.log`，用于查看脚本运行过程。
- 将 `result.txt` 路径作为第二个参数传给脚本，脚本将检测结果写入该文件。
- 由 Rust 进程控制 timeout，超时后 kill 子进程。
- 任务完成后删除 `source/`，保留 `run.log` 和 `result.txt` 便于排查。
- `exit 0` 表示检测通过；`exit 1` 表示检测发现问题并发布 MR 评论；其他退出码、无退出码或 timeout 表示脚本执行异常，只记录日志并保留 `run.log` / `result.txt`。

第一版脚本任务格式：

```toml
[[script_tasks]]
auto_enabled = true
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.c", "**/*.cc", "**/*.cpp", "**/*.h", "**/*.hpp", "**/*.rs"]
```

字段说明：

- `auto_enabled`: 单条任务自动触发开关，默认 `true`；`false` 时仍可通过 MR 评论 `@id` 手动触发。
- `id`: 任务唯一标识。
- `title`: MR 评论标题。
- `command`: 在 runner 可执行文件所在目录执行的命令；相对路径基于该目录解析。
- `timeout_seconds`: 超时时间，默认 60 秒。
- `when_changed`: 可选 glob 列表；为空时每个 MR 都执行。

脚本输出协议：

- `exit 0`: 检测通过。
- 第一个参数: MR head 代码快照根目录。
- 第二个参数: `result.txt` 路径。
- stdout/stderr: 运行过程日志，写入 `run.log`。
- `result.txt`: 检测结果摘要；推荐每条结果使用 `仓库相对路径:行号:提示内容`。
- `exit 0`: 检测通过。
- `exit 1`: 检测发现问题，读取 `result.txt` 并发布 MR 评论。
- 其他退出码或无退出码: 脚本执行异常，保留 `run.log` / `result.txt`，不发 MR 评论。
- timeout: 脚本执行异常，kill 子进程，保留 `run.log` / `result.txt`，不发 MR 评论。

行级结果格式示例：

```text
src/config.rs:5: //TODO aa
```

服务会按 `path:line:message` 解析每一行，路径会按 GitLab diff 路径处理。能解析且当前 MR diff refs 完整时，发布到对应代码行；无法解析时，发布一条 MR 级汇总评论。自动 MR Review 在 GitLab 返回的 diff refs 不完整时会整体跳过，并发布一条 MR 级跳过提示，因此不会继续执行自动脚本任务；手动触发脚本任务在 diff refs 不完整但 archive 可下载时仍会执行，发现问题后以 MR 级汇总评论发布。第一版不提供 Python helper，也不要求 JSON 输出。

手动触发规则：

- GitLab Webhook 需要开启 `Comments`。
- 只处理 MR comment event，不处理 issue、wiki、work item comment。
- 评论正文中出现独立 token `@task_id` 时触发对应 `script_tasks.id`。
- 评论正文中出现独立 token `@ai_review_id` 时触发对应 `ai_reviews.id`。
- 手动触发不写入自动 review 去重记录；用户每发一次合法命令，服务都会执行一次。
- 如果 `@task_id` 不存在，服务只记录日志，不发布额外评论。

### Comment Builder

职责：

- 将 finding 转换成 GitLab 评论正文。
- 对同一个文件同一行多个 finding 做合并。
- 为评论加入稳定标记，方便识别服务自己发过的评论。

评论正文示例：

```markdown
**[警告] AI finding**

Review finding.

<!-- gitlab-work-runner:rule=ai:ai-review -->
```

### GitLab Discussion Publisher

职责：

- 优先发布行级 discussion。
- 当 GitLab position 无法定位时，降级为 MR 级普通评论。
- 记录 GitLab 返回的 discussion id 和 note id。

第一版行级评论需要依赖 GitLab position 参数，包括：

- `base_sha`
- `start_sha`
- `head_sha`
- `old_path`
- `new_path`
- `new_line`

这些字段来自 MR diff refs 和 diff parser 的行号映射。

### Review Notifier

职责：

- 统一封装后台 review 运行态通知。
- 给手动触发评论追加确认 emoji。
- 发布重复运行、队列繁忙、review 失败等 MR 级状态评论。
- 统一维护状态评论正文和隐藏 marker，避免通知逻辑散落在 Webhook Server 或 Review Service 中。

### State Store

职责：

- 记录已处理 MR commit。
- 记录已发布评论。
- 支持任务幂等和后续清理。

第一版使用 SQLite，后续可以抽象到 PostgreSQL。

核心表：

```sql
create table processed_reviews (
    id integer primary key autoincrement,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    ruleset_hash text not null,
    status text not null,
    created_at text not null,
    updated_at text not null,
    unique(project_id, mr_iid, commit_sha, ruleset_hash)
);

create table review_comments (
    id integer primary key autoincrement,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    ruleset_hash text not null,
    rule_id text not null,
    path text not null,
    new_line integer,
    discussion_id text,
    note_id integer,
    created_at text not null
);
```

### Work Directory Cleanup

GitLab archive zip 只保存在内存中，不落盘保存。需要仓库上下文时，服务会把 archive 解压到 `work/`：

- AI context tools 解压到 `work/ai_review_context/.../<review_run_id>/source`，review 完成或失败后删除本次 context run 目录。
- 脚本任务解压到 `work/script_tasks/.../<task_id>/source`，任务结束后删除 `source`，保留 `run.log` 和 `result.txt` 便于排查。

进程启动时会清理一次超过 24 小时的残留目录，运行中每小时再清理一次。清理失败只记录 WARN，不阻断 review。当前运行中去重和清理都是单进程内防护；多实例部署时需要把运行中锁迁移到 SQLite/PostgreSQL。

## 配置设计

服务配置使用 `config.toml`：

```toml
[server]
bind = "0.0.0.0:8080"
webhook_secret = "change-me"
max_concurrent_reviews = 4

[gitlab]
base_url = "https://gitlab.example.com"
token = "<your-gitlab-token>"

[storage]
database_url = "sqlite://gitlab-work-runner.db"

[rules]
file = "rules.toml"

[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

Review 配置使用 `rules.toml`。当前支持两类配置：`[[ai_reviews]]` 调用 OpenAI-compatible API、`[[script_tasks]]` 下载 MR head 快照后执行外部脚本。脚本任务使用同一个 `rules.toml`，但作为独立任务执行；它的输出也会转换成统一 Finding 后发布。

## 错误处理

- Webhook token 不匹配：返回 `401 Unauthorized`。
- 非 MR event：返回 `202 Accepted` 并忽略。
- payload 缺少关键字段：返回 `400 Bad Request`。
- GitLab API 失败：记录错误日志，当前 webhook 请求返回 `500 Internal Server Error`；不会写入已处理状态。
- GitLab diff refs 不完整：自动 MR Review 发布 MR 级跳过提示并标记为 skipped；手动 AI Review 跳过；手动脚本任务仍可执行，但发现问题时只能发布 MR 级汇总评论。
- diff 解析失败：当前 review 失败并返回 `500 Internal Server Error`。
- 评论发布失败：当前 review 失败并返回 `500 Internal Server Error`，错误原因写入日志。
- 后台 review run 不可恢复失败：发布一条 MR 级失败通知，包含 `commit`、`review_run_id` 和截断错误摘要；失败通知发送失败只记录 WARN。
- 全局 review 并发达到 `[server].max_concurrent_reviews`：返回 `202 Accepted` 并跳过本次 review；发布 MR 级队列繁忙提示，MR 评论触发时额外给触发评论加 `eyes`。
- 重复事件：返回 `202 Accepted`，不重复评论。
- 日志文件超过大小限制：轮转当前日志文件，并最多保留配置数量的历史文件。
- 脚本任务超时：kill 子进程，保留 `run.log` / `result.txt`，不发布 MR 评论。

## 测试策略

第一版需要覆盖：

- GitLab webhook payload 解析。
- token 校验。
- 去重 key 生成。
- unified diff parser 的新增行、删除行、上下文行号映射。
- rules.toml 解析、AI Review 配置选择和脚本任务选择。
- AI Review 配置解析、自动/手动选择、OpenAI-compatible 响应解析和新增行过滤。
- finding 到 GitLab discussion payload 的转换。
- script task 的 archive 下载、解压、输出文件和 timeout 处理。
- SQLite 状态写入和重复处理。

集成测试可以使用 mock GitLab API server，验证完整流程：

```text
Webhook payload -> diff fixture -> AI finding -> discussion API request -> state store
```

## 第一版交付边界

第一版完成后，应能做到：

1. 本地启动 HTTP 服务。
2. 配置 GitLab webhook。
3. MR 更新后自动触发 review。
4. 服务拉取 MR diff。
5. 根据 `rules.toml` 执行 AI Review 和可选脚本任务。
6. 在 MR 中发布行级评论。
7. 对同一 commit 和同一规则集不重复评论。
8. 将完整 Review 流程写入 stdout 和日志文件，并按大小轮转日志文件。
9. 可选执行 `ai_reviews`；AI 返回的 finding 只发布到当前 MR diff 的新增行。
10. 可选执行 `script_tasks`；`exit 1` 发布脚本结果评论，执行异常或超时时只记录日志并保留 `run.log` / `result.txt`。

## 后续扩展

- 支持更多 AI Review API 适配方式。
- 支持更完整的 MR 级汇总评论。
- 支持 Redis / PostgreSQL。
- 支持任务队列和并发 worker。
- 支持定时轮询补偿。
- 支持删除或 resolve 旧评论。
- 支持更完整的 reviewdog diff 兼容层。
- 支持脚本任务 JSON 输出协议和更精细的结果定位。
