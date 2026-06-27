# GitLabWorkRunner

语言：**简体中文** | [English](README.en.md)

GitLabWorkRunner 是一个使用 Rust 编写的 GitLab Merge Request 自动 Review 服务。

它不是完整的 GitLab Runner 替代品，也不会自动执行仓库里的 CI 脚本。服务只执行 `rules.toml` 中显式配置的 Review 规则和脚本任务。

1. 接收 GitLab Merge Request Webhook。
2. 通过 GitLab API 拉取 MR diff。
3. 解析 unified diff 的 hunk 和新增行位置。
4. 根据 `rules.toml` 执行可配置规则。
5. 向 GitLab MR Discussion 发布评论。
6. 使用 SQLite 记录已处理 commit，避免重复评论。

## 项目方向

这个项目参考了 reviewdog 在 diff 解析和行级定位上的思路，但范围更窄：

- 只支持 GitLab。
- 由 Webhook 触发。
- 使用 `rules.toml` 配置 Review 规则。
- 第一版使用 SQLite 存储状态。
- 输出目标是 GitLab MR Discussion，而不是多平台 reporter。

详细设计见 [docs/design.md](docs/design.md)。

## 架构

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

## 第一版能力

当前第一版已经支持：

- 启动本地 HTTP 服务。
- 接收 GitLab Merge Request Webhook。
- 校验 Webhook secret token。
- 从 GitLab 拉取 MR changes。
- 对新增行执行正则规则。
- 可按配置调用 OpenAI-compatible API 执行 AI Review。
- 可按配置下载 MR head 快照并执行独立脚本任务。
- 发布行级 MR 评论。
- 对相同 commit 和规则集做去重。
- 将完整 Review 流程日志写入 stdout 和日志文件。
- 按大小轮转日志文件，避免单个日志文件无限增长。

## 配置

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

