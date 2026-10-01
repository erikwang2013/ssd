# 小盾 IPC 契约 v0

- 传输：stdio，每行一条 JSON（LF 结尾），UTF-8。
- 信封：JSON-RPC 2.0。`id` 为请求方生成的整数，必须原样回显。
- 版本：`protocol = 0`。破坏性变更递增；daemon 与 UI 版本不匹配时由 `ping` 检出。
- **golden 规则**：`examples/*.json` 是契约唯一事实源。Rust（`crates/xd-core/tests/contract.rs`）
  与 Dart（`ui/test/protocol_test.dart`）两侧测试都对同一组文件断言，改契约必须同步改 golden 与两侧测试。

## 方法

| method | params | result |
|---|---|---|
| `ping` | null | `{"pong": true, "version": "<crate 版本>", "protocol": 0}` |
| `device.list` | null | `{"devices": [DeviceInfo]}` |

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
| -32700 | 无法解析的 JSON（id 回显 null） |
| -32601 | 方法不存在 |
| -32602 | 参数不合法 |

## golden 文件

examples/ 下：`ping.request.json`、`ping.response.json`、`device_list.request.json`、
`device_list.response.json`、`error_method_not_found.response.json`。
