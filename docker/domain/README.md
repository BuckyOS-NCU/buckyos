# 公网域名部署

这些文件来自现有 `bucykos.ncu.bin-go.cc` 部署。HTTPS 由 BuckyOS 内的 cyfs-gateway 终止，Nginx 仅在 `buckyos_default` 内网负责 HTTP→HTTPS 跳转。不会上传现有私钥、证书或登录凭据。

## 配置自己的域名

先按 [REPRODUCE.md](../REPRODUCE.md) 启动 dev Zone。将本目录 `gateway/post_gateway.yaml`、`conf.d/default.conf` 中的 `bucykos.ncu.bin-go.cc` 全部替换为自己的域名，包括子域名、ACME hosts 和 TLS hosts。为主域名及使用到的子域名配置 DNS，并让公网 TCP 80/443 到达 BuckyOS 主机；HTTP-01 要求公网 80 可用。

`compose.yaml` 使用外部网络 `buckyos_default`，与根目录 Compose 的 `name: buckyos` 一致。若更改项目名，也要改这里的网络名称。

```bash
docker compose -f docker/domain/compose.yaml up -d
docker cp docker/domain/gateway/post_gateway.yaml buckyos:/opt/buckyos/etc/post_gateway.yaml
```

## 绑定 dev Zone 的公开 hostname

`bind-dev-domain.mjs` 复用已在本机执行过的 owner 签名流程，仅支持 `devtest` / `did:web:test.buckyos.io`，保留 Zone DID，核对预置开发 owner 公钥后更新公开 hostname。它先通过当前密码获取 sudo token，将 `boot/config` 备份到容器 `/tmp/domain-binding-<时间戳>/`；`--prepare` 只生成候选，`--apply` 同步 system-config 和本地启动文件。它适用于本项目 dev 初始化流程，不能用于 release 的真实 owner 身份。

```bash
docker cp docker/domain/bind-dev-domain.mjs buckyos:/tmp/bind-dev-domain.mjs
read -r -s -p 'devtest 当前密码: ' BUCKYOS_BIND_PASSWORD
printf '\n'
printf '%s' "$BUCKYOS_BIND_PASSWORD" | docker exec -i buckyos node /tmp/bind-dev-domain.mjs --prepare your.example.com
```

检查输出中的 hostname 和 Zone DID。确认候选后应用：

```bash
printf '%s' "$BUCKYOS_BIND_PASSWORD" | docker exec -i buckyos node /tmp/bind-dev-domain.mjs --apply your.example.com
unset BUCKYOS_BIND_PASSWORD
docker compose --env-file docker/deployment.local.env up -d --no-build --force-recreate
```

应用动作涉及多个持久位置，发生中途失败时需根据 `/tmp/domain-binding-<时间戳>/` 备份恢复或完成同步；不要在修改前丢弃备份。重启后确认 `https://your.example.com/`、`https://sys.your.example.com/` 到主域名的跳转、桌面登录以及 Jarvis 页面和桌面链接。

证书和私钥由网关新建，保存在 BuckyOS 运行卷中。新增应用子域名时同步更新 `acme.hosts` 与 TLS `hosts`；DNS 通配符不等于 TLS 通配符证书。

## sing-box TUN 回程（仅按需使用）

`gateway/` 还保存原主机的 `buckyos-ingress-bypass`、nftables 和 systemd 单元。普通直连主机无需安装。使用 sing-box TUN 且 Web 入站回复被代理截获时，先核对：

- `ingress-bypass.nft` 中入口接口 `eth0` 是否为新主机实际接口。
- 路由优先级 `5001`、标记 `0x100` 是否空闲，主路由表是否能直接返回客户端。
- 新主机 sing-box 服务名称是否与单元中的依赖一致。
- 已安装 `nft`、`ip`、`rg`；脚本会拒绝占用的路由优先级。

核对后安装：

```bash
sudo install -m 755 docker/domain/gateway/buckyos-ingress-bypass /usr/local/sbin/buckyos-ingress-bypass
sudo install -m 644 docker/domain/gateway/ingress-bypass.nft /etc/buckyos-ingress-bypass.nft
sudo install -m 644 docker/domain/gateway/buckyos-ingress-bypass.service /etc/systemd/system/buckyos-ingress-bypass.service
sudo systemctl daemon-reload
sudo systemctl enable --now buckyos-ingress-bypass.service
```

检查 `nft list table inet buckyos_ingress`、`ip -4 rule show`，并从主机外测试 80/443。此规则针对 IPv4 回程，不代表完成 IPv6 入站验证。
