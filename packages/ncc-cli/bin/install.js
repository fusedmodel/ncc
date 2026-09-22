#!/usr/bin/env node
// postinstall：尽力下载对应平台二进制到 ~/.ncc/bin/ncc[.exe]（失败不阻塞安装）。
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

// ⚠️ NCC_HOME 最优先，与 Rust 侧 `config::home_dir()` 保持一致 —— 两边必须算出
//    同一个 `~/.ncc/bin/ncc`，否则升级/安装的是另一个副本。
const HOME = process.env.NCC_HOME || process.env.HOME || os.homedir();

// ⚠️ Windows 下必须带 .exe 后缀 —— 与 bin/ncc.js 里的 EXE 保持同一条约定。
//    没有后缀的 PE 文件在 Windows 上 spawn 会直接 ENOENT（实测 2×2 隔离确认：
//    目录无关、扩展名是唯一变量）。两处必须同时改，否则 postinstall 存成
//    ncc.exe、启动器却去找 ncc（或反过来），表现是"刚装完就要重新下载"。
const EXE = process.platform === 'win32' ? '.exe' : '';
const BIN_DIR = path.join(HOME, '.ncc', 'bin');
const dest = path.join(BIN_DIR, 'ncc' + EXE);

// 版本标记：记下「这份缓存是哪一版装进去的」。
//
// 只判断文件存在是不够的 —— 早先的实现只要 dest 在就 exit，于是升级 npm 包之后
// postinstall 什么也不做，用户跑的还是第一次下载的那版二进制（实测：npm 已到
// 0.1.1，`npx` 仍报 0.1.0）。`ncc upgrade` 也会写这个标记（内容是同一条约定版本号），
// 所以正常情况下它升完级、包装不会再重下一遍。
const marker = path.join(BIN_DIR, '.version');
const PKG_VERSION = require(path.join(__dirname, '..', 'package.json')).version;

function markerMatches() {
  try {
    return fs.readFileSync(marker, 'utf8').trim() === PKG_VERSION;
  } catch {
    return false;
  }
}

// 已存在**且是本版装的**才跳过
try {
  fs.accessSync(dest, fs.constants.X_OK);
  if (markerMatches()) process.exit(0);
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
// 记下这一版 —— 写不上不影响使用，只是下次会重下一遍
try { fs.writeFileSync(marker, PKG_VERSION); } catch { /* ignore */ }
process.exit(0);
