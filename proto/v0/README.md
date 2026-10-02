# 小盾 IPC 契约 v0

- 传输：stdio，每行一条 JSON（LF 结尾），UTF-8。**stdout 仅输出 JSON-RPC 响应行；日志与诊断一律走 stderr。** 空行被忽略（不产生响应）。
- 信封：JSON-RPC 2.0。`id` 为请求方生成的整数，必须原样回显。
- 版本：`protocol = 0`。破坏性变更递增；daemon 与 UI 不匹配时由 `ping` 比对 `protocol` 检出。 M0 不校验信封 `jsonrpc` 字段值。
  `version` 随 workspace 版本更新；仅因发版改动 golden 中的 `version` 不属于契约变更。
- **golden 规则**：`examples/*.json` 是契约唯一事实源。Rust（`crates/xd-core/tests/contract.rs`）
  与 Dart（`ui/test/protocol_test.dart`）两侧测试都对同一组文件断言：解析（decode）须逐字段一致，
  序列化（encode）输出也须与 golden 逐字段一致（`id` 除外，由调用方生成）。改契约必须同步改 golden 与两侧测试。

## 方法

| method | params | result |
|---|---|---|
| `ping` | null | `{"pong": true, "version": "<workspace 版本>", "protocol": 0}` |
| `device.list` | null | `{"devices": [DeviceInfo]}` |

`params` 可省略或显式 `null`；M0 两个方法均无参数。

## DeviceInfo

```json
{"id": "image:test.img", "name": "test.img", "kind": "image",
 "sizeBytes": 4096, "removable": false, "fsGuess": null}
```

- `id`：`image:<路径>`（M0）；M1 起为 `unix:/dev/sdX` / `win:\\.\PhysicalDriveN` / `volume:...`。
- `kind`：`physical` | `volume` | `image`。
- `fsGuess`：探测到的文件系统（M0 恒为 null）。

## 错误

| code | 含义 |
|---|---|
| -32700 | 无法解析的 JSON，或反序列化后不构成合法 Request（M0 简化：两种情况一律 -32700、`id` 回显 null；-32600 保留给未来） |
| -32601 | 方法不存在（`message` 为 `Method not found: <method>`，文案属契约一部分，有测试逐字断言） |
| -32602 | 参数不合法（M0 两个方法均无参，暂不可达，保留） |

## golden 文件

examples/ 下：`ping.request.json`、`ping.response.json`、`device_list.request.json`、
`device_list.response.json`、`error_method_not_found.response.json`。

---

© 2026 erik · https://erik.xyz · erik@erik.xyz
