#!/usr/bin/env node
// postinstall：尽力下载对应平台二进制到 ~/.ncc/bin/ncc[.exe]（失败不阻塞安装）。
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const HOME = process.env.HOME || os.homedir();

// ⚠️ Windows 下必须带 .exe 后缀 —— 与 bin/ncc.js 里的 EXE 保持同一条约定。
//    没有后缀的 PE 文件在 Windows 上 spawn 会直接 ENOENT（实测 2×2 隔离确认：
//    目录无关、扩展名是唯一变量）。两处必须同时改，否则 postinstall 存成
//    ncc.exe、启动器却去找 ncc（或反过来），表现是"刚装完就要重新下载"。
const EXE = process.platform === 'win32' ? '.exe' : '';
const dest = path.join(HOME, '.ncc', 'bin', 'ncc' + EXE);

// 已存在则不重复下载
try {
  fs.accessSync(dest, fs.constants.X_OK);
  process.exit(0);
} catch { /* 继续 */ }

// 仓库本地已有 Rust 构建则无需下载
try {
  const dev = path.join(__dirname, '..', '..', '..', 'cli', 'target', 'release', 'ncc' + EXE);
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
  console.error('ℹ ncc 二进制暂未下载（首次运行 npx @fusedmodel/ncc-cli 时会自动下载）。');
  process.exit(0);
}
try { fs.chmodSync(dest, 0o755); } catch { /* ignore */ }
process.exit(0);
