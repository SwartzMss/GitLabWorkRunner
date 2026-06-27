# GitLabWorkRunner

语言：**简体中文** | [English](README.en.md)

GitLabWorkRunner 是一个 Rust 编写的 GitLab Merge Request 自动 Review 服务。它通过 GitLab Webhook 获取 MR 变更，根据 `rules.toml` 执行规则、AI Review 或脚本任务，并把结果发布到 MR Discussion。

它不是 GitLab Runner 替代品，也不会自动执行目标仓库里的 CI 脚本；只会运行你在 `rules.toml` 中显式配置的检查。

## 工作原理

```mermaid
flowchart LR
    A["GitLab MR Webhook"] --> B["GitLabWorkRunner"]
    B --> C["拉取 MR diff"]
    C --> D["解析新增行"]
    D --> E["正则规则"]
    D --> F["AI Review"]
    D --> G["脚本任务"]
    E --> H["生成评论"]
    F --> H
    G --> H
    H --> I["GitLab MR Discussion"]
    B --> J["SQLite 去重状态"]
```

自动 Review 的一次请求大致是：

```mermaid
sequenceDiagram
    participant GitLab
    participant Runner as GitLabWorkRunner
    participant DB as SQLite
    participant AI as OpenAI-compatible API

    GitLab->>Runner: Merge request event
    Runner->>DB: 检查 commit + ruleset 是否处理过
    Runner->>GitLab: 拉取 MR changes / diff refs
    Runner->>Runner: 匹配规则并定位新增行
    Runner-->>AI: 可选发送 MR diff
    Runner->>GitLab: 发布行级或 MR 级评论
    Runner->>DB: 记录处理状态和评论信息
```

更多设计细节见 [docs/design.md](docs/design.md)。

## 支持能力

- GitLab Merge Request Webhook 自动触发 Review。
- 只对 MR diff 的新增行发布行级评论。
- `[[rules]]`：按路径和正则匹配新增行。
- `[[ai_reviews]]`：调用 OpenAI-compatible `POST /chat/completions` 做 AI Review。
- `[[script_tasks]]`：下载 MR head 快照并执行本地脚本。
- MR 评论手动触发脚本任务或 AI Review，例如 `@check-todo-tbd`、`@ai-review`。
- SQLite 去重，避免同一 commit 和规则集重复评论。
- stdout + 文件日志，内置按大小轮转。

## 快速开始

准备配置文件：

```powershell
Copy-Item config.example.toml config.toml
Copy-Item rules.example.toml rules.toml
$env:GITLAB_TOKEN = "<your-gitlab-token>"
cargo run
```

Linux / macOS：

```bash
cp config.example.toml config.toml
cp rules.example.toml rules.toml
export GITLAB_TOKEN="<your-gitlab-token>"
cargo run
```

在 GitLab 项目中添加 Webhook：

1. 进入 GitLab 项目，打开 `Settings` -> `Webhooks`。
2. `URL` 填写服务地址：

```text
http://<host>:8080/webhooks/gitlab
```

其中 `<host>` 是 GitLab 能访问到的 GitLabWorkRunner 地址。如果服务只在本机开发环境运行，需要用内网穿透、反向代理或部署到 GitLab 可访问的机器上；`localhost` 通常只对 GitLab 服务器自己生效。

3. `Secret token` 填写 `config.toml` 中 `[server].webhook_secret` 的值：

```toml
[server]
webhook_secret = "change-me"
```

4. 勾选 `Merge request events`。
5. 如果需要在 MR 评论里手动触发脚本任务或 AI Review，同时勾选 `Comments`。
6. 保存后可以使用 GitLab Webhook 页面里的 `Test` 功能发送测试事件；服务日志中应能看到收到 Webhook、校验 token、解析事件和后续处理结果。

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

运行前仍需要准备 `config.toml`、`rules.toml`，并设置 `GITLAB_TOKEN`。

## 服务配置

`config.toml` 控制服务、GitLab、存储、规则文件和日志：

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

`GITLAB_TOKEN` 是服务调用 GitLab API 使用的 token，和 Webhook 里的 `Secret token` 不是同一个东西。建议使用 Project Access Token 或专用 Bot 用户 token，scope 使用 `api`，项目角色至少 `Developer`。它需要能读取 MR diff、下载仓库 archive，并发布 MR discussion。

## 规则配置

最小 `rules.toml` 示例：

```toml
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Direct unwrap can panic at runtime. Prefer explicit error handling."
```

AI Review 示例：

```toml
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

脚本任务示例：

```toml
[[script_tasks]]
enabled = false
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.rs"]
```

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
@check-todo-tbd
@ai-review
```

手动触发不会使用自动 Review 的去重键；每条合法命令评论都会执行一次。

当前实现不会额外校验评论人的 GitLab 角色；只要用户能在 MR 评论，并且评论内容包含合法的 `@id`，服务就会执行对应手动任务。如果需要限制只有 Maintainer 或指定用户可以触发，需要在服务侧增加权限校验或 allowlist。

## 日志

默认日志配置：

```toml
[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

可以通过 `RUST_LOG` 调整日志级别：

```powershell
$env:RUST_LOG = "info"
```

GitLab token、Webhook secret 和 AI token 不会写入日志。

## 更多文档

- [docs/design.md](docs/design.md)：设计和模块边界。
- [docs/gitlab-webhook.md](docs/gitlab-webhook.md)：GitLab Webhook 配置和触发行为。
- [rules.example.toml](rules.example.toml)：完整规则示例。
- [examples/scripts/check_todo_tbd.py](examples/scripts/check_todo_tbd.py)：脚本任务示例。

## 许可证

MIT，见 [LICENSE](LICENSE)。
