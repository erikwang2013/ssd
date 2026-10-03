// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/protocol.dart';
import 'features/scan/scan_page.dart';
import 'util/format.dart';

/// 吉祥物资源路径（与应用图标同款形象的小盾）。
const _mascotAsset = 'assets/xiaodun.png';

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.client, this.onClientReplaced});

  final CoreClient client;

  /// 扫描页特权重启客户端后的换用回调（见 main.dart）。
  final void Function(CoreClient client)? onClientReplaced;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late Future<List<DeviceInfo>> _devices;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  void _reload() {
    setState(() {
      _devices = widget.client.listDevices();
    });
  }

  void _openScan(DeviceInfo device) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ScanPage(
          client: widget.client,
          device: device,
          onClientReplaced: widget.onClientReplaced,
        ),
      ),
    );
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
          if (devices.isEmpty) {
            return const Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Image(image: AssetImage(_mascotAsset), width: 128),
                  SizedBox(height: 16),
                  Text('未发现设备'),
                  SizedBox(height: 6),
                  Text(
                    'M1 前可用 XD_IMAGE 注册镜像文件',
                    style: TextStyle(fontSize: 12, color: Color(0xFF8B97AC)),
                  ),
                ],
              ),
            );
          }
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
