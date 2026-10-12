# 从纯净机器复现本地部署

目标仓库为 `https://github.com/BuckyOS-NCU/buckyos` 的 `main`。本流程重建源码和部署行为，不复制当前实例的账号设置、会话、文件、Provider 密钥或证书。当前外层镜像是 0.7.0 dev；Jarvis 使用单独构建的本地 AIOS Worker。

## 前置条件

- Linux x86_64、Docker Engine 和 Docker Compose v2，允许 privileged 容器及容器内 Docker。当前 Dockerfile 固定构建 x86_64，不支持直接用于 ARM。
- 能访问 GitHub、Debian、Rust、npm、Deno 和 Docker 镜像源。构建在容器内进行，宿主机不需安装 Rust/Node/Python。
- 宿主机的 80、443、8080、2980、3180、3182 端口可用；防火墙按访问方式开放。
- 完整编译需要较多内存和磁盘；Dockerfile 当前设置 8 个 Cargo 并行任务。本次未测量最低资源要求。

## 1. 桌面与本地 Zone

```bash
git clone --branch main https://github.com/BuckyOS-NCU/buckyos.git
cd buckyos
cp docker/env.example docker/deployment.local.env
```

把 `docker/deployment.local.env` 的 `BUCKYOS_LOCAL_HTTP_ORIGINS` 改为新主机实际的 LAN/Tailscale IP，例如 `http://192.168.1.20:3180,http://192.168.1.20:8080`。后续 Compose 命令始终使用同一个 env 文件：

```bash
docker compose --env-file docker/deployment.local.env up -d --build
docker compose --env-file docker/deployment.local.env ps
```

通过实际服务探测和日志检查启动状态：

```bash
curl -f http://192.168.1.20:3180/
docker compose --env-file docker/deployment.local.env logs --tail 100 buckyos
```

打开 `http://<新主机IP>:3180`，首次初始化默认账号为 `devtest` / `bucky2025`。dev 模式自动初始化 `test.buckyos.io` Zone。IP 入口提供桌面；应用子域名需额外 DNS/hosts 配置。客户端 hosts 可指向新主机：

```text
192.168.1.20 test.buckyos.io sys.test.buckyos.io jarvis.test.buckyos.io
```

默认 Compose 中的旧主机 IP 只用于原部署；新机器必须覆盖来源列表。要使用正式身份，改为 `BUCKYOS_CONFIG_GROUP=release` 后构建，在 3182 激活；官方 SN 注册需要邀请码。

## 2. Jarvis Worker

外层构建已生成当前源码的 OpenDAN、WebUI、xagent、agent_tool 和 SDK。将它们装入内层 Worker，避免使用远端旧 Worker 中的程序：

```bash
bash docker/build-aios-runtime.sh
docker exec -i buckyos python3 - <<'PY'
import json
from pathlib import Path
p = Path('/opt/buckyos/etc/devenv.json')
d = json.loads(p.read_text()) if p.exists() else {}
d['aios'] = 'buckyos-aios-local'
t = p.with_suffix('.json.tmp')
t.write_text(json.dumps(d, indent=2) + '\n')
t.replace(p)
PY
```

随后从桌面安装/启动 Jarvis；已有实例需通过 App Service 面板重建应用容器，让它使用 `buckyos-aios-local:latest-amd64`。单独重启外层容器不会更新 Worker。`BUCKYOS_OPENDAN_BINARY` 只供使用外部已编译产物，纯净机器流程无需指定本机备份路径。

当前源码已有 `[[loader.ui]]` Agent 模板。旧数据卷的 `[dispatch.rule]` 配置不能直接当作新 Loader 配置；既有实例的本地修改也可能阻止模板同步。应先备份，再核对 AgentRoot 的 `agent.toml`，不能据此流程宣称完成旧数据迁移。

DeepSeek `deepseek-v4.1-flash` 的 none/high/max 映射已从运行卷修复同步到源码内置 metadata。需要在 AICC 配置自己的 Provider endpoint、API key、模型和路由才能进行真实对话。源码部署不会携带当前实例的凭据。系统配置或本地 metadata 覆盖内置定义时，仍需检查覆盖内容。

## 3. 公网 HTTPS（可选）

仓库内 `docker/domain/` 保存当前部署使用的网关、Nginx HTTP 跳转及 sing-box 回程配置。按 [域名部署说明](domain/README.md) 换成自己的域名后安装。

DNS、路由器端口转发、Tailscale 注册和 sing-box 本身由新主机自行配置。原域名 `bucykos.ncu.bin-go.cc` 仍指向现有主机；仅复制文件不会让新主机接管域名。

## 复现边界

| 范围 | 可达到的程度 |
| --- | --- |
| 桌面、登录、本地 Zone、持久卷 | 有源码构建及 Compose 初始化流程；需在新机器完整验收 |
| Jarvis 程序和 WebUI | 本地 Worker 构建流程；需安装应用、配置 Provider 后验收对话 |
| 公网 HTTPS、HTTP 跳转 | 配置已收录；需自己的 DNS、80/443 入站和 Zone 域名绑定 |
| 当前用户文件、会话、身份、账号设置 | 不在 Git 中；完整迁移需单独备份/恢复运行卷与内部 Docker 数据卷 |
| 与当前实例逐字节相同的二进制 | 不保证；外部 Git 依赖跟随 main，npm/Worker/toolchain 部分跟随 latest/stable，且锁文件未纳入仓库 |

现有镜像 ID 和关键程序摘要见 [deployment-reference.json](deployment-reference.json)，仅供核对，不是可下载的镜像。外层镜像早于 Worker 的公开域名修复，两者程序摘要因此不同。

若需要精确恢复当前实例，应另行安全保存外层镜像、内层 Worker 镜像及两个持久卷，并记录镜像 ID/摘要；源码提交不能替代这些产物。复制现有实例时不要同时运行两套相同 Zone 身份。

本次整理没有在第二台纯净机器做完整构建，也没有执行完整 Rust workspace、全量 DV 或新 Provider 的真实模型调用。推送前验证结果以任务交付说明为准。
