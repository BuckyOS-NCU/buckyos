`boot_gateway.yaml` 中 `type: json_map` 使用 cyfs-gateway-lib 的 JsonMap 后端。其 `json_value_to_collection_value` 将 JSON 布尔值转换为 DSL 的字符串，而 DSL `eq` 默认执行严格类型比较。因此 JSON 文件保持布尔值 `true` / `false`，在网关脚本中比较时使用字符串字面量 `"true"` / `"false"`。

此行为已通过当前 cyfs-gateway `main` 源码及实际二进制验证。cyfs-process-chain 的独立 JSON 文件集合实现保留布尔类型，不能据此推断 gateway 的 json_map 值类型；更新依赖时应重新检查该转换。
