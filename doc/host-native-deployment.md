# 本机校内独立部署

本机已通过官方离线 provision 首装，并切换为宿主机原生部署。node-daemon、cyfs-gateway 和系统服务由 systemd 直接运行；应用容器使用宿主机 Docker。核心身份、启动、用户/RBAC、消息和 Agent 创建不依赖官方运营的账号、SN/BNS 或凭据服务。模型单独配置。

部署使用官方产品源码，没有增加产品依赖或常驻启动框架。fork 已撤销旧 IP 登录和版本记录定制；Docker/Compose/Nginx 旧部署文件保留作归档参考，当前运行不使用它们。

## 访问与身份

- 桌面：<https://buckyos.ncu.bin-go.cc/>
- Jarvis：<https://jarvis.buckyos.ncu.bin-go.cc/>
- 对话：<https://buckyos.ncu.bin-go.cc/messagehub?entityId=did%3Aweb%3Ajarvis.buckyos.ncu.bin-go.cc>
- 本地管理员：`devtest`。口令在交付会话中提供，不写入仓库。
- Owner 和 Zone：`did:web:buckyos.ncu.bin-go.cc`。
- Device：`did:web:ood1.buckyos.ncu.bin-go.cc`。

这是新生成独立密钥的身份，不复用公开开发 seed。用户已授权弃用旧数据，旧 devtest 开发身份和历史未迁入当前实例。

校内客户端需要把本域名及子域名交给 `192.168.0.64:53` 解析，并信任校内 CA。也可在已有学校 DNS 配置相同主域名与通配记录。公开 CA 副本为 `/mnt/data/buckyos/recovery/campus-ca.crt`；它可以分发，两个私钥文件不可公开或提交 Git。本机已安装 CA 信任；其他客户端不会自动获得信任。

## 目录与配置

| 项目 | 位置 |
| --- | --- |
| 运行目录 | `/opt/buckyos`，bind mount 自 `/mnt/data/buckyos/runtime` |
| Docker 数据 | `/mnt/data/buckyos-host-docker` |
| DNS/TLS 网关配置 | `/opt/buckyos/etc/post_gateway.yaml` |
| 校内 DNS 记录 | `/opt/buckyos/etc/campus-dns.toml` |
| 本地身份/解析配置 | `/opt/buckyos/etc/node_identity.json`、`machine.json` |
| AgentRoot | `/opt/buckyos/data/home/devtest/agents/jarvis.buckyos.ncu.bin-go.cc/` |
| 恢复材料 | `/mnt/data/buckyos/recovery/`，目录0700、私钥0600 |
| 数据迁移步骤 | [数据库与实例迁移](local-database-migration.md) |
| 首装与恢复证据 | `/mnt/data/buckyos-campus-20261117-200648/` |
| 上游更新证据 | `/mnt/data/buckyos-upgrade-20261118-003339/` |

2026-11-18 已从干净的官方源码构建并覆盖更新：BuckyOS `9357c1fb747925655bef2370579928d5c36a13e0`，cyfs-gateway `f0ef92f271eb91e16c590eae32cde66ae59e60bd`。实际运行版本为 `0.7.0+build261118.HEAD.9357c1fb7479 (nightly)`，SDK/Tool 为 `0.7.191`。源码与构建产物保留在 `/mnt/data/buckyos-upgrade-20261118-003339/projects/`；两个官方工作树没有产品源码改动。fork 的 main 已合并同一个官方版本，产品源码与该官方提交一致。

`buckyos system info` 当前仍读取首次初始化写入的旧构建记录。这次官方文件更新没有刷新该记录；没有手改数据库或伪造版本。核验实际程序可运行 `/opt/buckyos/bin/node-daemon/node_daemon --version`；本次也核对了 systemd 服务组中15个运行程序与新构建产物的 SHA-256。

## 为什么原生部署仍有镜像

