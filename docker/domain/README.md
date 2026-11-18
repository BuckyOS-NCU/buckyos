# 域名配置与旧入口示例

当前学校部署复用官方 cyfs-gateway，使用校内 DNS 与独立本地 CA。具体文件、访问条件和验证步骤见[宿主机部署流程](../../doc/host-native-deployment.md)。核心身份通过官方 `buckyos provision` 离线生成；自有域名部署不必依赖官方运营的 SN/BNS 服务。

域名必须与实际 Zone、Owner 和设备文档一致。新安装生成随机密钥；迁移已有实例保留原身份和密钥。不要伪造 SN/BNS 响应、复用公开 dev seed，或仅修改旧 Zone 的 hostname。

本目录的 `gateway/post_gateway.yaml` 和 `conf.d/default.conf` 是旧公网 HTTP-01 与 HTTP→HTTPS 跳转示例，使用 `zone.example.com` 占位域名，不是当前校内 DNS/私有 CA 配置。当前实例不运行这里的外层 Nginx。`gateway/buckyos-ingress-bypass*` 和 `ingress-bypass.nft` 是原主机 TUN 网络适配的历史参考，已退出当前 BuckyOS 启动流程。

客户端需要正确解析主域名与应用子域名，并安装校内公开 CA 信任。DNS 通配记录不等于 TLS 通配证书，身份验签也不等于浏览器证书信任。验收应包含 HTTPS、登录/SSO 刷新、DID 文档身份与签名、Agent 与重启后的数据保留。
