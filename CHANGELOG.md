# Changelog

## Unreleased

- 移除脚本任务；配置中的 `[[script_tasks]]` 现在会被严格解析拒绝，升级时必须删除这些配置块。
- AI archive 下载或解压触发任一安全限制时记录 WARN，并以仅 MR diff 的模式继续 Review；此时不提供上下文工具，也不发送 `archive_limit_exceeded` 失败通知。
- archive 超时、权限、HTTP、损坏 ZIP、文件系统等非限制错误仍会使 Review 失败。
- 带上下文的 AI Review 仅在 `review_run_timeout`、`ai_request_timeout` 或 `ai_tool_loop_timeout` 时独立重启一次 diff-only fallback；新执行获得完整 `timeout_seconds` 预算，保留 request timeout、分批与 `max_batches`，但不提供上下文工具、不做 fallback clean confirmation、也不递归重试，最坏耗时可接近两倍预算加少量开销，且无需新增配置。
- Dashboard 和 GitLab Review 汇总现在展示 timeout/archive 降级原因及 context/fallback 耗时；Dashboard 还展示相加后的总耗时，新增 nullable metadata 与旧数据库记录兼容。非适用错误与 fallback 自身失败仍走原有失败路径。

## v2.0.8

- 新增 `[gitlab].api_timeout_seconds` 和 `[gitlab].archive_timeout_seconds` 配置。
- repository archive 下载现在使用独立超时，避免大仓库 archive 下载被默认 30 秒 GitLab API 超时提前中断。
- `config.example.toml`、README 和 release notes 补充 archive 下载超时配置说明。

## v1.4.3

- 新增 MR 评论手动触发脚本任务：在 GitLab Webhook 开启 `Comments` 后，可在 MR 评论中发送 `@script_task_id` 执行指定 `[[script_tasks]]`。
- `enabled = false` 的脚本任务不再被配置加载阶段丢弃，可通过手动评论触发；自动 MR 触发仍只执行 `enabled = true` 的任务。
- 手动触发忽略 `when_changed` 且不使用自动 Review 去重，用户每发一次合法命令就执行一次。

## v1.4.2

- 脚本任务 `exit 1` 现在会读取 `result.txt` 并发布 MR 评论。
- `result.txt` 中符合 `path:line:message` 的结果会尽量发布为行级评论；无法解析或 diff refs 不完整时发布 MR 级汇总评论。
- 修复脚本任务切换到 runner 可执行文件目录执行后，相对 `work/` 路径传给脚本可能找不到 `result.txt` 的问题。

## v1.4.1

- 修复脚本任务执行目录：`command` 现在从 runner 可执行文件所在目录执行，相对路径可直接引用 release 包内的 `examples/scripts/...`。
- 被检查的 MR head 代码快照仍作为第 1 个参数传给脚本，`result.txt` 路径仍作为第 2 个参数传给脚本。
- 日志新增 `script_cwd` 字段，便于确认脚本命令实际从哪个目录执行。

## v1.4.0

- 发布包新增 `examples/` 目录，示例脚本会随 Windows/Linux release artifact 一起打包。
- 脚本任务命令统一使用 `python` 示例，不再使用 `python3`。
- 脚本任务命令在 runner 可执行文件所在目录执行，相对路径不再绑定目标 GitLab 仓库。
- 脚本任务执行时会追加两个参数：MR head 代码快照根目录和结果文件路径。
- 脚本任务输出拆分为 `run.log` 和 `result.txt`：stdout/stderr 写入 `run.log`，检测结果写入 `result.txt`。
- 定义脚本任务退出码协议：`0` 表示检测通过，`1` 表示检测发现问题，其他退出码或超时表示脚本执行异常。
- 脚本任务失败、发现问题或超时都不再发布 MR 评论，只记录服务日志并保留 `run.log` / `result.txt`。
- 示例脚本 `examples/scripts/check_todo_tbd.py` 适配新的双参数协议。

## v1.3.0

- 新增 `[[script_tasks]]`，支持下载 MR head 代码快照后运行独立脚本任务。
- 支持每条脚本任务独立配置 `enabled`、`command`、`timeout_seconds` 和 `when_changed`。
- 脚本任务失败或超时时发布 MR 级评论，并保留 `output.log` 便于排查。
- 新增 `examples/scripts/check_todo_tbd.py` 示例脚本。
- 脚本 archive 下载改用 MR diff refs `head_sha`，避免 webhook commit 与最新 MR diff 不一致。
- 脚本任务超时时尽量清理子进程树，减少残留进程风险。
- 不完整 diff refs 场景下，行级评论改为跳过并发布简短 MR 级提示。
