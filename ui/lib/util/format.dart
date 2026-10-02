// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
/// 字节数 → '3.0 MB'（1024 进制，1 位小数）。
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

/// 毫秒 → '1.5 s'（不足 1s 显示 '800 ms'）。
String formatElapsed(int ms) =>
    ms < 1000 ? '$ms ms' : '${(ms / 1000).toStringAsFixed(1)} s';
