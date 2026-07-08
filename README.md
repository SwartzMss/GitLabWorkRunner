# GitLabWorkRunner

语言：**简体中文** | [English](README.en.md)

GitLabWorkRunner 是一个 Rust 编写的 GitLab Merge Request 手动 Review 服务。它通过 GitLab MR 评论触发 `[[ai_reviews]]` 或脚本任务，获取 MR 变更后把结果发布到 MR Discussion。

它不是 GitLab Runner 替代品，也不会自动执行目标仓库里的 CI 脚本；默认示例提供 AI Review 配置，并保留一个可选脚本任务示例。

## 工作原理

```mermaid
flowchart LR
    A["GitLab MR Comment Webhook"] --> B["GitLabWorkRunner"]
    B --> C["拉取 MR diff"]
    C --> D["解析新增行"]
    D --> F["AI Review"]
    D --> E["可选：脚本任务"]
    F --> H["生成评论"]
    E --> H
    H --> I["GitLab MR Discussion"]
    B --> J["SQLite 运行统计"]
```

手动 Review 的一次请求大致是：

```mermaid
sequenceDiagram
    participant GitLab
    participant Runner as GitLabWorkRunner
    participant DB as SQLite
    participant AI as OpenAI-compatible API

    GitLab->>Runner: MR note event with @id
    Runner->>DB: 记录 review_run_id 和运行状态
    Runner->>GitLab: 拉取 MR changes / diff refs
    Runner->>Runner: 构建 AI Review 输入并定位新增行
    Runner-->>AI: 发送 MR diff，可选附带只读上下文工具
    Runner->>GitLab: 发布行级或 MR 级评论
    Runner->>DB: 记录任务、findings、comments 和完成状态
```

更多设计细节见 [docs/design.md](docs/design.md)。

## 支持能力

- GitLab Merge Request Note Webhook 手动触发 Review。
- 只对 MR diff 的新增行发布行级评论。
- `[[ai_reviews]]`：调用 OpenAI-compatible `POST /chat/completions` 做 AI Review。
- 支持 Chat Completions `tool_calls` 结构化输出，以及内置只读上下文工具 `read_file`、`search_code`、`list_files`。
- MR 评论手动触发 AI Review，例如 `@ai-review`。
- 同一个 `project_id + mr_iid + commit_sha` 正在执行 review 时，会拦截重复触发；如果是 MR 评论触发，会给评论加 `eyes` 并回复“当前 commit 已有 review 正在执行”。
- 每次 review run、子任务、finding 和已发布评论都会写入 SQLite 结构化表，时间字段使用 RFC3339 UTC 字符串。
- 可选能力：`[[script_tasks]]` 下载 MR head 快照并执行本地脚本，默认不启用。

## 快速开始

准备配置文件：

```powershell
Copy-Item config.example.toml config.toml
Copy-Item rules.example.toml rules.toml
cargo run
```

Linux / macOS：

```bash
cp config.example.toml config.toml
cp rules.example.toml rules.toml
cargo run
```

在 GitLab 项目中添加 Webhook：

1. 进入 GitLab 项目，打开 `Settings` -> `Webhooks`。
2. `URL` 填写服务地址：

```text
http://<host>:8080/webhooks/gitlab
```

其中 `<host>` 是 GitLab 能访问到的 GitLabWorkRunner 地址。

3. `Secret token` 填写 `config.toml` 中 `[server].webhook_secret` 的值：

```toml
[server]
webhook_secret = "change-me"
```

4. 勾选 `Merge request events`。
5. 如果需要在 MR 评论里用 `@ai-review` 手动触发 AI Review，同时勾选 `Comments`。
6. 保存后可以使用 GitLab Webhook 页面里的 `Test` 功能发送测试事件。

Webhook 行为说明见 [docs/gitlab-webhook.md](docs/gitlab-webhook.md)。

## 构建

开发构建：

```bash
cargo build
```

发布/部署构建：

```bash
cargo build --release
```

构建产物：

```text
target/debug/gitlab-work-runner.exe      # Windows debug
target/release/gitlab-work-runner.exe    # Windows release
target/debug/gitlab-work-runner          # Linux / macOS debug
target/release/gitlab-work-runner        # Linux / macOS release
```

