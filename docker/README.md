# 旧 Docker 封装（归档）

本 fork 当前使用官方宿主机部署。部署、离线激活、域名/DNS/TLS 和日常更新见[宿主机部署流程](../doc/host-native-deployment.md)，数据备份与迁机见[数据库迁移说明](../doc/local-database-migration.md)。

仓库根目录的 `Dockerfile`、`compose.yaml` 和本目录入口脚本保留作旧方案参考，当前实例不使用它们；本次上游更新没有重新验证这套外层容器构建。不要将这些文件当作学校环境的安装入口，也不要把旧卷直接挂到新身份上。

官方宿主机部署本身仍会使用 `paios/aios` 和 `paios/exttool` 应用镜像，由 node-daemon 管理。它们与这里的外层 BuckyOS 容器封装是两种用途，规则见 [App Loader](../doc/app_service/rust_app_loader.md) §9.1.1。

已移除旧方案的自定义 Worker 拼装、IP 登录配置脚本及开发身份域名重签脚本。核心身份通过官方 `buckyos provision` 本地生成，不需要模拟官方 SN/BNS，也不依赖官方运营的身份服务。模型按需单独配置。

存量容器方案的迁移边界见 [REPRODUCE.md](REPRODUCE.md)，旧公网入口配置说明见 [domain/README.md](domain/README.md)。
