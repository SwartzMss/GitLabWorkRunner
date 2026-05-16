# GitLab Webhook 说明

本文说明 GitLabWorkRunner 依赖的 GitLab Webhook 行为，重点是 `Merge request events` 和 `Comments` 什么时候触发，以及服务收到消息后如何判断是否需要执行 Review。

参考资料：

- [GitLab Webhook events](https://docs.gitlab.com/user/project/integrations/webhook_events/)

## 推荐配置

在 GitLab 项目中配置 Project Webhook：

```text
URL: http://<host>:8080/webhooks/gitlab
Secret token: config.toml 中 [server].webhook_secret 的值
Trigger: Merge request events, Comments
```

自动 Review 需要开启 `Merge request events`。如果需要在 MR 评论中用 `@script_task_id` 手动触发 disabled script task，还需要开启 `Comments`。通常不需要同时开启 `Push events`，因为 GitLab 的 Merge Request event 本身会覆盖“source branch 新增 commit”的场景。

## Merge request events 什么时候触发

GitLab 官方文档说明，`Merge request events` 会在以下场景触发：

- 新 MR 创建。
- 已有 MR 被更新。
- MR 被 approved、unapproved、merged 或 closed。
- 用户对 MR 添加或移除 approval。
- reviewer 被重新请求 review。
- source branch 新增 commit。
- MR 上的所有讨论线程被 resolved。

所以，“MR 更新了会不会收到通知”的答案是：会收到，但不同更新类型的意义不同。

## 关键 payload 字段

本服务主要关心这些字段：

```text
object_kind
event_type
project.id
object_attributes.iid
object_attributes.action
object_attributes.last_commit.id
object_attributes.source_branch
object_attributes.target_branch
object_attributes.oldrev
changes
```

其中：

- `object_kind = "merge_request"` 表示这是 MR 事件。
- `object_attributes.action` 表示触发动作，例如 `open`、`update`、`merge`。
- `object_attributes.last_commit.id` 是当前 MR source branch 的最新 commit。
- `object_attributes.oldrev` 只会在 `update` 且存在实际代码变化时出现，例如 source branch 新 push commit 或应用 suggestion。
- `changes` 只包含本次事件中变化的字段。GitLab 文档也提醒：MR event 可能被触发，但 `changes` 为空，接收方应根据 `changes` 判断具体变化。

## 常见 action

| action | 含义 | 本服务的处理建议 |
| --- | --- | --- |
| `open` | 新 MR 创建 | 执行 Review |
| `update` | MR 更新 | 如果 commit 变化则执行 Review；否则依赖去重跳过 |
| `reopen` | MR 重新打开 | 通常可以执行 Review，去重会避免重复评论 |
| `merge` | MR 合并 | 不需要 Review |
| `close` | MR 关闭 | 不需要 Review |
| `approval` / `approved` | 审批状态变化 | 通常不需要 Review |
| `unapproval` / `unapproved` | 审批状态变化 | 通常不需要 Review |

当前实现不会按 action 做复杂过滤，而是用去重键控制重复执行：

```text
project_id + mr_iid + commit_sha + ruleset_hash
```

这意味着：

- 新 MR 创建时，会 review 当前 `last_commit.id`。
- MR source branch 新 push commit 时，`last_commit.id` 改变，会再次 review。
- 只修改标题、描述、label、reviewer 等元信息时，`last_commit.id` 通常不变，会被去重跳过。
- 规则文件改变时，`ruleset_hash` 改变，同一个 commit 也可以重新 review。

## Comments 手动触发脚本任务

开启 `Comments` 后，用户可以在 MR 评论中发送脚本任务命令：

```text
@check-todo-tbd
```

服务只处理 MR comment event：

```text
object_kind = "note"
object_attributes.noteable_type = "MergeRequest"
```

手动触发行为：

- 只按 `@id` 精确匹配 `[[script_tasks]].id`。
- 即使任务配置了 `enabled = false`，也允许手动触发。
- 手动触发忽略 `when_changed`，因为用户已经明确要求执行该任务。
- 手动触发不使用自动 Review 的去重键；用户每发一次合法命令，服务就执行一次。
- 如果评论里没有合法任务命令，或 `@id` 不存在，服务只记录日志并返回 accepted。
- issue、wiki、work item 等非 MR 评论会被忽略。

这意味着可以把高成本或低频脚本配置为：

```toml
[[script_tasks]]
enabled = false
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.rs"]
```

平时 MR 更新不会自动执行；需要时在 MR 评论区发送 `@check-todo-tbd` 即可。

## 为什么不依赖 Push events

GitLab 的 `Push events` 会在仓库 push 时触发，但它不一定直接告诉服务“这个 push 对应哪个 MR”。如果用 Push event，需要额外查询 commit 和 MR 的关系。

而 `Merge request events` 的 payload 直接包含：

- project id
- MR iid
- source branch
- target branch
- last commit

这些字段已经足够让服务拉取 MR diff 并发布 MR Discussion。因此第一版选择只依赖 `Merge request events`。

## 当前服务的处理流程

收到 GitLab Webhook 后：

1. 校验 `X-Gitlab-Token`。
2. 解析 payload，确认是 `object_kind = "merge_request"` 或 MR `object_kind = "note"`。
3. 提取 `project_id`、`mr_iid`、`last_commit.id`、source branch、target branch。
4. 对 MR event，计算去重键：`project_id + mr_iid + commit_sha + ruleset_hash`。
5. 对 MR event，如果已处理，直接跳过。
6. 通过 GitLab API 拉取 MR changes。
7. 对 MR event，解析 diff，只对新增行执行规则，并自动执行 `enabled = true` 的脚本任务。
8. 对 MR comment event，解析评论正文中的 `@task_id`，手动执行匹配的脚本任务。
9. 发布 GitLab MR Discussion。
10. 对 MR event 写入状态存储，避免重复评论；手动 comment event 不写入自动去重记录。

## 后续可以增强的地方

后续如果需要更精细控制，可以增加：

- 只处理 `open`、`update`、`reopen`。
- 对 `update` 事件优先检查 `object_attributes.oldrev`，没有代码变化时更早跳过。
- 对 `changes` 做更细判断，例如只在 `last_commit` 或 source branch 变化时 review。
- 对 system-initiated event 做过滤，例如 approval reset 这类系统事件。
- 增加轮询补偿任务，避免 Webhook 丢失导致漏 review。