运行前仍需要准备 `config.toml` 和 `rules.toml`。

## 服务配置

`config.toml` 控制服务、GitLab、存储和规则文件：

```toml
[server]
bind = "0.0.0.0:8080"
webhook_secret = "change-me"
max_concurrent_reviews = 4

[gitlab]
base_url = "https://gitlab.example.com"
token = "<your-gitlab-token>"
api_timeout_seconds = 30
archive_timeout_seconds = 300

[storage]
database_url = "sqlite://gitlab-work-runner.db"

[rules]
file = "rules.toml"

[archive]
max_archive_bytes = 104857600      # 100 MiB
max_extracted_files = 10000        # 10,000 files
max_extracted_bytes = 209715200    # 200 MiB
max_single_file_bytes = 10485760   # 10 MiB
max_entry_path_bytes = 512         # 512 bytes

```

`[gitlab].token` 是服务调用 GitLab API 使用的 token，和 Webhook 里的 `Secret token` 不是同一个东西。建议使用 Project Access Token 或专用 Bot 用户 token，scope 使用 `api`，项目角色至少 `Developer`。它需要能读取 MR diff、下载仓库 archive，并发布 MR discussion。不要把包含真实 token 的 `config.toml` 提交到仓库。`api_timeout_seconds` 控制普通 GitLab API 请求超时，`archive_timeout_seconds` 单独控制 repository archive 下载超时；两者默认都是 `30` 秒。

`[server].max_concurrent_reviews` 控制单进程内最多同时执行多少个 review run，默认 `4`。达到上限时不会启动新的后台 review，并会发布一条 MR 级评论提示当前 review 队列繁忙，请稍后再试；如果是 MR 评论手动触发，服务还会给触发评论加 `eyes`。

`[archive]` 控制 GitLab repository archive 的下载和解压硬限制。超过 `max_archive_bytes` 会停止下载并让本次 review 失败；解压时超过 `max_extracted_files`、`max_extracted_bytes`、`max_single_file_bytes` 或 `max_entry_path_bytes` 会停止解压并让本次 review 失败。大仓库需要同时调大 `[archive].max_archive_bytes` 和 `[gitlab].archive_timeout_seconds`。失败会走现有 MR 失败通知，不会继续执行 AI context tools 或脚本任务。

## Dashboard

Dashboard 是独立二进制，不和 webhook runner 共用 HTTP 端口。runner 负责写 SQLite 统计表，dashboard 只读同一个数据库。

它默认读取同一个 `config.toml`：`[storage].database_url` 决定读取哪个 SQLite 数据库，`[dashboard].bind` 决定 dashboard HTTP 监听地址。不提供 `config.toml` 时会退回到本地默认值：监听 `127.0.0.1:8082`，读取当前目录的 `gitlab-work-runner.db`。

配置示例：

```toml
[storage]
database_url = "sqlite://gitlab-work-runner.db"

[dashboard]
bind = "127.0.0.1:8082"
```

启动：

```powershell
.\gitlab-work-runner-dashboard.exe
```

访问：

```text
http://127.0.0.1:8082/dashboard
```

API：

```text
GET /api/summary
GET /api/finding-summary
GET /api/runs
GET /api/runs?status=failed&project=group/project&mr_iid=2
GET /api/runs/<review_run_id>
GET /api/projects
GET /api/merge-requests
GET /api/findings
GET /api/comments
```

dashboard 进程不会执行 migration。如果数据库或统计表不存在，先启动一次 `gitlab-work-runner.exe` 完成迁移。

## AI Review 配置

当前只支持在 MR 评论里手动触发 review，不再支持 MR 更新后自动筛选执行。`rules.toml` 里需要保留 `[ai_review]` 和 `[[ai_reviews]]`；脚本任务可以保留配置，后续需要跑本地确定性检查时通过评论手动触发。

推荐 `rules.toml` 示例：

