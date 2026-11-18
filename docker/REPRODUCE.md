# 旧容器方案迁移说明

新部署使用[官方宿主机部署流程](../doc/host-native-deployment.md)。本文件记录旧 Docker 方案的数据边界，不再提供外层容器作为推荐部署入口。

旧部署曾将整个运行目录保存在 `buckyos_dev_runtime` 或 `buckyos_release_runtime`，并将容器内部 Docker 数据放在独立卷。实际卷名须以 `docker inspect`、`docker volume inspect` 为准；当前原生实例使用宿主机 `/opt/buckyos` 和宿主机 Docker。

迁移已有身份时先停机保存一致快照，包含运行目录、内部 Docker 的应用实例卷、身份密钥与配套镜像，再按[数据库与整套实例迁移](../doc/local-database-migration.md)恢复。不能只复制 SQLite，也不能直接覆盖目标机器的整个 Docker data-root。

原来使用公开开发 seed 的实例，不能靠修改 hostname、全局替换 DID 或重签域名完成正式身份迁移。需要新身份时，在空安装目录使用官方 `buckyos provision` 激活；需要保留旧身份时，恢复其原始密钥与全部关联数据。这两种操作不能混用。

本机此前经授权弃用旧开发数据后完成了本地独立首装，后续上游更新则保留当前身份和数据。操作记录与验证限制见[部署改动清单](../doc/local-deployment-changes.md)。
