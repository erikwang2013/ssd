// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/elevation.dart';
import 'core_client/ipc_transport.dart';
import 'core_client/protocol.dart';
import 'features/scan/scan_page.dart';
import 'util/errors.dart';
import 'util/format.dart';

/// 吉祥物资源路径（与应用图标同款形象的小盾）。
const _mascotAsset = 'assets/xiaodun.png';

/// 提权入口支持的平台（三平台同一分支；移动端 M4 无此路径）。
const _elevationPlatforms = {
  TargetPlatform.windows,
  TargetPlatform.macOS,
  TargetPlatform.linux,
};

class HomePage extends StatefulWidget {
  const HomePage({
    super.key,
    required this.client,
    this.onClientReplaced,
    this.elevationLauncher,
    this.elevationConnect,
  });

  final CoreClient client;

  /// 扫描页特权重启客户端后的换用回调（见 main.dart）。
  final void Function(CoreClient client)? onClientReplaced;

  /// 测试注入缝（提权器启动/会话连接）：null = 真实现（UAC/osascript/pkexec + TCP）。
  final ElevationLauncher? elevationLauncher;
  final ElevationSessionConnector? elevationConnect;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  /// 生效中的客户端（提权入口换用新 client 后随之更新；`widget.client` 要等父级重建，
  /// 换用后立刻 `_reload()` 必须用新引用）。
  late CoreClient _client = widget.client;

  late Future<List<DeviceInfo>> _devices;

  /// 提权会话目录（成功建立后由 [elevateSession] 交出，dispose 时清理）。
  Directory? _elevationDir;

  /// 提权会话建立中（认证框可能久置）：按钮禁用 + 「等待授权…」。
  bool _elevationPending = false;

  /// 提权失败文案 / macOS 已提权仍空列表的 FDA 提示（随空列表态展示）。
  String? _elevationNotice;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  @override
  void didUpdateWidget(HomePage oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 父级换用新 client（扫描页提权重启后经 main.dart setState）而本页尚未用它：状态跟随，
    // 设备表按新 client 重列（提权后枚举结果可能不同）。本页自己的提权入口已就地重列，
    // 此处 identical 即跳过（不重复请求）。
    if (!identical(widget.client, _client)) {
      _client = widget.client;
      _reload();
    }
  }

  @override
  void dispose() {
    final dir = _elevationDir;
    if (dir != null) cleanupElevationDir(dir);
    super.dispose();
  }

  bool get _supportsElevation =>
      _elevationPlatforms.contains(defaultTargetPlatform);

  void _reload() {
    setState(() {
      _devices = _client.listDevices();
    });
  }