官方 `start.py` 启动宿主机上的 node-daemon，后者按调度配置管理应用。默认容器模式的 Agent 使用官方 `paios/aios`，`paios/exttool` 向工具卷提供种子；这与核心服务直接运行在宿主机上并不冲突。当前部署的最新源码中，Linux Full版的Agent Service使用容器；原生Agent用于Lite等关闭容器能力、且满足执行器授权条件的产品路径。切换默认执行位置不会让当前Full版停止使用镜像。实现入口为 `src/kernel/node_daemon/src/paios_images.rs` 和 `src/kernel/buckyos-api/src/app_schema.rs` 的 `native_agent_node`；[App Loader](app_service/rust_app_loader.md)的Worker规则见§9.1.1，产品条件以当前部署的官方源码为准。

本次使用官方镜像，没有自建 Worker 镜像或个人镜像覆盖。更新前按不可变 digest 下载并核对内容，停机后才替换本机标签；日常启动只使用本地镜像，不需要联系官方镜像仓库。编译也使用官方宿主机脚本；曾尝试的额外构建容器已停用，旧 Compose/Dockerfile 不参与运行。测试隔离脚本只保留作本次验收证据，不是部署或启动依赖。

## 官方离线首装入口

新节点先按[官方源码构建步骤](../README_zhCN.md)准备工具链、构建 cyfs-gateway 和 BuckyOS。源码可从本 fork 的 `main` 获取，家族依赖仍使用官方 `main` 分支；构建机器可以预先准备源码、SDK、包缓存与镜像，核心系统运行时不需要访问这些下载源。完全离线的目标机也可直接采用[整套实例恢复流程](local-database-migration.md)。

普通开发环境的 `start.py --all` 使用公开 dev seed，不用于本校独立身份首装。新的空安装环境使用官方 `start.py --reinstall release` 准备待激活安装；停止待激活进程后执行下面的本地 provision。已有实例的迁移或更新不走此步骤。

已安装 Tool 提供 `buckyos provision status/check/activate`。它生成随机 Owner/Device 密钥，构造真实签名文档，并把 Owner=Zone 设置为自有域名 DID；不经过在线 Web 激活，也不调用 SN/BNS。

```bash
buckyos provision activate --help
buckyos provision check --root <新的安装目录> --domain <自己的域名> --owner-name <管理员名> --owner-key-backup <新建的私钥备份文件>
buckyos --non-interactive --yes --input <0600秘密输入文件> provision activate --root <新的安装目录> --domain <自己的域名> --owner-name <管理员名> --owner-key-backup <新建的私钥备份文件>
```

秘密输入文件使用命令 schema 中的 `admin_password` 字段。以上仅用于尚未激活的新安装目录，不能对当前实例重复激活。迁移已有实例时恢复原身份，不重新生成密钥。

激活后按下一节配置自有域名解析、引导 TXT 与本地 CA 信任，确认站点网关监听正确，再启动官方 node-daemon。源码开发入口为 `uv run start.py --skip-update`（在 `src/` 下运行）；安装了官方 systemd 服务的节点使用 `sudo systemctl start buckyos.service`。一个实例只使用一个启动管理入口。随后执行 `check.py` 并验证登录、DNS、DID 与 Agent，不能把待激活端口可达当作部署完成。

## 域名、证书与网络

cyfs-gateway 使用已有 `local_dns`/`dns` 模块，在 LAN 地址监听 UDP/TCP 53；主域名、通配子域名都解析到本机，TXT 提供 Owner 签名的 BOOT/DEV 及 PKX。没有另装 DNS 守护进程。引导材料和本地 Root Trust 使用现有校验机制。

TLS 使用包内 provision SDK 生成的独立本地 CA，配置主域名及通配子域名证书，不使用外部 ACME。证书在 `local/identity/`，对应服务私钥在 `security/`。CA 签发私钥与 Owner 恢复私钥单独保存在 recovery 目录，备份需同时带走。