[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

规则配置使用 `rules.toml`：

```toml
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Direct unwrap can panic at runtime. Prefer explicit error handling."

[[script_tasks]]
enabled = false
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.c", "**/*.cc", "**/*.cpp", "**/*.h", "**/*.hpp", "**/*.rs"]

[[ai_reviews]]
enabled = false
id = "ai-review"
title = "AI Review"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1-mini"
trigger = "auto_and_manual"
timeout_seconds = 60
max_diff_bytes = 60000
when_changed = ["**/*.rs", "**/*.toml"]
```

### AI Review

`[[ai_reviews]]` 是原生 AI Review 配置，独立于 `[[rules]]` 和 `[[script_tasks]]`。当前支持 OpenAI-compatible 的 `POST /chat/completions` API。

执行规则：

- `enabled` 不写时默认为 `true`，只控制自动触发。
- `trigger` 支持 `auto`、`manual`、`auto_and_manual`，默认 `auto_and_manual`。
- 自动触发要求 `enabled = true`、`trigger` 允许自动执行，并且 `when_changed` 为空或匹配变更文件。
- 手动触发使用 MR 评论中的独立命令 token，例如 `@ai-review`。
- 手动触发会忽略 `enabled` 和 `when_changed`，但 `trigger` 必须允许 `manual`。
- `api_key_env` 指定 AI API token 的环境变量名；token 不会写入日志。
- 服务只把 GitLab MR diff 发给 AI，不下载完整仓库。
- `max_diff_bytes` 控制发送给 AI 的 diff 文本上限，默认 `60000`。
- AI 返回的结果只会发布到当前 MR diff 的新增行；不在新增行上的结果会被过滤并写日志。
- AI 调用失败、超时、非 2xx 或返回 JSON 无法解析时，不会阻断正则规则和脚本任务。

AI 服务应返回 OpenAI-compatible chat completion，并在 assistant message 的 `content` 中返回严格 JSON：

```json
{
  "findings": [
    {
      "path": "src/lib.rs",
      "line": 42,
      "severity": "warning",
      "title": "Possible panic",
      "message": "This unwrap can panic when the value is absent."
    }
  ]
}
```

本地运行前需要设置 AI token，例如：

```bash
export OPENAI_API_KEY="<your-ai-api-key>"
```

### 脚本任务

`[[script_tasks]]` 是独立任务，不影响现有 `[[rules]]` 行级检查。每条任务单独配置 `enabled`，不提供总开关。

执行规则：

- `enabled` 不写时默认为 `true`。
- `enabled = true` 时，MR 创建或更新会按 `when_changed` 自动执行。
- `enabled = false` 时，不自动执行；但可以在 MR 评论里手动发送 `@任务id` 触发，例如 `@check-todo-tbd`。
- 手动触发会忽略 `enabled` 和 `when_changed`，只按 `id` 精确选择脚本任务。
- 手动触发不做去重；用户每发一次合法命令，服务就执行一次。
- `when_changed` 不写或为空时，自动触发会对每个 MR 执行。
- 服务固定下载 MR 当前 head commit 的 archive。
- 命令在 runner 可执行文件所在目录执行；`command` 中的相对路径也基于这个目录解析。
- stdout 和 stderr 合并写入 `run.log`，用于查看脚本运行过程。
- 服务会把 `result.txt` 路径作为第二个参数传给脚本，脚本应把检测结果写入这个文件。
- `exit 0` 表示检测通过。
- `exit 1` 表示检测发现问题，服务会读取 `result.txt` 并发布 MR 评论。
- 其他退出码、无退出码或超时表示脚本执行异常。
- 执行异常和超时不会发 MR 评论；只记录服务日志并保留 `run.log` / `result.txt`。
- 超时由 Rust 进程控制，默认 `timeout_seconds = 60`。
- 服务会把本次要检查的 MR head 代码快照根目录作为第一个参数追加到命令末尾。

`result.txt` 支持简单的行级评论格式：

```text
src/config.rs:5: //TODO aa
```

每一行按 `仓库相对路径:行号:提示内容` 解析。能解析的结果会尽量发布到对应代码行；无法解析成该格式，或当前 MR diff refs 不完整时，会发布一条 MR 级汇总评论。脚本可以在 `result.txt` 里保留标题行，例如 `Found //TODO or //TBD markers:`，这类行不会被当成行级结果。

工作目录：

```text
work/script_tasks/<project_id>/<mr_iid>/<commit_sha>/<task_id>/
  run.log
  result.txt
```

执行完成后会删除解压出的 `source/` 目录，只保留 `run.log` 和 `result.txt` 便于排查。脚本任务会移除配置中的 GitLab token 环境变量，避免脚本直接继承服务 token。

仓库提供了一个最小脚本示例：[examples/scripts/check_todo_tbd.py](examples/scripts/check_todo_tbd.py)。它读取第一个参数作为检查目录，第二个参数作为结果文件路径；过程日志写 stdout，检测结果按 `path:line:message` 写入 `result.txt`。

注意：`command = "python examples/scripts/check_todo_tbd.py"` 中的相对路径是相对于 runner 可执行文件所在目录的。如果使用 release 包里的示例脚本，保持这个路径即可；如果脚本放在其他目录，可以改成绝对路径。Windows 上如果返回退出码 `9009`，通常表示命令不存在，需要把 Python 加入 `PATH`。

手动触发需要 GitLab Webhook 同时开启 `Comments` 和 `Merge request events`。服务只处理 MR 评论中的独立命令 token，例如：

```text
@check-todo-tbd
```

也可以写在多行评论中：

```text
请帮我跑一下：
@check-todo-tbd
```

## 本地运行

Windows PowerShell：

```powershell
Copy-Item config.example.toml config.toml
Copy-Item rules.example.toml rules.toml
$env:GITLAB_TOKEN = "<your-token>"
cargo run
```

Linux / macOS：

```bash
cp config.example.toml config.toml
cp rules.example.toml rules.toml
export GITLAB_TOKEN="<your-token>"
cargo run
```

在 GitLab 项目中配置 Webhook：

- URL: `http://<host>:8080/webhooks/gitlab`
- Secret token: `[server].webhook_secret` 的值
- Trigger: `Merge request events`；如果需要 MR 评论手动触发脚本任务或 AI Review，同时开启 `Comments`

关于 `Merge request events`、`Comments` 的触发时机和 payload 字段，见 [GitLab Webhook 说明](docs/gitlab-webhook.md)。

## 日志

服务会同时输出日志到 stdout 和配置的日志文件：

```toml
[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

可以通过 `RUST_LOG` 控制日志级别：

```powershell
$env:RUST_LOG = "info"
cargo run
```

每次 Merge Request Webhook 的日志流程包含：

- 收到 Webhook 和 payload 大小。
- Webhook token 校验失败原因。
- 解析出的 `project_id`、`mr_iid`、`commit_sha`、action、source branch、target branch。
- Review 开始和 `ruleset_hash`。
- 相同 commit / ruleset 的跳过决策。
- GitLab MR changes 拉取开始和完成。
- AI Review provider、model、调用结果和 finding 数量。
- script task archive 下载、执行、超时和输出文件路径。
- diff refs：`base_sha`、`start_sha`、`head_sha`。
- changed file 数量。
- 每个文件的 path、hunk 数、finding 数、new/renamed/deleted 状态。
- 总 finding 数和 comment draft 数。
- 每条评论发布的 path 和 line number。
- GitLab 返回的 discussion id 和 note id。
- GitLab 拒绝行级 position 时，降级为 MR 级评论。
- 最终 Review 汇总：skipped、finding count、comment count。

GitLab token 和 Webhook secret 不会被写入日志。

### 日志轮转

服务内置按大小轮转日志文件。默认配置为：

```toml
[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

当当前日志文件在下一次写入时会超过 `max_bytes`，服务会先执行轮转：

- 当前文件重命名为 `gitlab-work-runner.log.1`。
- 旧的 `.1` 依次移动为 `.2`，直到 `max_files`。
- 最多保留 `max_files` 个历史文件。
- `max_files = 0` 时不保留历史文件，只重新创建当前日志文件。

这是内置的大小轮转，不包含按时间轮转、压缩、上传或集中采集。生产环境如果已经接入容器运行时、日志平台、`logrotate` 或 Windows 日志采集系统，也可以继续由外部系统统一管理。

## 许可证

本项目使用 MIT License，详情见 [LICENSE](LICENSE)。
