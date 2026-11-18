# NCU fork 部署改动与验证记录（2026-11-18）

本 fork 使用官方宿主机部署，核心身份、用户与系统服务在本地运行，不依赖官方运营的 SN/BNS 或凭据服务。模型另行配置。主分支已合并本次实际部署验收的官方 BuckyOS `9357c1fb747925655bef2370579928d5c36a13e0`。

## 合并与源码范围

| 提交 | 内容 |
| --- | --- |
| `e245e70d1` | 撤销旧私有 IP-origin 登录、Desktop IP 自动探测和 scheduler 版本记录重写，恢复官方行为 |
| `2730e45de` | 移除自定义 Worker 拼装、IP 配置及开发域名重签脚本，将外层 Docker 方案归档 |
| `94b332550` | 无冲突合并已部署验证的官方 `9357c1fb7479` |
| 部署与迁移文档提交 | 添加本说明、部署和数据库迁移手册，更新 README 入口及部署记忆 |

合并后的 `src/`、`build.py`、`devenv.py`、`build_aios`、`build_exttool` 和 `pyproject.toml` 与该官方提交一致，没有新增产品逻辑、协议、共享类型或第三方依赖。站点差异位于运行配置、部署文档与归档的旧容器文件，不通过修改鉴权或身份语义实现。

本次合并对象是已验收的提交快照。后续上游提交需要另行构建验证，不能把本记录解释为已经部署所有后续 main 更新。

## 文档入口

| 文件 | 内容 |
| --- | --- |
| [宿主机部署流程](host-native-deployment.md) | 官方构建/启动入口、离线激活、DNS/CA、目录、日常更新与验收 |
| [数据库与整套实例迁移](local-database-migration.md) | 一致停机备份、Sled/SQLite/对象块、实例卷、身份与镜像、迁机及升级回退 |
| README.md、README_zhCN.md | fork 部署与迁移文档的发现入口 |
| memory/local-deployment.md | 当前已验证的部署事实与维护约束 |
| docker/README.md、docker/REPRODUCE.md、docker/domain/README.md | 旧外层容器和网络封装的归档说明，指向当前原生部署 |

部署口令、私钥、令牌、完整代理配置、数据库、运行卷和镜像不提交 Git。`.workflow/`、`doc/research/` 和 `docker/locks/` 的既有本地资料未加入此次提交。

## 已部署的版本与配置

- BuckyOS：`0.7.0+build261118.HEAD.9357c1fb7479`，SDK/Tool `0.7.191`。
- cyfs-gateway：官方 `f0ef92f271eb91e16c590eae32cde66ae59e60bd`。
- 核心服务：官方 systemd 服务直接运行宿主机程序；应用使用宿主机 Docker。
- 域名：`buckyos.ncu.bin-go.cc`；Owner=Zone 为 `did:web:buckyos.ncu.bin-go.cc`，管理员为 `devtest`。
- 运行目录：`/opt/buckyos` 从 `/mnt/data/buckyos/runtime` bind mount；Docker 数据目录为 `/mnt/data/buckyos-host-docker`。
- 身份与证书：官方 `buckyos provision` 生成随机密钥及真实签名文档，独立本地 CA；不复用公开开发 seed。
- 域名解析：cyfs-gateway 自带 local_dns/dns 提供校内 UDP/TCP DNS 与签名引导 TXT，配置在 `etc/post_gateway.yaml`、`etc/campus-dns.toml`。

首次本地独立首装经授权弃用旧开发数据。后续本次上游更新保留当前身份、账号和数据库；没有重新激活或用测试副本数据库覆盖正式实例。

构建使用官方 `devenv.py`、`buckyos-build.py` 与 devkit 入口，部署使用官方 `buckyos-update`。宿主机已准备 Rust、Node/pnpm、CMake、Clang 等构建工具。曾尝试的额外 Docker 构建环境已停用；保留的旧 Dockerfile 未在本次更新中重新验收。

官方 `paios/aios` 与 `paios/exttool` 按不可变 digest 核对后更新。前者发布 revision 为 `a475bd5`，与9357相关Worker源码路径无差异；后者 revision 为9357。node-daemon只使用本地配套镜像，日常启动不拉取镜像。标准Agent工具齐全，浏览器/Office等可选扩展尚未齐备。

站点身份与网络配置独立保留，包括模块更新会覆盖的 `bin/node-active/active_config.json`。已有 systemd、bind mount 与 Docker 存储布局继续使用，没有增加常驻启动封装或反向代理。BuckyOS 专用 ingress-bypass 服务已退出当前流程；既有 sing-box 只对校内域名做 DNS 转发，不是 BuckyOS 必需组件。

## 验证及限制

本次官方原生构建通过，分发CLI在冷缓存、断网环境中可启动。已用当前实例的一致停机副本断网启动新版，验证原账号、SSO、DNS、已有Agent模板更新及工具基线；随后在正式目录更新并再次验证，包括systemd重启后的持久性。15个宿主机运行程序与新构建产物的SHA-256一致。

选择的Python测试最终132通过、2跳过；Rust认证、调度、libopendan与opendan测试合计332通过、18忽略、1失败。失败项为官方 `browser::screen_tests::what_pages_show_outside_themselves_is_asked_of_the_screen`：重连时多回放一个confirm弹窗请求，在普通用户和root环境均可复现，未改动或跳过断言。未宣称全量Cargo测试通过。

`buckyos system info`仍读取首次安装的旧构建记录，官方文件更新未刷新它；实际程序版本可用 `node_daemon --version` 核对，没有手改数据库版本记录。模型未配置，未验证真实模型回复、可选浏览器/Office能力、整机重启、跨架构或任意跨版本数据库迁移。

本机证据保留于 `/mnt/data/buckyos-campus-20261117-200648/`（首装与恢复）及 `/mnt/data/buckyos-upgrade-20261118-003339/`（构建与更新）。升级前最终一致快照在后者的 `final-snapshot/`，含运行目录、实例卷、恢复材料与清单；这些受保护资料不随Git发布。

内置cmd_skills随官方源码合并与分发更新。本 fork 未改变官方CLI/API/配置协议，手册接口和示例无需另加私有变体；新增的是站点操作文档。真实Provider、其他平台与可选扩展仍按各自手册另行验收。
