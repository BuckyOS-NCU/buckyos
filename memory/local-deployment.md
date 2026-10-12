此部署宿主机的 500 GB ext4 数据盘 UUID 为 `a08bd4c5-4244-4ed6-a5cc-cf134af698e2`，挂载到 `/mnt/data`，通过 `/etc/fstab` 按 UUID 自动挂载。盘上已有 `app/`、`docker/` 目录，使用新子目录存放构建数据或备份，避免覆盖既有内容。

宿主机 Docker 数据目录仍为 `/var/lib/docker`，位于 50 GB 根分区。大型临时数据和备份应放到 `/mnt/data`；数据盘挂载不会自动改变 Docker 数据目录。

本机 Zone 公开 hostname 为 `bucykos.ncu.bin-go.cc`，原 Zone DID `did:web:test.buckyos.io` 保留。域名绑定同时维护 system-config 的 `boot/config` 和本地 owner 签名的启动 Zone 文档。cyfs-gateway 接收宿主机 80/443 并通过 HTTP-01 管理证书；`/root/neko/nginx/compose.yaml` 的 Nginx 只在 `buckyos_default` 内网处理 HTTP→HTTPS 跳转。完整主机配置见 `/root/neko/nginx/README.md`。

此主机的 sing-box 默认 bypass 仅覆盖 SSH/Tailscale。Docker Web 入站连接另由 `buckyos-ingress-bypass.service` 标记，在 prerouting/output 恢复连接标记，再通过主路由表回程；仅恢复宿主机 output 标记不足以处理 Docker 转发回复。未配置此规则时，80/443 TCP 连接超时；配置后可信 HTTPS 和证书签发已验证成功。

BuckyOS Compose 将 `/run` 挂为 tmpfs，避免普通容器重启时残留 containerd PID/socket 导致内部 Docker 无法启动；持久 Docker 数据仍由独立卷保存。

Jarvis 容器的 OpenDAN 程序和 WebUI 来自内层 AIOS 镜像；更新外层 BuckyOS 镜像不会自动同步它们。诊断时分别核对内外层实际程序版本及容器内 `bin/opendan/web/index.html`，不要以外层 WebUI 文件存在推断 Jarvis 容器也有网页产物。

此主机通过运行卷内 `etc/devenv.json` 的 `aios` 使用 `buckyos-aios-local` Worker 镜像。`docker/build-aios-runtime.sh` 在内部 Docker 中构建它，可通过 `BUCKYOS_OPENDAN_BINARY` 使用已编译的 OpenDAN；随后需要重建应用容器。node-daemon 对本地已有的 Worker 标签不会自动 pull。

Agent Loader 实际读取 `data/home/<owner>/.local/share/<app_id>/agents/<agent_id>/agent.toml`。旧 `[dispatch.rule]` 不会启用新 Loader 的消息入口，需要 `[[loader.ui]]`；迁移时保留本地配置并按已有文件同步策略更新模板，不能只更新容器程序。

AICC 的 DeepSeek discovery 将 `deepseek-flash` 归一为 `deepseek-v4.1-flash`；Provider variants 必须覆盖归一后的模型身份，否则实例健康、模型已发现仍会报 `missing_channel_mapping`，导致 `llm.chat` 无候选。本机映射补充位于运行卷 `etc/aicc/driver_metadata/local/providers/deepseek.provider.json`，通过 `service.reload_settings` 生效；本地 metadata 覆盖内置定义，system-config metadata 的优先级更高。

可迁移部署入口为仓库内 `docker/REPRODUCE.md`；原 `/root/neko/nginx/` 的域名网关、HTTP 跳转和可选 TUN 回程配置已收录到 `docker/domain/`。外部依赖跟随 main/latest，源码部署不保证复现已有镜像摘要；精确迁移仍需镜像和持久卷。
