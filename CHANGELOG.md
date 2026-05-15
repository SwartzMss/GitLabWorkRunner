# Changelog

## v1.3.0

- 新增 `[[script_tasks]]`，支持下载 MR head 代码快照后运行独立脚本任务。
- 支持每条脚本任务独立配置 `enabled`、`command`、`timeout_seconds` 和 `when_changed`。
- 脚本任务定义输出协议：stdout/stderr 写入 `run.log`，检测结果写入 `result.txt`；`0` 表示检测通过，`1` 表示检测发现问题，其他退出码或超时表示脚本执行异常；非通过状态不发布 MR 评论。
- 新增 `examples/scripts/check_todo_tbd.py` 示例脚本。
- 脚本 archive 下载改用 MR diff refs `head_sha`，避免 webhook commit 与最新 MR diff 不一致。
- 脚本任务超时时尽量清理子进程树，减少残留进程风险。
- 不完整 diff refs 场景下，行级评论改为跳过并发布简短 MR 级提示。
