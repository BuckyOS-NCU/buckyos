# 数据库与整套实例迁移

本手册针对当前 Linux x86_64 原生部署。同版本、同架构的停机备份与离线恢复启动已实测；另已验证本次0.7.0不同构建之间的上游更新。其他跨版本升级、换域名和换身份需要额外验证。

## 为什么不能只复制几个数据库文件

当前系统的数据不是一个独立 SQL 库：

| 数据 | 必须保留的范围 |
| --- | --- |
| 用户、RBAC、Agent定义、调度及服务状态真相源 | `local/system_config/` 整个 Sled 存储，包含 db/conf/blobs 等文件 |
| 业务 SQLite 数据库 | `data/` 及 `local/` 中的数据库和对应 WAL/SHM；含消息、任务、AICC、HomeStation、Workflow等 |
| 命名对象、消息正文、附件及保留关系 | `storage/` 整体，包含 named_store.json、各 store 的数据库和块文件 |
| 用户文件与 Agent 状态 | `data/home/` 等用户数据，包含 AgentRoot 与会话文件 |
| 已安装应用的本地包 | `bin/pkgs/`、对应包链接和内置 PIKG 缓存 |
| 应用容器内部工作目录 | Docker 的 `buckyos-instance-*` 卷；不在运行目录的普通文件树内 |
| 身份、启动配置和服务私钥 | `etc/`、`security/`、`local/identity/` |
| Owner 恢复与 CA 签发私钥 | `/mnt/data/buckyos/recovery/`，独立于运行目录 |

`local/system_config/` 虽然叫 local，却是当前实际使用的真相库，不可当缓存省略。SQLite 也不能在写入过程中只复制主 .db、遗漏 WAL。命名存储的块文件与数据库必须同一时点保存。仓库 `etc/backup.py` 是身份备份工具，不能代替以上整套迁移。

最省心的方式是：**停机后备份整个运行目录（可排除日志）、实例卷和恢复材料**，保留UID/GID、权限与符号链接。

## 本次已经生成并恢复验证的快照

目录：`/mnt/data/buckyos-campus-20261117-200648/migration-backup-20261117-235500/`。

- `runtime.tar`：约3.3GB，包含运行程序、配置、身份、数据库、对象及工具；排除日志。
- 两份实例卷 tar：归属和名称在 `manifest.json` 的 `instance_volumes` 中。
- `paios-images.tar`：当前配套 Worker/ExtTool 镜像，不需要从外网拉取。
- `recovery/`：Owner 恢复私钥、CA签发私钥和公开CA。
- `manifest.json`：版本、身份、卷映射、大小、SHA-256和17个SQLite完整性检查记录。

已经执行：停止原生服务与应用、生成一致快照、解包到独立目录、官方 provision status 检查、17个 SQLite quick_check，以及在无外网隔离环境启动恢复副本并用原账号登录。当前线上服务在备份后已恢复运行。

整个目录包含私钥和用户数据，应以受保护方式转移，并在其他存储介质留恢复副本。不要提交 Git。

## 以后如何做新快照

以下为仓库根目录下的操作顺序，不依赖在线 Python 包安装：

1. 记录程序/SDK/镜像版本、运行目录和当前实例卷映射，创建权限0700的快照目录。
2. 停止保活服务，再用官方脚本停止应用容器：

```bash
sudo systemctl stop buckyos.service
BUCKYOS_ROOT=/opt/buckyos python3 src/stop.py
```

确认关键端口已释放、没有运行中的 BuckyOS 应用容器。单停 node-daemon 不会自动停止 Docker 管理的应用。

3. 以有权限读取私钥的账号归档运行目录，保留数值属主、权限和扩展属性：

```bash
snapshot_dir=/mnt/data/buckyos-snapshot-YYYYMMDD-HHMM
sudo install -d -m 700 "$snapshot_dir"
sudo tar --xattrs --acls --numeric-owner --exclude=./logs -cpf "$snapshot_dir/runtime.tar" -C /opt/buckyos .
sudo cp -a /mnt/data/buckyos/recovery "$snapshot_dir/recovery"
```

对记录的每个 `buckyos-instance-*` 卷，用 `docker volume inspect <卷名>` 获取 Mountpoint，再用相同 tar 选项归档该目录内容，每卷一个文件并登记名称。不需要备份整个 Docker data-root；`buckyos-tools` 绑定到运行目录的 tools，已随 runtime 归档。

