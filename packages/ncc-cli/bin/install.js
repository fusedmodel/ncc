#!/usr/bin/env node
// postinstall：尽力下载对应平台二进制到 ~/.ncc/bin/ncc（失败不阻塞安装）。
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const HOME = process.env.HOME || os.homedir();
const dest = path.join(HOME, '.ncc', 'bin', 'ncc');

// 已存在则不重复下载
try {
  fs.accessSync(dest, fs.constants.X_OK);
  process.exit(0);
} catch { /* 继续 */ }

// 仓库本地已有 Rust 构建则无需下载
try {
  const dev = path.join(__dirname, '..', '..', '..', 'cli', 'target', 'release', 'ncc');
  fs.accessSync(dev, fs.constants.X_OK);
  process.exit(0);
} catch { /* 继续 */ }

function platformFile() {
  const p = os.platform();
  const a = os.arch();
  const osn = p === 'darwin' ? 'darwin' : p === 'linux' ? 'linux' : p === 'win32' ? 'windows' : p;
  const arch = a === 'x64' ? 'x86_64' : a === 'arm64' ? 'arm64' : a;
  return `ncc-${osn}-${arch}${osn === 'windows' ? '.exe' : ''}`;
}

const base = process.env.NCC_RELEASE_BASE || 'https://github.com/fusedmodel/ncc/releases/latest/download';
const url = `${base}/${platformFile()}`;
const r = spawnSync(process.execPath, [path.join(__dirname, 'download.js'), url, dest], { stdio: 'inherit' });
if (r.status !== 0) {
  console.error('ℹ ncc 二进制暂未下载（首次运行 npx ncc 时会自动下载）。');
  process.exit(0);
}
try { fs.chmodSync(dest, 0o755); } catch { /* ignore */ }
process.exit(0);
