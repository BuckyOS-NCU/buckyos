此主机为原生部署：官方 buckyos.service 直接运行 node-daemon 及系统服务，cyfs-gateway 接收80/443并提供本地DNS，应用使用宿主机Docker。运行目录 /opt/buckyos 从 /mnt/data/buckyos/runtime bind mount；Docker数据目录为 /mnt/data/buckyos-host-docker。当前运维说明见 doc/host-native-deployment.md，数据库迁移见 doc/local-database-migration.md；Compose不是本机启动入口。

当前 Owner DID = Zone DID = did:web:buckyos.ncu.bin-go.cc，Device DID = did:web:ood1.buckyos.ncu.bin-go.cc。本地管理员user_id为devtest，属于Admin；它不是旧did:bns:devtest开发身份，不使用公开seed密钥。当前Agent为jarvis.buckyos.ncu.bin-go.cc，AgentRoot为data/home/devtest/agents/<agent_id>/。旧业务数据经用户授权未迁入新实例。

官方 buckyos provision status/check/activate 支持single OOD、did:web离线首装，不需要SN/BNS或在线Web激活。它生成随机Owner/Device密钥并强制保存Owner恢复副本。口令经秘密输入提供，不必改make_config或复用开发seed。命令只接受未激活的新目录；恢复旧实例或迁移数据库时不能重新activate。

DNS配置为etc/campus-dns.toml和etc/post_gateway.yaml，复用cyfs-gateway内置local_dns/dns，在192.168.0.64:53提供UDP/TCP解析；主域名与通配子域名指向本机，TXT包含签名BOOT/DEV与PKX。客户端需使用校内DNS或条件转发。本机既有sing-box配置仅对该域名后缀转发到本地DNS；迁到没有sing-box的主机不需要为BuckyOS安装它。

TLS使用独立校内CA，无外部ACME依赖。客户端需安装公开CA信任，不能把DID验签或测试浏览器SPKI pin当作浏览器已信任CA。恢复材料在/mnt/data/buckyos/recovery/：owner-recovery.pem、campus-ca-key.pem是私钥，campus-ca.crt可公开分发；目录0700、私钥0600，不能提交Git。

数据库不全是SQLite：local/system_config/是Sled真相库，不能因为路径带local就漏掉。完整迁移需保留etc、security、local、data、storage、相关bin/pkgs/分发及独立实例卷；named-store的数据库和对象块必须一起带走。etc/backup.py是身份备份工具，不是整套数据库备份。停保活服务后还要停止应用容器以得到一致快照；保持同一身份和密钥，先验证同版本恢复再做schema升级。

数据盘UUID为a08bd4c5-4244-4ed6-a5cc-cf134af698e2，挂载/mnt/data。fstab维护运行目录bind mount，BuckyOS与Docker有存储顺序drop-in。保留规范/opt/buckyos路径，符号链接布局会触发当前Deno launcher执行路径权限问题。迁机时调整新磁盘/IP和DNS监听，不盲目覆盖整份fstab或Docker数据目录。

Worker为配套官方paios/aios，无本地aios覆盖，node-daemon不拉取镜像。工具来自运行目录tools/并以buckyos-tools卷挂载；实例卷含独立/opt/buckyos/instance数据，不能只带AgentRoot而遗漏它们。更新OpenDAN需核对Worker实际程序，不以宿主机文件推断容器版本。

当前已部署官方BuckyOS 9357c1fb747925655bef2370579928d5c36a13e0、网关f0ef92f271eb91e16c590eae32cde66ae59e60bd，实际build261118.HEAD.9357c1fb7479，SDK/Tool0.7.191。从干净官方工作树原生构建，使用官方buckyos-update保留身份和数据更新；源码保留于/mnt/data/buckyos-upgrade-20261118-003339/projects/。fork的main已合并经过部署验收的官方9357c1fb7479，src及官方构建入口与该提交一致。旧私有IP登录、Desktop IP探测和scheduler版本重写已撤销，外层Docker方案归档。学校要求核心系统和身份不依赖任何官方运营服务，模型另行配置；当前未配置AI Provider，不能把工具基线通过说成真实模型回复通过。

官方文件更新未刷新system/buckyos_info，CLI system info可能仍显示初次安装的旧build；核对实际程序用node_daemon --version和运行进程/产物校验，不手改版本记录。当前Linux Full版Agent使用官方Worker容器；最新native_agent_node仅对关闭container能力的产品路径生效，切换默认host执行位置不会改成原生Agent。额外Docker构建环境已停用，正常启动不依赖仓库外验收脚本。

新节点先登记system-config，再登记其他服务。必须等待完整网关服务目录并验证认证RPC，端口开放不能代替就绪；过早进入MessageHub会503。旧BuckyOS专用ingress-bypass脚本/nft/service已撤除；保留通用sing-box回程规则。旧Nginx和外层BuckyOS容器不参与现行部署。

维护约束：源码优先跟随官方；域名/身份、用户RBAC和Agent交互语义不为部署随意改写。站点定制限运行配置、存储、网络与有证据的数据迁移；不伪造官方API成功响应、不删鉴权、不替换schema字符串。历史构建需核对家族Cargo锁文件与SDK配套；devkit的pnpm install --no-lockfile会忽略单独复制的pnpm锁文件。