本机既有 sing-box DNS 增加了针对本域名后缀的转发，目标是同一个本地 DNS；其他网络配置保持原语义。旧 `buckyos-ingress-bypass` 专用脚本、nft 配置和 service 已撤除；继续使用通用网络回程规则。迁移到没有 sing-box 的主机时不需要为了 BuckyOS 安装它，直接配置校内 DNS 即可。

## 日常管理与更新

```bash
sudo systemctl status buckyos.service
sudo systemctl restart buckyos.service
python3 src/check.py
/opt/buckyos/bin/buckyos --yes system info
/opt/buckyos/bin/buckyos --yes agent get jarvis.buckyos.ncu.bin-go.cc
docker ps
```

在仓库根目录运行上述 Python 命令。要等网关服务目录完整和认证 RPC 成功，不能只看端口；端口开放早于服务登记，过早进入 MessageHub 可能返回503。已有 systemd 保活时，源码更新前先停服务，避免 stop.py 后被自动拉起。源码构建与普通更新见[官方 README](../README_zhCN.md)。

更新顺序为：在干净官方源码中构建网关及 BuckyOS → 先在停机快照副本验证 → 停止 systemd 和应用 → 备份运行目录、实例卷及恢复材料 → 分别从两个项目的 `src/` 调用官方 `buckyos-update --app=<应用名> --target-rootfs=/opt/buckyos` → 更新已核对的本地 Worker 镜像 → 启动 systemd → 验证登录、域名、Agent及重启。不要把隔离测试副本里的数据库覆盖回正在使用的实例。

本次更新保留了10个站点身份/配置文件，包括模块更新会覆盖的 `bin/node-active/active_config.json`。更新后的程序目录由官方 updater 提供；站点配置从当前实例保留。此次完整回退快照为 `/mnt/data/buckyos-upgrade-20261118-003339/final-snapshot/`，回退应恢复整套旧数据、实例卷及旧镜像，不能只换回旧二进制。

不要用 `start.py --all`、`--reinstall` 或 `provision activate` 代替日常重启。保留规范 `/opt/buckyos` 路径及 bind mount；符号链接布局会与 Deno launcher 的执行权限路径冲突。

## 验收与限制

已通过：无外网全新启动、口令登录与SSO刷新、匿名/无效token拒绝、本地用户创建及越权拒绝、Jarvis创建与工具基线、UDP/TCP通配DNS和签名TXT、原生服务重启、真实浏览器登录/桌面/MessageHub、公开域名与设备 DID 文档核对，以及停机备份后的断网恢复启动。

真实浏览器辅助环境使用精确证书 SPKI pin；正常 TLS 验链另以校内 CA 验证。正式用户应安装校内 CA，不使用忽略证书错误的启动参数。

AI Provider 未随旧数据迁入，因此未宣称 Jarvis 已完成真实模型回复。模型可在 AI Center 独立配置。整机重启、新架构机器、跨版本数据库升级、多 OOD 与跨 Zone 联邦没有在本次验收中实测。

此次上游更新另外通过：官方宿主机构建、分发 CLI 冷缓存断网启动、当前数据副本断网升级、线上登录/SSO、8项DNS、已有Agent模板更新与工具基线、15个运行程序校验，以及systemd重启后持久性。标准Agent工具齐全；浏览器、Office等可选扩展未齐备，没有验证它们的实际能力。

选择的Python测试最终132通过、2跳过；Rust认证、调度、libopendan及opendan测试合计332通过、18忽略、1项失败。未解决项为官方 `browser::screen_tests::what_pages_show_outside_themselves_is_asked_of_the_screen`：重连时多回放一个confirm弹窗请求；普通用户和root均可复现，未修改或跳过断言。不是全量Cargo测试通过。日志和验收JSON在 `/mnt/data/buckyos-upgrade-20261118-003339/`。
