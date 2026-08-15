---
status: accepted
---

# 采用 SQLite 单写者存储与版本化导出

QuickNote 使用平台私有应用数据目录中的单个 bundled SQLite 数据库作为领域事实源。一个后台存储线程独占连接并串行执行应用命令；UI 与 `PlatformServices` adapter 不执行 SQL。

数据库使用 WAL、`synchronous=NORMAL`、稳定 UUID、UTC Unix 毫秒和只前向迁移。`current_note` 是零或一行的 singleton 关系；提醒事实与持久 outbox 在同一事务提交，平台投影只在提交后发生。

迁移开始前使用 SQLite Online Backup API 生成一致备份，任一步失败都回滚旧 schema 并保留备份。高于客户端支持版本的数据库拒绝写入，不执行自动降级。

完整 JSON 稳定的是版本化领域契约而非 SQLite 表结构；单张 Markdown 保留原始正文。导出、备份恢复和永久清除的完整产品行为在后续纵向切片实现。