```toml
[ai_review]
# 可选：全局 AI Review prompt 配置，所有 [[ai_reviews]] 共用。
# system_prompt = "You are a careful code reviewer. Return only high-confidence bugs."
extra_instructions = """
重点关注编译错误、运行时错误、资源生命周期、线程安全和安全漏洞。
不要报告风格建议、命名建议或不确定的问题。
"""
max_tool_calls = 30
max_tool_result_bytes = 60000

[ai_review.context_tools]
read_file = false
search_code = false
list_files = false

[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "<your-ai-api-key>"
model = "gpt-4.1-mini"
timeout_seconds = 1200
request_timeout_seconds = 420
max_diff_bytes = 60000
second_pass_on_clean = false
batch_review = true
max_batch_diff_bytes = 15000
max_batches = 10
```

在 MR 评论中发送独立的 `@ai-review` 会触发 `id = "ai-review"` 的配置。MR 更新事件会被接收并忽略，不会进入 review 队列。
`[ai_review]` 是全局 AI Review prompt 配置：`system_prompt` 可以替换内置 system prompt，`extra_instructions` 会追加到用户 prompt。缺省时使用内置 prompt，不需要配置。
`[ai_review.context_tools]` 是进程内只读上下文工具配置，默认全部关闭。开启后，服务会下载 MR head archive，让模型可以通过 tool call 请求 `read_file`、`search_code` 或 `list_files`；runner 只返回仓库目录内的文本内容，不执行 shell，也不会读取 `.env` 或 `.git`。
`max_tool_calls` 默认是 `30`，`max_tool_result_bytes` 默认是 `60000`。如果 context tools 都关闭，不会额外下载 archive，也不会增加中间 AI 请求。
开启 context tools 后，日志会记录每次工具调用的工具名、参数摘要、返回 bytes、结果是否截断、是否触发调用上限、batch index/count 和累计 tool call 次数，便于确认模型是否真的调用了 `read_file`、`search_code` 或 `list_files`。
`request_timeout_seconds` 是单次 AI API 请求的超时；不配置时默认使用 `timeout_seconds / 2`，用于保留一次失败重试机会。
`second_pass_on_clean` 默认是 `false`；设置为 `true` 时，第一次 AI Review 没有发现问题会再执行一次确认。
AI Review 默认请求 Chat Completions `tool_calls` 结构化输出，并从 `submit_review_findings` 的 arguments 解析 findings；如果响应没有 tool call，会回退解析 `content` 中的 JSON。内置 context tools 不需要 MCP，也不会调用外部服务。
`batch_review` 默认是 `false`；设置为 `true` 时，会按完整文件 diff 分批调用 AI Review。`max_batch_diff_bytes` 控制单批 diff 字节上限，`max_batches` 控制最多请求批次数。
上面的示例是面向较大 MR 的推荐配置；代码缺省值仍保持保守，不写 `batch_review` 时不会自动分批，也不会增加额外 AI 请求。

内置 context tools 说明：

`read_file(path)` 读取 MR head checkout 中的一个 UTF-8 文本文件。

参数：

```json
{ "path": "src/lib.rs" }
```

返回：

```json
{
  "ok": true,
  "path": "src/lib.rs",
  "content": "file content...",
  "truncated": false
}
```

`search_code(query, glob?)` 在 MR head checkout 中搜索文本。`query` 是普通子串匹配，不是正则；`glob` 可选，用于限制文件范围。

参数：

```json
{ "query": "parse_config", "glob": "src/**/*.rs" }
```

返回：

```json
{
  "ok": true,
  "matches": [
    {
      "path": "src/config.rs",
      "line": 42,
      "before": "impl Config {",
      "text": "fn parse_config(...)",
      "after": "}"
    }
  ],
  "truncated": false
}
```

`list_files(glob?)` 列出 MR head checkout 中的文件。`glob` 可选，用于限制文件范围。

参数：

```json
{ "glob": "src/**/*.rs" }
```

返回：

```json
{
  "ok": true,
  "files": ["src/lib.rs", "src/config.rs"],
  "truncated": false
}
```

工具失败时统一返回：

```json
{ "ok": false, "error": "error message" }
```

