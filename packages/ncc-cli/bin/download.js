#!/usr/bin/env node
// 下载文件到目标路径（跟随重定向，失败退出非 0）。
const fs = require('fs');
const http = require('http');
const https = require('https');
const path = require('path');

const [url, dest] = process.argv.slice(2);
if (!url || !dest) {
  console.error('usage: download.js <url> <dest>');
  process.exit(1);
}

fs.mkdirSync(path.dirname(dest), { recursive: true });

function get(u, redirects) {
  if (redirects > 5) {
    console.error('下载失败：重定向次数过多');
    process.exit(1);
  }
  const mod = u.startsWith('https:') ? https : http;
  mod
    .get(u, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        get(new URL(res.headers.location, u).toString(), redirects + 1);
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        console.error(`下载失败：HTTP ${res.statusCode}`);
        process.exit(1);
        return;
      }
      const tmp = dest + '.tmp';
      const f = fs.createWriteStream(tmp);
      res.pipe(f);
      f.on('finish', () => {
        fs.renameSync(tmp, dest);
        process.exit(0);
      });
      f.on('error', (e) => {
        console.error(`下载失败：${e.message}`);
        process.exit(1);
      });
    })
    .on('error', (e) => {
      console.error(`下载失败：${e.message}`);
      process.exit(1);
    });
}

get(url, 0);
