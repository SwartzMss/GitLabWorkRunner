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
command = "python3 examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.c", "**/*.cc", "**/*.cpp", "**/*.h", "**/*.hpp", "**/*.rs"]
```

### 脚本任务

`[[script_tasks]]` 是独立任务，不影响现有 `[[rules]]` 行级检查。每条任务单独配置 `enabled`，不提供总开关。

执行规则：

- `enabled` 不写时默认为 `true`。
- `when_changed` 不写或为空时，每个 MR 都执行。
- 服务固定下载 MR 当前 head commit 的 archive。
- 命令在解压后的 MR head 仓库根目录执行，也就是脚本检查的代码快照。
- stdout 和 stderr 合并写入一个 `output.log`。
- `exit 0` 表示通过，不发评论。
- `exit != 0` 或超时表示失败，发一条 MR 级评论。
- 超时由 Rust 进程控制，默认 `timeout_seconds = 60`。
- 运行时会注入 `GITLAB_WORK_RUNNER_CHECK_ROOT`，值为本次要检查的 MR head 代码快照根目录。

工作目录：

```text
work/script_tasks/<project_id>/<mr_iid>/<commit_sha>/<task_id>/
  output.log
```

执行完成后会删除解压出的 `source/` 目录，只保留 `output.log` 便于排查。脚本任务会移除配置中的 GitLab token 环境变量，避免脚本直接继承服务 token。

仓库提供了一个最小脚本示例：[examples/scripts/check_todo_tbd.py](examples/scripts/check_todo_tbd.py)。它会扫描 `GITLAB_WORK_RUNNER_CHECK_ROOT` 指向的目录；如果没有这个环境变量，则扫描当前工作目录。发现 `//TODO` 或 `//TBD` 时返回失败并输出文件位置。

注意：`command = "python3 examples/scripts/check_todo_tbd.py"` 中的相对路径是相对于 MR 代码快照根目录的。如果目标 GitLab 项目里没有这个脚本，请把示例脚本复制到目标仓库，或者把 `command` 改成 runner 机器上的绝对路径。Windows 上如果 `python3` 返回退出码 `9009`，通常表示命令不存在，可以改用 `python` 或把 Python 加入 `PATH`。

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
- Trigger: Merge request events

关于 `Merge request events` 的触发时机和 payload 字段，见 [GitLab Webhook 说明](docs/gitlab-webhook.md)。

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
