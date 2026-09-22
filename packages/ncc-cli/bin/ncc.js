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
// ⚠️ NCC_HOME 最优先，与 Rust 侧 `config::home_dir()` 一致（两边必须是同一个 ~/.ncc/bin/ncc）
const HOME = process.env.NCC_HOME || process.env.HOME || os.homedir();

// ⚠️ Windows 下**必须**带 .exe 后缀。
//
// 注意这是**本地落盘名**，与下载 URL 里的资产名是两回事 —— 后者由
// platformFile() 生成（ncc-windows-x86_64.exe），本来就是对的；这里错的是
// 存到 ~/.ncc/bin/ 之后叫什么。
//
// 实测（Windows 11 + Node 22，扩展名 × 目录 的 2×2 隔离）：
//     ~/.ncc/bin/ncc       → spawn ENOENT
//     ~/.ncc/bin/ncc.exe   → 正常执行
//   目录不影响，**扩展名是唯一变量**。PE 文件没有 .exe 后缀时 Node 的
//   CreateProcess 直接找不到它。
//
// 后果是每个 Windows 用户第一次 `npx @fusedmodel/ncc-cli` 必然失败：
// 下载成功、文件正确、就是执行不了，报
//   ✗ 执行 ncc 失败：spawn C:\Users\...\.ncc\bin\ncc ENOENT
// 兄弟工程 rsi3d 的 launcher.js 用 exeName() 加后缀，所以没这个问题。
const EXE = process.platform === 'win32' ? '.exe' : '';
const BIN_DIR = path.join(HOME, '.ncc', 'bin');
const NCC_BIN = path.join(BIN_DIR, 'ncc' + EXE);

// 版本标记：见 install.js 里的说明。缓存只有在「标记 == 本包装版本」时才可信 ——
// 否则它可能是更早的包装装进去的旧二进制，直接用会让用户永远停在旧版本上。
const MARKER = path.join(BIN_DIR, '.version');
const PKG_VERSION = require(path.join(PKG_ROOT, 'package.json')).version;

function platformFile() {
  const p = os.platform();
  const a = os.arch();
  const osn = p === 'darwin' ? 'darwin' : p === 'linux' ? 'linux' : p === 'win32' ? 'windows' : p;
  const arch = a === 'x64' ? 'x86_64' : a === 'arm64' ? 'arm64' : a;
  return `ncc-${osn}-${arch}${osn === 'windows' ? '.exe' : ''}`;
}

function exists(p) {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** 缓存里的那份是不是本版装的。 */
function cacheIsCurrent() {
  if (!exists(NCC_BIN)) return false;
  try {
    return fs.readFileSync(MARKER, 'utf8').trim() === PKG_VERSION;
  } catch {
    return false;
  }
}

/**
 * 不参与版本判定的候选：显式指定 / 随包分发 / 仓库本地构建。
 * 这三个是调用方有意为之的来源，不该被 npm 包版本覆盖。
 */
function trustedBin() {
  const list = [];
  if (process.env.NCC_BIN) list.push(process.env.NCC_BIN);
  list.push(path.join(PKG_ROOT, 'vendor', platformFile()));
  list.push(path.join(REPO_ROOT, 'cli', 'target', 'release', 'ncc' + EXE));
  return list.find(exists) || null;
}

function download(url, dest) {
  const r = require('child_process').spawnSync(
    process.execPath,
    [path.join(__dirname, 'download.js'), url, dest],
    { stdio: 'inherit' },
  );
  return r.status === 0;
}

let bin = trustedBin();
if (!bin) {
  if (cacheIsCurrent()) {
    bin = NCC_BIN;
  } else {
    const stale = exists(NCC_BIN);
    const base = process.env.NCC_RELEASE_BASE || 'https://github.com/fusedmodel/ncc/releases/latest/download';
    const url = `${base}/${platformFile()}`;
    console.error(stale ? '→ 缓存里的 ncc 不是本版装的，正在更新…' : '→ 首次使用，正在下载 ncc…');
    console.error('  ' + url);
    if (download(url, NCC_BIN)) {
      try { fs.chmodSync(NCC_BIN, 0o755); } catch { /* ignore */ }
      try { fs.writeFileSync(MARKER, PKG_VERSION); } catch { /* ignore */ }
      bin = NCC_BIN;
    } else if (stale) {
      // 下载失败，但手里还有一份旧的：先用起来，但要说清楚它不是本版 ——
      // 静默退回才是真正会坑人的做法。
      console.error('  ⚠️ 下载失败，退回使用缓存里的旧版本（可能与当前包装不匹配）。');
      console.error('     恢复网络后可运行 `ncc upgrade`，或重装本包。');
      bin = NCC_BIN;
    }
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
