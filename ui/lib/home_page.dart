import 'package:flutter/material.dart';

import 'core_client/core_client.dart';
import 'core_client/protocol.dart';

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.client});

  final CoreClient client;

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

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('小盾 · 选择设备')),
      body: FutureBuilder<List<DeviceInfo>>(
        future: _devices,
        builder: (context, snapshot) {
          if (snapshot.connectionState != ConnectionState.done) {
            return const Center(child: CircularProgressIndicator());
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
            return const Center(child: Text('未发现设备'));
          }
          return ListView.separated(
            itemCount: devices.length,
            separatorBuilder: (_, _) => const Divider(height: 1),
            itemBuilder: (context, index) =>
                _DeviceTile(device: devices[index]),
          );
        },
      ),
    );
  }
}

class _DeviceTile extends StatelessWidget {
  const _DeviceTile({required this.device});

  final DeviceInfo device;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: const Icon(Icons.storage),
      title: Text(device.name),
      subtitle: Text('${device.kind} · ${formatBytes(device.sizeBytes)}'),
      onTap: () {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('扫描功能将在 M1 接入')));
      },
    );
  }
}

String formatBytes(int bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var value = bytes.toDouble();
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return '${value.toStringAsFixed(1)} ${units[unit]}';
}
