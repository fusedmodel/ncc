#!/usr/bin/env node
// ncc 启动器：按顺序定位平台二进制并转发参数 / stdio / 信号。
//   1) NCC_BIN 环境变量
//   2) 随包分发的 vendor/ncc-<os>-<arch>
//   3) ~/.ncc/bin/ncc（install.sh / cargo install 安装）
//   4) 仓库本地 Rust 构建 cli/target/release/ncc（开发用）
// 若都不存在，则尝试从发布源下载到 ~/.ncc/bin/ncc。
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const PKG_ROOT = path.join(__dirname, '..');
const REPO_ROOT = path.join(PKG_ROOT, '..', '..');
const HOME = process.env.HOME || os.homedir();
const NCC_BIN = path.join(HOME, '.ncc', 'bin', 'ncc');

function platformFile() {
  const p = os.platform();
  const a = os.arch();
  const osn = p === 'darwin' ? 'darwin' : p === 'linux' ? 'linux' : p === 'win32' ? 'windows' : p;
  const arch = a === 'x64' ? 'x86_64' : a === 'arm64' ? 'arm64' : a;
  return `ncc-${osn}-${arch}${osn === 'windows' ? '.exe' : ''}`;
}

function candidates() {
  const list = [];
  if (process.env.NCC_BIN) list.push(process.env.NCC_BIN);
  list.push(path.join(PKG_ROOT, 'vendor', platformFile()));
  list.push(NCC_BIN);
  list.push(path.join(REPO_ROOT, 'cli', 'target', 'release', 'ncc'));
  return list;
}

function firstBin() {
  for (const p of candidates()) {
    try {
      fs.accessSync(p, fs.constants.X_OK);
      return p;
    } catch {
      /* next */
    }
  }
  return null;
}

function download(url, dest) {
  const r = require('child_process').spawnSync(
    process.execPath,
    [path.join(__dirname, 'download.js'), url, dest],
    { stdio: 'inherit' },
  );
  return r.status === 0;
}

let bin = firstBin();
if (!bin) {
  const base = process.env.NCC_RELEASE_BASE || 'https://github.com/ncc-ai/ncc/releases/latest/download';
  const url = `${base}/${platformFile()}`;
  console.error('→ 首次使用，正在下载 ncc…');
  console.error('  ' + url);
  if (download(url, NCC_BIN)) {
    try { fs.chmodSync(NCC_BIN, 0o755); } catch { /* ignore */ }
    bin = firstBin();
  }
}

if (!bin) {
  console.error('✗ 未找到 ncc 二进制。请先安装：');
  console.error('  curl -fsSL https://ncc.ai/install.sh | sh');
  console.error('  或 cargo install --path cli');
  process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: 'inherit' });
child.on('error', (e) => {
  console.error(`✗ 执行 ncc 失败：${e.message}`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code === null ? 1 : code);
});
