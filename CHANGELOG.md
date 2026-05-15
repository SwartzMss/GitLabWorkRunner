# Changelog

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
