# 南昌大学个人资料持久化

## 1. 概述

Control Panel 在学校统一认证成功时保存一份经 IAADP 验证的学校资料。使用原有 BuckyOS 匿名化账号标识关联记录。当前账号本人通过受限的 `user.get.school_profile` 查看适合显示的字段。

## 2. 数据分类

持久数据是每个账号最近一次已核验的校方档案及其核验时间。登录挑战、回调授权码、票据和响应缓冲仅用于处理请求。旧账户在下次真实 CAS 登录时补齐档案。

## 3. 存储方式

由 BuckyOS 管理 Control Panel 的命名 RDB `ncu-school`，只使用 `user_data` 分区。当前驱动是工作区已有的 SQLite，实例连接由平台解析。服务对专用目录和文件分别施行 `0700` 和 `0600`。该档案不复用公共资料或 DID 文档。

## 4. 数据表

表 `ncu_school_dossiers` 按原有匿名化账号标识保存整份最新快照。

| 列 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `account_id` | TEXT | 主键，非空 | 现有 BuckyOS 账号标识 |
| `dossier_json` | TEXT | 非空 | 经字段许可名单筛选的 IAADP 业务档案 |
| `verified_at_ms` | INTEGER | 非空 | 收到验证结果的 Unix 毫秒 |

保存协议中有文档的业务字段及 `identities` 明细，保留已识别的标量类型、前导零和空值。仅返回当前身份的必要文字字段。授权码、短期认证票据、有效期、未知字段和未知嵌套字段不入库。

## 5. 版本

平台实例版本及 SQLite `user_version` 都从 1 起步。新版本需要显式迁移。

## 6. 升级与回退

未登记实例的旧 Zone 继续使用原有 CAS 登录。已登记 Zone 在成功回调写入时初始化空库。存储初始化及档案写入在事务内处理。已配置实例写入失败时拒绝本次登录并撤销新会话。发布回退无需改变已有 BuckyOS 账号，档案文件按数据保留处理。读取失败时常规个人资料仍可访问。

现有 Zone 应由有权限的运维在 `services/control-panel/spec` 的 `spec_config.rdb_instances` 下以保留原 spec 字段的版本保护更新登记：

```json
{
  "ncu-school": {
    "backend": "sqlite",
    "version": 1,
    "connection": "sqlite:$partdata/ncu-school/school.sqlite",
    "partitions": ["user_data"]
  }
}
```

部署工具应先用已授权的 `SystemConfigClient.get` 读取 spec 和 revision。用一次 `exec_tx` 带该 revision 作为 `main_key`，通过 `KVAction::SetByJsonPath` 更新 `spec_config.rdb_instances.ncu-school`。冲突时重新读取并检查。同名的不同配置须先审核，避免覆盖另一份档案。

## 7. 扩展规则

账号标识及核验时间的含义固定。新增已核验业务字段须单独审核，并同步更新内部许可名单。前端个人信息投影只包括显示所需字段，无编辑或跨账号读取入口。

## 8. 查询方式

登录时按 `account_id` 更新最新快照，个人信息页按同一主键读一行。相同账号较早完成的验证不能覆盖较新的快照。无列表扫描或公开查询。