  void _openScan(DeviceInfo device) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ScanPage(
          client: _client,
          device: device,
          onClientReplaced: widget.onClientReplaced,
        ),
      ),
    );
  }

  /// 首页提权入口（缺陷②：Windows 非提权时枚举为空 ⇒ 首页无设备 ⇒ 走不到扫描页的 -32001
  /// 引导）。复用扫描页同一条提权流（[elevateSession]：UAC/osascript/pkexec → TCP 提权会话），
  /// 换新 client → 重列设备；取消/超时 ⇒ 既有文案「未获得授权（原因）」。
  /// **真 UAC/授权框路径未验证（需真机）**。
  Future<void> _elevate() async {
    // 打包布局/开发树同源解析：client 记录的实际二进制优先，打包同目录回退。
    final daemonPath = _client.daemonPath ?? packagedDaemonPath();
    if (daemonPath == null) {
      setState(() {
        _elevationNotice = '找不到引擎可执行文件，无法提权（可用 XD_DAEMON_BIN 指定）';
      });
      return;
    }
    setState(() {
      _elevationPending = true;
      _elevationNotice = null;
    });
    // 换用前的会话目录：成功换用后清掉（重复提权不得在系统临时目录累积残留；同 scan 侧口径）
    final oldDir = _elevationDir;
    final CoreClient fresh;
    try {
      fresh = await elevateSession(
        daemonPath: daemonPath,
        ownerPid: pid,
        launcher: widget.elevationLauncher,
        connect: widget.elevationConnect,
        onSessionDir: (sessionDir) => _elevationDir = sessionDir,
      );
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _elevationPending = false;
        _elevationNotice = switch (e) {
          ElevationDeniedException() ||
          ElevationTimeoutException() => '未获得授权（$e）',
          _ => describeCoreError(e),
        };
      });
      return;
    }
    if (!mounted) {
      // 页面已销毁：提权 daemon 虽由 --owner-pid 监督兜底，显式关掉更直接
      unawaited(fresh.close().catchError((Object _) {}));
      // dispose 先于会话目录交出（彼时 _elevationDir 仍为 null）⇒ 在此补清，否则残留
      final dir = _elevationDir;
      if (dir != null) cleanupElevationDir(dir);
      return;
    }
    final old = _client;
    // 旧（非提权）daemon 是 UI 子进程：显式关闭（等价让它退出），再换新 client。
    unawaited(old.close().catchError((Object _) {}));
    setState(() {
      _client = fresh;
      _elevationPending = false;
      // osascript 提权 ≠ FDA：已提权仍列不到设备 ⇒ 指路系统设置（§9/§10）
      _elevationNotice = defaultTargetPlatform == TargetPlatform.macOS
          ? kMacosFdaHint
          : null;
    });
    if (oldDir != null) cleanupElevationDir(oldDir);
    widget.onClientReplaced?.call(fresh);
    _reload();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        leading: Padding(
          padding: const EdgeInsets.all(8),
          child: Image.asset(_mascotAsset),
        ),
        title: const Text('小盾 · 选择设备'),
      ),
      body: FutureBuilder<List<DeviceInfo>>(
        future: _devices,
        builder: (context, snapshot) {
          if (snapshot.connectionState != ConnectionState.done) {
            return const Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Image(image: AssetImage(_mascotAsset), width: 96),
                  SizedBox(height: 16),
                  CircularProgressIndicator(),
                ],
              ),
            );
          }
          if (snapshot.hasError) {
            return Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text('读取设备失败：${snapshot.error}'),
                  const SizedBox(height: 12),
                  ElevatedButton(onPressed: _reload, child: const Text('重试')),
                ],
              ),
            );
          }
          final devices = snapshot.data ?? const <DeviceInfo>[];
          if (devices.isEmpty) return _emptyState();
          return ListView.separated(
            itemCount: devices.length,
            separatorBuilder: (_, _) => const Divider(height: 1),
            itemBuilder: (context, index) => _DeviceTile(
              device: devices[index],
              onTap: () => _openScan(devices[index]),
            ),
          );
        },
      ),
    );
  }

  /// 空列表态：三平台展示提权入口（平台支持时）+ 结果提示（失败文案 / macOS FDA 提示）。
  Widget _emptyState() {
    final notice = _elevationNotice;
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const Image(image: AssetImage(_mascotAsset), width: 128),
          const SizedBox(height: 16),
          const Text('未发现设备'),
          const SizedBox(height: 6),
          const Text(
            'M1 前可用 XD_IMAGE 注册镜像文件',
            style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
          ),
          if (_supportsElevation) ...[
            const SizedBox(height: 24),
            const Padding(
              padding: EdgeInsets.symmetric(horizontal: 32),
              child: Text(
                '物理磁盘等设备可能因权限不足未列出；以管理员身份重启引擎后再扫描。',
                textAlign: TextAlign.center,
                style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
              ),
            ),
            const SizedBox(height: 12),
            FilledButton(
              onPressed: _elevationPending ? null : _elevate,
              child: Text(_elevationPending ? '等待授权…' : '以管理员身份重启引擎'),
            ),
          ],
          if (notice != null) ...[
            const SizedBox(height: 12),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 32),
              child: Text(
                notice,
                textAlign: TextAlign.center,
                style: TextStyle(
                  fontSize: 12,
                  color: Theme.of(context).colorScheme.error,
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _DeviceTile extends StatelessWidget {
  const _DeviceTile({required this.device, required this.onTap});

  final DeviceInfo device;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: const Icon(Icons.storage),
      title: Text(device.name),
      subtitle: Text('${device.kind} · ${formatBytes(device.sizeBytes)}'),
      onTap: onTap,
    );
  }
}
