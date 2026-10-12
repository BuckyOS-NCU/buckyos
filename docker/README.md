BuckyOS Docker 启动说明

纯净机器的完整步骤、Jarvis Worker 和复现边界见 [REPRODUCE.md](REPRODUCE.md)；公网配置见 [domain/README.md](domain/README.md)。

在仓库根目录首次构建并启动：

```bash
docker compose up -d --build
```

如果部署主机无法访问官方 npm registry，可指定构建用的镜像源：

```bash
NPM_REGISTRY=https://registry.npmmirror.com docker compose build
docker compose up -d --no-build
```

镜像从当前仓库构建 BuckyOS，并从 `main` 分支构建 `cyfs-gateway`。默认采用 `dev` 本地开发配置，预置 Zone `test.buckyos.io` 和账号 `devtest` / `bucky2025`，无需邀请码或手工激活。

当前部署支持直接从另一台 Tailscale 设备打开 `http://100.79.198.28:3180`，使用 `devtest` / `bucky2025` 登录桌面。局域网入口是 `http://192.168.0.64:3180`；这两个地址的 `8080` 端口也可用。

此主机另已绑定 `https://bucykos.ncu.bin-go.cc/`。HTTP/HTTPS 第一跳是容器内的 cyfs-gateway，证书由它通过 HTTP-01 自动维护。`/root/neko/nginx/compose.yaml` 启动内网 Nginx，处理网关转来的 HTTP→HTTPS 跳转；配置和 sing-box Web 回程规则已收录至 [domain/](domain/README.md)，原部署副本仍位于 `/root/neko/nginx/`。现有实例的密码以当前账号设置为准，上述密码仅为首次初始化默认值。

`BUCKYOS_LOCAL_HTTP_ORIGINS` 配置允许直接访问桌面的 IP 来源地址，格式为逗号分隔的规范 HTTP(S) IP 来源地址（协议、IP、端口），无需结尾斜杠。容器启动时将它写入 `/opt/buckyos/etc/local_access_origins.json`，网关和 Control Panel 使用同一份配置校验路由、登录回调和会话刷新。IP 入口仅提供 Control Panel 桌面；独立应用域名仍按 Zone 的域名规则访问。

更换部署地址时，例如：

```bash
BUCKYOS_LOCAL_HTTP_ORIGINS=http://100.79.198.28:3180 docker compose up -d
```

如需按 Zone 域名访问，可在客户端 hosts 中将 `test.buckyos.io` 和 `sys.test.buckyos.io` 指向部署主机的 IP，然后打开 `http://test.buckyos.io:3180`。

当前部署主机的 Tailscale 地址是 `100.79.198.28`；访问设备上的 hosts 条目为：

```text
100.79.198.28 test.buckyos.io sys.test.buckyos.io
```

如需使用官方 SN/BNS 注册自己的身份，采用 `release` 模式：

```bash
BUCKYOS_CONFIG_GROUP=release docker compose up -d --build
```

该模式在 `http://<主机IP>:3182` 手工激活；官方 SN 注册需要有效邀请码。

所有端口发布到 `0.0.0.0`：

| 主机端口 | 容器端口 | 用途 |
| --- | --- | --- |
| 80 | 80 | HTTP 域名入口和 ACME 验证 |
| 3182 | 3182 | release 模式的首次激活页面 |
| 8080 | 80 | 系统 HTTP 入口 |
| 3180 | 3180 | NodeGateway |
| 443 | 443 | HTTPS |
| 2980 | 2980 | RTCP |

运行目录按模式分别保存在 `buckyos_dev_runtime` / `buckyos_release_runtime` 卷中，容器内 Docker 的镜像和应用数据保存在 `buckyos_docker-data` 卷中。原先待激活环境的 `buckyos_runtime` 卷保留。BuckyOS 容器使用 `privileged`，以启动应用所需的内部 Docker daemon。

```bash
docker compose ps
docker compose logs -f buckyos
docker compose restart buckyos
docker compose stop
docker compose up -d --no-build
```

服务日志位于容器内 `/opt/buckyos/logs`。上述停止和重启命令保留持久卷。首次启动时镜像内容会复制到运行卷；后续创建容器时同步镜像中的程序、Web 页面、SDK/CLI 和 boot gateway 配置，用户身份、设置和数据保留在运行卷中。

容器的 `/run` 使用 tmpfs，普通重启时清除 Docker/containerd 的运行 PID 和 socket；应用镜像及数据仍保存在 `buckyos_docker-data` 卷中。

设置页的 Build Version 来自 system-config 的 `system/buckyos_info`。scheduler 启动时按实际程序版本更新该记录，保留首次安装时间；同一构建重启不改变更新时间。

Jarvis 在内部 Docker 的 AIOS Worker 镜像中运行。更新外层 BuckyOS 镜像不会自动更新它；node-daemon 仅在本地缺少 Worker 镜像时拉取，已有的 `latest-amd64` 标签不会自动刷新。

本机 `/opt/buckyos/etc/devenv.json` 的 `aios` 已设为 `buckyos-aios-local`。`docker/build-aios-runtime.sh` 在内部 Docker 中构建该镜像，复用当前运行环境的 OpenDAN、xagent、agent_tool、WebUI 和 SDK，使用仓库内的入口脚本及 Agent 工具。需要替换 OpenDAN 程序时可指定已编译产物。纯净机器从完整源码构建外层镜像后，重建命令是：

```bash
bash docker/build-aios-runtime.sh
```

构建后通过 App Service 面板重启 Jarvis，使它使用新镜像。Agent 数据与应用实例卷保留。本机已将旧 Agent 配置迁移到 Loader 消息接入规则，通过实例工作副本机制同步未被本地修改的模板；备份与验证记录位于 `/mnt/data/buckyos-backups/20261009/jarvis-repair/`。完整源码构建和发布 AIOS 仍使用仓库根目录的 `build_aios`。
