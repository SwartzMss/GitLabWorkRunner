# GitLab MR 自动 Review 服务设计

## 目标

用 Rust 实现一个 GitLab 专用的 Merge Request 自动 Review 服务。第一版不做通用 CI Runner，也不复刻完整 reviewdog CLI，而是先跑通一个稳定闭环：

1. GitLab Webhook 触发 MR review。
2. 服务校验事件并做去重。
3. 通过 GitLab API 获取 MR diff。
4. 解析 diff，建立文件、hunk、行号映射。
5. 执行配置文件定义的规则。
6. 在 GitLab MR Discussion 中发布行级或 MR 级评论。
7. 记录已处理 commit、评论 note id 和 MR 状态，避免重复评论。

## 非目标

第一版不实现以下能力：

- 不实现完整 GitLab Runner 协议。
- 不执行用户仓库中的 CI 脚本。
- 不支持 GitHub、Bitbucket、Gitea 等其他平台。
- 不兼容 reviewdog 的所有输入格式、reporter 和 linter adapter。
- 不做容器沙箱、任务隔离、分布式调度。
- 不接入 LLM 自动审查。后续可以作为规则插件扩展。

## 推荐方案

采用“Webhook 实时触发 + GitLab API 获取 Diff + SQLite 状态存储 + 配置化规则引擎 + GitLab MR Discussion 输出”的服务化架构。

这个方案比内置写死规则更灵活，也比第一版直接做插件系统更可控。规则先放在 `rules.toml` 中，通过路径匹配、正则匹配和提示模板完成基础 review。

## 系统架构

```text
GitLab Merge Request Event
  -> Webhook Server
  -> Event Scheduler / Deduplicator
  -> GitLab API Client
  -> Diff Fetcher
  -> Diff Parser
  -> Rule Engine
  -> Comment Builder
  -> GitLab Discussion Publisher
  -> State Store
```

## 模块设计

### Webhook Server

职责：

- 暴露 HTTP endpoint，例如 `POST /webhooks/gitlab`。
- 校验 `X-Gitlab-Token`。
- 只接受 Merge Request event。
- 从 payload 中提取 `project_id`、`merge_request.iid`、source branch、target branch、last commit sha、event action。
- 返回快速响应，不在 HTTP 请求中长时间执行 review。

第一版可以同步执行 review，但代码边界上要保留异步任务调度入口，便于后续接入队列。

### Event Scheduler / Deduplicator

职责：

- 判断当前 MR commit 是否已经处理。
- 对同一个 `project_id + mr_iid + commit_sha` 保证幂等。
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

- 使用环境变量 `GITLAB_TOKEN`。
- token 需要 `read_api` 和 `api` 权限。

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

第一版规则只对 `Added` 行运行，避免对历史代码产生大量噪音。

### Rule Engine

职责：

- 读取 `rules.toml`。
- 对 diff 文件路径和新增行内容执行匹配。
- 生成 review finding。

第一版规则格式：

```toml
[[rules]]
id = "forbid-unwrap"
title = "避免直接 unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "这里直接使用 unwrap 可能导致运行时 panic，建议改成错误传播或显式处理。"
```

规则字段：

- `id`: 规则唯一标识。
- `title`: 评论标题。
- `severity`: `info`、`warning`、`error`。
- `path`: glob 路径匹配。
- `pattern`: 正则表达式。
- `message`: 评论内容。

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

### Comment Builder

职责：

- 将 finding 转换成 GitLab 评论正文。
- 对同一个文件同一行多个 finding 做合并。
- 为评论加入稳定标记，方便识别服务自己发过的评论。

评论正文示例：

```markdown
**[warning] 避免直接 unwrap**

这里直接使用 unwrap 可能导致运行时 panic，建议改成错误传播或显式处理。

<!-- gitlab-work-runner:rule=forbid-unwrap -->
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

## 配置设计

服务配置使用 `config.toml`：

```toml
[server]
bind = "0.0.0.0:8080"
webhook_secret = "change-me"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite://gitlab-work-runner.db"

[rules]
file = "rules.toml"
```

规则配置使用 `rules.toml`，第一版只支持正则匹配新增行。后续可以扩展成多种 rule kind。

## 错误处理

- Webhook token 不匹配：返回 `401 Unauthorized`。
- 非 MR event：返回 `202 Accepted` 并忽略。
- payload 缺少关键字段：返回 `400 Bad Request`。
- GitLab API 失败：记录 review 状态为 `failed`，返回可观测日志。
- diff 解析失败：跳过单个文件，继续处理其他文件，并记录 warning。
- 评论发布失败：保留 finding 和失败原因，避免整个任务静默成功。
- 重复事件：返回 `202 Accepted`，不重复评论。

## 测试策略

第一版需要覆盖：

- GitLab webhook payload 解析。
- token 校验。
- 去重 key 生成。
- unified diff parser 的新增行、删除行、上下文行号映射。
- rules.toml 解析和正则匹配。
- finding 到 GitLab discussion payload 的转换。
- SQLite 状态写入和重复处理。

集成测试可以使用 mock GitLab API server，验证完整流程：

```text
Webhook payload -> diff fixture -> rule finding -> discussion API request -> state store
```

## 第一版交付边界

第一版完成后，应能做到：

1. 本地启动 HTTP 服务。
2. 配置 GitLab webhook。
3. MR 更新后自动触发 review。
4. 服务拉取 MR diff。
5. 根据 `rules.toml` 对新增行执行规则。
6. 在 MR 中发布行级评论。
7. 对同一 commit 和同一规则集不重复评论。

## 后续扩展

- 支持规则插件。
- 支持 LLM review provider。
- 支持 MR 级汇总评论。
- 支持 Redis / PostgreSQL。
- 支持任务队列和并发 worker。
- 支持定时轮询补偿。
- 支持删除或 resolve 旧评论。
- 支持更完整的 reviewdog diff 兼容层。
