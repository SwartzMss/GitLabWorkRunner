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

自动 Review 需要开启 `Merge request events`。如果需要在 MR 评论中用 `@ai_review_id` 手动触发 AI Review，还需要开启 `Comments`。脚本任务仍可选支持，但不是默认推荐路径。不需要开启 `Push events`。

## Webhook Secret 和 GitLab API Token

Webhook 页面里的 `Secret token` 只用于校验收到的请求是否来自 GitLab。GitLab 会把它放在 `X-Gitlab-Token` 请求头里，服务会用 `config.toml` 的 `[server].webhook_secret` 校验它。

服务真正调用 GitLab API 时使用的是 `config.toml` 里的 `[gitlab].token`。建议使用 Project Access Token 或专用 Bot 用户 token，scope 使用 `api`，项目角色至少 `Developer`。不要把包含真实 token 的 `config.toml` 提交到仓库。

当前服务会用这个 token 调用：

```text
GET /projects/:id/merge_requests/:merge_request_iid/changes
GET /projects/:id/repository/archive.zip
POST /projects/:id/merge_requests/:merge_request_iid/discussions
```

因此 token 需要能读取 MR diff、读取仓库 archive，并发布 MR discussion。

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

## Comments 手动触发 AI Review

开启 `Comments` 后，用户可以在 MR 评论中发送 AI Review 命令：

```text
@ai-review
```

服务只处理 MR comment event：

```text
object_kind = "note"
object_attributes.noteable_type = "MergeRequest"
```

手动触发行为：

- AI Review 手动触发按 `[[ai_reviews]].id` 精确匹配，忽略 `auto_enabled` 和 `when_changed`。
- 手动触发不使用自动 Review 的已完成去重键；同一个 commit 完成后可以再次触发。
- 但同一个 `project_id + mr_iid + commit_sha` 如果仍在执行中，新的触发会被跳过；服务会给触发评论加 `eyes`，并回复一条 MR 评论提示当前 commit 已有 review 正在执行，请稍后再试。
- 如果评论里没有合法脚本任务或 AI Review 命令，或 `@id` 不存在，服务只记录日志并返回 accepted。
- issue、wiki、work item 等非 MR 评论会被忽略。

权限边界：

- Webhook 只负责把 MR 评论事件发给服务，不代表评论人一定有执行任务的额外权限。
- 当前实现不会额外查询或校验评论人的 GitLab 角色。
- 只要用户能在 MR 评论，并且评论内容包含合法的 `@id`，服务就会执行对应手动任务。
- 如果需要限制只有 Maintainer 或指定用户可以手动触发，需要在服务侧增加评论人权限校验或 allowlist。

如果确实需要可选脚本任务，也可以把高成本或低频脚本配置为：

```toml
[[script_tasks]]
auto_enabled = false
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.rs"]
```

平时 MR 更新不会自动执行；需要时在 MR 评论区发送 `@check-todo-tbd` 即可。

脚本任务的手动触发规则和 AI Review 类似：只按 `@id` 精确匹配 `[[script_tasks]].id`；即使任务配置了 `auto_enabled = false`，也允许手动触发；手动触发忽略 `when_changed`。如果同一条评论同时包含脚本任务和 AI Review 的合法命令，服务会分别执行匹配项。

## 当前服务的处理流程

收到 GitLab Webhook 后：

1. 校验 `X-Gitlab-Token`。
2. 解析 payload，确认是 `object_kind = "merge_request"` 或 MR `object_kind = "note"`。
3. 提取 `project_id`、`mr_iid`、`last_commit.id`、source branch、target branch。
4. 对实际会执行 review 的事件，先登记运行中 key：`project_id + mr_iid + commit_sha`。
5. 如果同一个 key 已经在运行中，跳过本次 review；MR comment 触发时会加 `eyes` 并发布提示评论。
6. 对 MR event，计算已完成去重键：`project_id + mr_iid + commit_sha + ruleset_hash`。
7. 对 MR event，如果已处理，直接跳过。
8. 通过 GitLab API 拉取 MR changes。
9. 对 MR event，如果 GitLab diff refs 不完整，发布一条 MR 级跳过提示并写入状态存储。
10. 对 MR event，解析 diff，并自动执行匹配的 `auto_enabled = true` AI Review 和脚本任务。
11. 对 MR comment event，解析评论正文中的 `@id`，手动执行匹配的脚本任务和 AI Review。
12. 发布 GitLab MR Discussion。
13. 对 MR event 写入状态存储，避免重复评论；手动 comment event 不写入自动去重记录。
14. review 完成或失败后，释放运行中 key；因此同一个 commit 完成后可以再次手动触发。

## 后续可以增强的地方

后续如果需要更精细控制，可以增加：

- 只处理 `open`、`update`、`reopen`。
- 对 `update` 事件优先检查 `object_attributes.oldrev`，没有代码变化时更早跳过。
- 对 `changes` 做更细判断，例如只在 `last_commit` 或 source branch 变化时 review。
- 对 system-initiated event 做过滤，例如 approval reset 这类系统事件。
- 增加轮询补偿任务，避免 Webhook 丢失导致漏 review。