4. 用 `docker save` 导出当前配套镜像，或复用已核对且未改变的离线镜像包；记录实际镜像ID和归档SHA-256。
5. 将 tar 权限设为0600，保存版本、卷映射、恢复材料位置和校验清单。
6. 启动原服务，并验证一次登录、文件/消息、Agent和业务状态：

```bash
sudo systemctl start buckyos.service
python3 src/check.py
```

## 搬到另一台机器

目标先准备兼容的 Linux、Docker 和基础运行条件，使用相同版本与架构；不要让两台机器同时在同一网络以相同设备身份运行。本次宿主机原生构建的node-daemon最高要求 `GLIBC_2.39`（已检查ELF版本符号），不能直接搬到更旧glibc的发行版；应使用兼容主机或在目标环境用官方入口重新构建配套程序，再验证恢复副本。

1. 复制完整快照目录，逐个校验文件大小和 SHA-256。
2. 使用空的运行目录恢复 `runtime.tar`，保留属主/权限/链接。继续使用规范 `/opt/buckyos`；如果实际数据放其他磁盘，设置 bind mount 并维护 fstab 和服务挂载依赖，不改成符号链接。
3. 恢复 `recovery/` 到受保护目录，保持目录0700、私钥0600。Owner、Zone、Device DID和密钥保持不变，**不运行 provision activate 或 start.py --all/--reinstall**。
4. 使用 `docker load -i <镜像包>` 恢复镜像；按清单以原名称创建实例卷，在没有容器使用它们时，将相应卷 tar 解到各 Mountpoint。不要把旧 `/var/lib/docker` 整个覆盖到新系统。
5. 使用官方 systemd unit；`host-reference/` 是原机器配置参考，不应直接覆盖新机器整个 fstab 或 Docker配置。调整新磁盘UUID、实际数据路径和Docker挂载依赖。
6. IP改变但域名不变时，更新 `etc/campus-dns.toml` 的地址、`etc/post_gateway.yaml` 的LAN DNS监听地址及校内DNS转发目标。保留域名身份和原证书/CA，不需要因换IP重签身份。
7. 安装校内 CA 信任并让客户端使用新的校内DNS地址。迁移到没有 sing-box 的机器不必安装它，直接使用已有学校DNS或网关的本地DNS。
8. 先在隔离网络进行恢复启动验证，再停止源实例并切换正式访问入口。检查登录刷新、用户权限、文件、消息、Agent、对象读取及重启持久性。

可以用 `buckyos provision status --root <恢复目录>` 做只读身份材料检查。它不证明全部数据库和业务已正确恢复，所以还要完成真实启动与业务验证。

## 升级版本时的数据库迁移

2026-11-18已将原0.7.0构建更新到官方 `9357c1fb7479`，网关为 `f0ef92f271eb`，Tool从0.7.170更新为0.7.191。先在当前实例的一致副本中断网启动新程序，验证原账号、DNS与已有Agent；随后再次停机生成最终快照，才在正式目录执行相同的官方文件更新。保留现有身份/数据，没有重新激活、手改schema或替换正式库为测试库。线上登录、Agent基线与服务重启均通过。

此次升级前完整回退点为 `/mnt/data/buckyos-upgrade-20261118-003339/final-snapshot/`，含 `runtime/`、`instance-volumes/`、`recovery/` 和 `manifest.json`；清单记录旧版本、镜像ID、卷映射和站点配置摘要。旧镜像仍在本机Docker中，迁移这个目录到另一台机器时还需按清单 `docker save` 旧镜像，不能假设目标已经有这些镜像。这证明本次具体构建组合可升级，不保证任意版本均有自动迁移器。

搬机器和升级schema是两件事。先证明旧版本快照可恢复，再在隔离副本部署配套新版本，核对各服务的正式迁移路径、共享类型与Agent会话协议。当前工程允许breaking change，不能假设所有数据库都有自动升级支持。

没有适用迁移器时，需要针对具体服务设计迁移，保留原库并用摘要、对象ID、记录数及业务请求核对结果。不要改 schema_version 字符串、全局替换DID或删除表来绕过校验。版本回退优先恢复完整旧快照，不让旧程序直接读取已被新版本改写的库。

换域名/Owner属于身份迁移，也不等于数据库搬家；当前 Owner=Zone 的域名DID需要单独处理引用与签名关系。