所有工具都只接受仓库内相对路径；绝对路径、`..` 越界路径、`.env` 和 `.git` 会被拒绝或跳过。`read_file` 最多读取 `max_tool_result_bytes` 和 1 MiB 两者中的较小值，并按 UTF-8 字符边界截断。`search_code` 和 `list_files` 会跳过常见依赖/构建目录与 lock 文件，例如 `node_modules/`、`target/`、`dist/`、`vendor/`、`Cargo.lock`、`package-lock.json`。单个工具结果会按 `max_tool_result_bytes` 截断；`search_code` 最多返回 50 条匹配、每个文件最多 5 条，并跳过大于 1 MiB 的文件；`list_files` 最多返回 200 个文件。

不要把包含真实 `api_key` 的 `rules.toml` 提交到仓库。

`@ai-review` 匹配的是 `[[ai_reviews]]` 里的 `id = "ai-review"`。`[[ai_reviews]]` 只是配置块类型，不是触发命令。

### 可选：脚本任务

脚本任务仍然支持，但默认不启用。可以先保留配置，后续需要跑本地确定性检查时再打开：

```toml
[[script_tasks]]
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
```

`@check-todo-tbd` 匹配的是 `[[script_tasks]]` 里的 `id = "check-todo-tbd"`。

脚本会收到两个参数：

```text
<MR head source directory> <result.txt path>
```

当脚本返回 `exit 1` 时，服务读取 `result.txt`。推荐每行写成：

```text
src/config.rs:5: //TODO aa
```

## 手动触发

开启 GitLab Webhook 的 `Comments` 后，可以在 MR 评论中发送独立命令：

```text
@ai-review
```

同一个 commit 完成后可以再次触发。但如果同一个 `project_id + mr_iid + commit_sha` 仍在执行中，新的触发会被跳过：服务会给触发评论加 `eyes`，并回复一条 MR 评论提示当前 commit 已有 review 正在执行，请稍后再试。如果全局运行中的 review 数已经达到 `[server].max_concurrent_reviews`，也会跳过本次触发并回复队列繁忙提示。

如果你配置了可选脚本任务，也可以用对应 id 手动触发：

```text
@check-todo-tbd
```

当前实现不会额外校验评论人的 GitLab 角色；只要用户能在 MR 评论，并且评论内容包含合法的 `@id`，服务就会执行对应手动任务。如果需要限制只有 Maintainer 或指定用户可以触发，需要在服务侧增加权限校验或 allowlist。

## 工作目录清理

GitLab archive zip 下载后只保存在内存里，不会作为 zip 文件写入磁盘。需要仓库上下文时，服务会把 archive 解压到 `work/` 下：

- AI context tools：`work/ai_review_context/.../<review_run_id>/source`
- 脚本任务：`work/script_tasks/.../<task_id>/source`

正常完成或失败后，AI Review 会删除本次 context run 目录；脚本任务会删除 `source` 目录，但保留 `run.log` 和 `result.txt` 便于排查。服务启动时会清理一次超过 24 小时的残留目录，运行中也会每小时清理一次。清理失败只写 WARN，不会阻断 review。

当前运行中去重和全局并发上限都是单进程内的；多实例部署时，如果需要跨进程互斥、全局并发限制和全局清理，需要把运行中锁迁移到 SQLite/PostgreSQL。

## 失败通知

如果整个 review run 因不可恢复错误失败，例如拉取 MR diff、下载 archive、调用 GitLab API 或内部处理失败，服务会向 MR 发布一条 MR 级失败评论，内容包含 `commit`、`review_run_id` 和截断后的错误摘要。失败通知本身发送失败时只写 WARN，不会继续重试。

如果某个 AI review 子任务失败，但其它 AI review 或脚本任务仍可继续执行，服务会在本次 run 结束前发布一条 MR 级“部分 AI Review 失败”汇总评论，列出失败的 `ai_review.id/title`，并带上 `commit` 和 `review_run_id`，提示查看 runner 日志。

## 更多文档

- [docs/design.md](docs/design.md)：设计和模块边界。
- [docs/gitlab-webhook.md](docs/gitlab-webhook.md)：GitLab Webhook 配置和触发行为。
- [rules.example.toml](rules.example.toml)：完整规则示例。
- [examples/scripts/check_todo_tbd.py](examples/scripts/check_todo_tbd.py)：可选脚本任务示例。

## 许可证

MIT，见 [LICENSE](LICENSE)。
