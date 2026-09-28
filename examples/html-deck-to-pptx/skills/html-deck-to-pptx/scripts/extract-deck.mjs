#!/usr/bin/env node
/**
 * extract-deck.mjs —— 把 HTML deck 的「版面事实」抽成 JSON：几何 + 文字 + 图片。
 *
 * 为什么需要它：HTML deck（1920×1080 固定画布 + .slide 分页）转「可编辑 PPT」时，
 * 不能靠人眼看图手抄坐标 —— 由浏览器把每个元素的盒子、边框、字号字重颜色、行高、
 * 图片原始尺寸算出来，交给 deck2pptx.py 原样重建为 PowerPoint 形状。
 *
 * 零依赖：Node 22+ 内置 fetch / WebSocket，直接驱动本机 Google Chrome（CDP 协议）。
 *
 * 用法：
 *   node extract-deck.mjs <deck.html> [--out <layout.json>] [--selector .slide]
 *                                    [--design 1920x1080] [--no-reset] [--port 9222]
 *                                    [--note-selectors ".note,.footline,.ft,[data-note]"]
 *
 * 输出（默认写到 deck 同目录 <deck 同名>._layout.json）：
 * {
 *   "source": "/abs/path/deck.html", "title": "...", "design": [1920, 1080],
 *   "slides": [{
 *     "bg": "rgb(250, 248, 243)",
 *     "boxes": [{ "k": "pcard", "cls": ["card","pcard"], "tag": "div", "r": [x,y,w,h],
 *                 "bg": "rgb(255,253,249)"|"none", "bgi": "none"|"linear-gradient(...)",
 *                 "bt": [宽, 色], "br": [..], "bb": [..], "bl": [..],   // 上/右/下/左
 *                 "rad": "8px", "pad": [t, r, b, l] }],
 *     "texts": [{ "k": "pcard", "cls": [...], "tag": "div", "r": [x,y,w,h],
 *                 "lh": 26, "ta": "left", "ws": "normal"|"nowrap", "va": "middle"|"top",
 *                 "fs": 16, "fw": 400, "co": "rgb(27,25,21)", "pad": [t,r,b,l],
 *                 "runs": [{ "t": "文字", "fs": 16, "fw": 700, "co": "rgb(...)" }] }],
 *     "imgs": [{ "src": "assets/a.jpg", "r": [x,y,w,h], "fit": "cover", "nw": 1600, "nh": 900 }],
 *     "notes": ["备注第一行", "..."]
 *   }]
 * }
 *
 * 抽取规则（踩过的坑，改代码前务必读）：
 *   1) 坐标一律相对所属 .slide 左上角，并除以该 slide 的实测缩放比（deck 常用
 *      transform:scale() 做自适应；transform 不影响布局尺寸，故用 offsetWidth 反推）。
 *   2) 「有底/有边框」的元素同时产出 box 与 text（如胶囊 .fl：底色 + 文字）——
 *      text 的 r 与 box 相同，消费侧用 pad 内缩，别在这里改 r。
 *   3) 父容器带可见盒子时**不能**把它的后代文字吞成一个文本块（否则胶囊的底色/边框丢失）：
 *      下钻规则 = 只把 inline 子元素并进本元素的 runs，块级子元素另起条目。
 *   4) 页面级重置：入场动画常用 opacity:0 / translateY(...) 起始态，抽取时若动画没跑完
 *      会量到「不可见」或错位。故注入一段 !important 的样式把 opacity/visibility/transition
 *      归零（--no-reset 可关）。只影响这一次抽取，页面是一次性的。
 *   5) 图片必须有 <img>（naturalWidth/Height + object-fit）；用 background-image 承载的
 *      照片抽不出字节，需要先改成 <img>。
 */
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import http from 'node:http';
import { writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';

const CHROME = process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/* ────────────────────────── 参数 ────────────────────────── */

function parseArgs(argv) {
  const a = { selector: '.slide', out: null, design: null, reset: true, port: 0, timeout: 30000,
              noteSelectors: '.note, .footline, [data-note]' };
  const rest = [];
  for (let i = 0; i < argv.length; i++) {
    const v = argv[i];
    if (v === '--out') a.out = argv[++i];
    else if (v === '--selector') a.selector = argv[++i];
    else if (v === '--note-selectors') a.noteSelectors = argv[++i];
    else if (v === '--design') a.design = argv[++i].split(/[x×,]/).map(Number);
    else if (v === '--port') a.port = Number(argv[++i]);
    else if (v === '--timeout') a.timeout = Number(argv[++i]) * 1000;
    else if (v === '--no-reset') a.reset = false;
    else if (v === '--chrome') a.chrome = argv[++i];
    else if (v === '-h' || v === '--help') a.help = true;
    else rest.push(v);
  }
  a.input = rest[0];
  return a;
}

/* ────────────────────── CDP 最小客户端 ────────────────────── */

function jsonReq(port, pathname, method = 'GET') {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: pathname, method, timeout: 5000 }, (res) => {
      let body = '';
      res.on('data', (c) => (body += c));
      res.on('end', () => {
        if (res.statusCode >= 400) return reject(new Error(`${method} ${pathname} → ${res.statusCode}`));
        try { resolve(JSON.parse(body)); } catch (e) { resolve(body); }
      });
    });
    req.on('timeout', () => req.destroy(new Error('timeout')));
    req.on('error', reject);
    req.end();
  });
}

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.seq = 0;
    this.pending = new Map();
  }
  ready() {
    return new Promise((resolve, reject) => {
      this.ws.addEventListener('open', () => resolve());
      this.ws.addEventListener('error', () => reject(new Error('DevTools WebSocket 连接失败')));
      this.ws.addEventListener('message', (ev) => {
        let msg;
        try { msg = JSON.parse(ev.data); } catch { return; }
        const slot = this.pending.get(msg.id);
        if (!slot) return;
        this.pending.delete(msg.id);
        msg.error ? slot.reject(new Error(msg.error.message)) : slot.resolve(msg.result);
      });
    });
  }
  send(method, params = {}) {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${method} 超时`));
      }, 30000);
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expression) {
    const r = await this.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) {
      throw new Error('页面内报错：' + (r.exceptionDetails.exception?.description || JSON.stringify(r.exceptionDetails)));
    }
    return r.result.value;
  }
  close() { try { this.ws.close(); } catch {} }
}

/* ─────────────── 在页面里执行的抽取函数（会被序列化后注入） ─────────────── */

function EXTRACT(o) {
  const px = (v) => { const n = parseFloat(v); return isNaN(n) ? 0 : n; };
  const dot = (v) => Math.round(v * 100) / 100;
  const transparent = (c) => !c || c === 'transparent' || /^rgba\(0,\s*0,\s*0,\s*0\)$/i.test(c);
  const inline = (cs) => cs.display === 'inline';
  const tag = (el) => el.tagName.toLowerCase();
  const clsOf = (el) => Array.from(el.classList || []);
  const kOf = (el) => { const c = clsOf(el); return c.length ? c[c.length - 1] : tag(el); };
  const padOf = (cs) => [px(cs.paddingTop), px(cs.paddingRight), px(cs.paddingBottom), px(cs.paddingLeft)].map(dot);
  const lhOf = (cs) => {
    const lh = cs.lineHeight;
    if (lh && lh.endsWith('px')) return dot(px(lh));
    const f = parseFloat(lh);
    return dot(px(cs.fontSize) * (isNaN(f) ? 1.2 : f));
  };
  // 文字在框内的垂直对齐：只有「自己是 flex 容器且 align-items:center」时才中置
  // （例如 .fl 胶囊）；表格单元格等一律顶部对齐，与 HTML 的默认流向一致。
  const vaOf = (cs) => {
    if (/flex/.test(cs.display) && cs.alignItems === 'center') return 'middle';
    if (cs.display === 'table-cell' && cs.verticalAlign === 'middle') return 'middle';
    return 'top';
  };

  if (o.reset) {
    const s = document.createElement('style');
    s.id = '__extract_reset__';
    s.textContent = '*,*::before,*::after{opacity:1 !important;visibility:visible !important;transition:none !important}'
      + '[class*="rv"],[class*="reveal"],[data-reveal]{transform:none !important}';
    document.head.appendChild(s);
  }

  const slidesEl = Array.from(document.querySelectorAll(o.selector));
  const slides = [];
  let design = o.design && o.design.length === 2 ? o.design : null;

  for (const slide of slidesEl) {
    // 量之前把该 slide 强制显示（deck 常把非当前页 display:none / opacity:0）
    const saved = { display: slide.style.display, visibility: slide.style.visibility };
    if (getComputedStyle(slide).display === 'none') {
      slide.style.display = 'block';
      slide.style.visibility = 'visible';
    }
    const box = slide.getBoundingClientRect();
    const dw = slide.offsetWidth || box.width;
    const dh = slide.offsetHeight || box.height;
    if (!design && dw > 0) design = [dw, dh];
    const scale = dw > 0 && box.width > 0 ? box.width / dw : 1;
    const base = { left: box.left, top: box.top };
    const rOf = (el) => {
      const r = el.getBoundingClientRect();
      return [dot((r.left - base.left) / scale), dot((r.top - base.top) / scale),
              dot(r.width / scale), dot(r.height / scale)];
    };

    // 页面底色：slide 自己没底就往上找第一个不透明的祖先
    let bg = getComputedStyle(slide).backgroundColor;
    for (let p = slide.parentElement; p && transparent(bg); p = p.parentElement) {
      bg = getComputedStyle(p).backgroundColor;
    }

    const out = { bg: transparent(bg) ? 'rgb(255,255,255)' : bg, boxes: [], texts: [], imgs: [], notes: [] };

    // 备注：按选择器独立收一遍（不要复用 walk 的条目 —— flex 容器会把子元素的文字
    // 「块化」成独立条目，容器的 textContent 才是完整的页脚口径）
    const noteSeen = new Set();
    for (const n of slide.querySelectorAll(o.noteSelectors)) {
      // 页脚里的页码占位（.pg/.pageno，常由 deck 的 JS 填成「01 / 16」）不要混进备注
      let node = n;
      if (n.querySelector('.pg, .pageno, [data-pageno]')) {
        node = n.cloneNode(true);
        for (const x of node.querySelectorAll('.pg, .pageno, [data-pageno]')) x.remove();
      }
      const t = (n.getAttribute('data-note') || node.textContent || '')
        .replace(/[ \t\n\r]+/g, ' ').trim();       // 不用 \s：全角空格 U+3000 要留着
      if (t && !noteSeen.has(t)) { noteSeen.add(t); out.notes.push(t); }
    }

    // skipInline：本元素已经产出 text（inline 后代已被并入 runs）时，下钻要跳过 inline 子元素，
    // 否则同一段文字会同时以「父元素整个文本块」和「子元素自己的文本块」两次进入 PPT（重叠重影）。
    const walk = (el, depth, skipInline) => {
      if (depth > 40 || out.boxes.length + out.texts.length > 1200) return;
      for (const el2 of el.children) {
        const cs = getComputedStyle(el2);
        if (cs.display === 'none') continue;
        if (skipInline && inline(cs)) continue;
        const r = rOf(el2);
        if (r[2] <= 0.5 || r[3] <= 0.5) continue;

        if (el2.tagName === 'IMG') {
          // 被 overflow:hidden 的祖先裁掉的部分，PPT 里不会自动裁 → 把可见区算成新框 + 裁切比例
          const rb = el2.getBoundingClientRect();
          const vis = { l: rb.left, t: rb.top, r: rb.right, b: rb.bottom };
          for (let p = el2.parentElement; p; p = p.parentElement) {
            const pcs = getComputedStyle(p);
            if (pcs.overflow === 'visible' && pcs.overflowX === 'visible' && pcs.overflowY === 'visible') continue;
            const pr = p.getBoundingClientRect();
            vis.l = Math.max(vis.l, pr.left); vis.t = Math.max(vis.t, pr.top);
            vis.r = Math.min(vis.r, pr.right); vis.b = Math.min(vis.b, pr.bottom);
          }
          const vw = (vis.r - vis.l) / scale, vh = (vis.b - vis.t) / scale;
          if (vw <= 0.5 || vh <= 0.5) continue;                 // 完全被裁掉 → 不要
          const frac = (v, total) => dot(Math.min(0.98, Math.max(0, v / (total || 1))));
          out.imgs.push({
            src: el2.currentSrc || el2.src,
            r: [dot((vis.l - base.left) / scale), dot((vis.t - base.top) / scale), dot(vw), dot(vh)],
            crop: [frac(vis.l - rb.left, rb.width), frac(rb.right - vis.r, rb.width),
                   frac(vis.t - rb.top, rb.height), frac(rb.bottom - vis.b, rb.height)],
            fit: cs.objectFit || 'fill',
            nw: el2.naturalWidth || 0, nh: el2.naturalHeight || 0, alt: el2.alt || '',
          });
          continue;
        }

        // 1) 有底/有边框 → box
        const sides = ['Top', 'Right', 'Bottom', 'Left'].map((s) => (
          cs['border' + s + 'Style'] === 'none' || cs['border' + s + 'Style'] === 'hidden'
            ? [0, cs['border' + s + 'Color']] : [dot(px(cs['border' + s + 'Width'])), cs['border' + s + 'Color']]
        ));
        const hasBox = !transparent(cs.backgroundColor)
          || (cs.backgroundImage && cs.backgroundImage !== 'none')
          || sides.some((s) => s[0] > 0);
        if (hasBox) {
          out.boxes.push({
            k: kOf(el2), cls: clsOf(el2), tag: tag(el2), r,
            bg: transparent(cs.backgroundColor) ? 'none' : cs.backgroundColor,
            bgi: (!cs.backgroundImage || cs.backgroundImage === 'none') ? 'none' : cs.backgroundImage,
            bt: sides[0], br: sides[1], bb: sides[2], bl: sides[3],
            rad: cs.borderRadius, pad: padOf(cs),
          });
        }

        // 2) 文字：只把 inline 后代并进来，块级子元素各自成条目
        const runs = [];
        const flexHost = /flex|grid/.test(cs.display);      // flex/grid 里的纯空白 text node 不渲染
        const fontOf = (e) => {
          const c = getComputedStyle(e);
          return { fs: dot(px(c.fontSize)), fw: parseInt(c.fontWeight, 10) || 400,
                   co: c.color, ws: c.whiteSpace };
        };
        const push = (t, f) => {
          if (!t) return;
          const last = runs[runs.length - 1];
          if (last && last.fs === f.fs && last.fw === f.fw && last.co === f.co) last.t += t;
          else runs.push({ t, fs: f.fs, fw: f.fw, co: f.co });
        };
        const walkNode = (n) => {
          if (n.nodeType === 3) {
            const f = fontOf(n.parentElement);
            const pre = /^pre/.test(f.ws) || f.ws === 'break-spaces';   // pre* 保留原样
            // CSS 折叠：连续空白（空格/换行/制表）→ 单个空格；U+3000 不在折叠集合里，别动它
            const raw = pre ? n.nodeValue : n.nodeValue.replace(/[\t\n\r ]+/g, ' ');
            if (!raw.trim() && !pre && flexHost) return;                // 不渲染的空白节点
            push(raw, f);
            return;
          }
          if (n.nodeType !== 1) return;
          if (n.tagName === 'BR') { push('\n', fontOf(n.parentElement)); return; }
          const c = getComputedStyle(n);
          if (c.display === 'none' || !inline(c)) return;
          for (const k of n.childNodes) walkNode(k);
        };
        for (const n of el2.childNodes) walkNode(n);
        // 折叠后的行首/行尾空白浏览器不渲染 → 去掉（空段落保留为新段落）
        while (runs.length && !runs[0].t.trim()) runs.shift();
        while (runs.length && !runs[runs.length - 1].t.trim()) runs.pop();
        if (runs.length) {
          runs[0].t = runs[0].t.replace(/^[ \t]+/, '');
          const last = runs[runs.length - 1];
          last.t = last.t.replace(/[ \t]+$/, '');
        }
        const joined = runs.map((x) => x.t).join('');
        if (joined.trim()) {
          out.texts.push({ k: kOf(el2), cls: clsOf(el2), tag: tag(el2), r, lh: lhOf(cs),
                           ta: cs.textAlign, ws: cs.whiteSpace, va: vaOf(cs),
                           fs: dot(px(cs.fontSize)), fw: parseInt(cs.fontWeight, 10) || 400,
                           co: cs.color, pad: padOf(cs), runs });
        }

        // 3) 下钻：块级子元素（inline 的已经被上面并进 runs 了）
        walk(el2, depth + 1, !!joined.trim());
      }
    };
    walk(slide, 0, false);

    if (saved.display !== undefined) { slide.style.display = saved.display; slide.style.visibility = saved.visibility; }
    slides.push(out);
  }

  return {
    title: document.title || '',
    design: design || [1920, 1080],
    slides,
    counts: { slides: slides.length, boxes: slides.reduce((n, s) => n + s.boxes.length, 0),
              texts: slides.reduce((n, s) => n + s.texts.length, 0),
              imgs: slides.reduce((n, s) => n + s.imgs.length, 0) },
  };
}

/* ────────────────────────── 主流程 ────────────────────────── */

async function freePort() {
  return new Promise((resolve) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => resolve(p)); });
  });
}

async function waitDevtools(port, timeout) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeout) {
    try { return await jsonReq(port, '/json/version'); } catch { /* 还没起来 */ }
    await sleep(120);
  }
  throw new Error(`Chrome 调试端口 ${port} 未就绪（${timeout}ms）`);
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  if (a.help || !a.input) {
    console.log('用法：node extract-deck.mjs <deck.html> [--out <deck>._layout.json] [--selector .slide]'
    + ' [--design 1920x1080] [--note-selectors ".note,.footline,.ft,[data-note]"] [--no-reset]');
    process.exit(a.input ? 0 : 1);
  }
  const deck = path.resolve(a.input);
  const deckUrl = pathToFileURL(deck).href;
  // 中间件跟 deck 同名（不叫 _layout.json）：同一目录下多个 deck 不会互相覆盖，
  // 也就不会出现「拿 A 的 layout 去校验 B 的 pptx」这种静默错配。
  const outPath = a.out || path.join(path.dirname(deck),
    path.basename(deck).replace(/\.html?$/i, '') + '._layout.json');
  const chromeBin = a.chrome || CHROME;

  const port = a.port || (await freePort());
  const profile = mkdtempSync(path.join(tmpdir(), 'deck-extract-'));
  let child = null;
  let cdp = null;
  try {
    if (!a.port) {
      child = spawn(chromeBin, [
        '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
        '--allow-file-access-from-files', '--force-device-scale-factor=1',
        '--window-size=1920,1080', `--user-data-dir=${profile}`,
        `--remote-debugging-port=${port}`, 'about:blank',
      ], { stdio: ['ignore', 'ignore', 'pipe'] });
      child.stderr.on('data', () => {});
    }
    await waitDevtools(port, a.timeout);

    // 新建标签并等它加载完（新版 Chrome 的 /json/new 只接受 PUT）
    let target;
    try { target = await jsonReq(port, `/json/new?${encodeURIComponent(deckUrl)}`, 'PUT'); }
    catch { target = await jsonReq(port, `/json/new?${encodeURIComponent(deckUrl)}`, 'GET'); }
    cdp = new Cdp(target.webSocketDebuggerUrl);
    await cdp.ready();

    // deck 的 #/N 路由在加载后常会改一次 hash —— 同文档导航会让 Chrome 换掉执行上下文，
    // 正在跑的 Runtime.evaluate 就会以「Execution context was destroyed」失败（随机发生）。
    // 对策：等 URL 稳定 + 出错重试（换上下文后 CDP 连接本身仍然有效）。
    const isContextGone = (e) => /Execution context was destroyed|Cannot find context|Inspected target navigated/i.test(e.message || '');
    const evalRetry = async (expr, tries = 4, label = '求值') => {
      for (let i = 1; i <= tries; i++) {
        try { return await cdp.eval(expr); } catch (e) {
          if (!isContextGone(e) || i === tries) throw e;
          console.warn(`  (${label}被页面导航打断，第 ${i} 次重试…)`);
          await sleep(400);
        }
      }
    };
    const settle = async () => {                  // 等 URL 不再变化
      let last = '';
      for (let i = 0; i < 15; i++) {
        await sleep(200);
        let href = last;
        try { href = await cdp.eval('location.href'); } catch { continue; }
        if (href === last) return href;
        last = href;
      }
      return last;
    };

    const readyExpr = `(async () => {
      if (document.readyState !== 'complete') await new Promise(r => addEventListener('load', r, { once: true }));
      try { await document.fonts.ready; } catch (e) {}
      await Promise.all(Array.from(document.images).map(i => i.complete ? 0 : new Promise(r => { i.onload = i.onerror = r; })));
      await new Promise(r => setTimeout(r, 300));
      return document.querySelectorAll(${JSON.stringify(a.selector)}).length;
    })()`;
    const ready = await evalRetry(readyExpr, 4, '等待就绪');
    if (!ready) throw new Error(`页面上找不到 ${a.selector}（deck 结构不符？）`);
    await settle();

    const expr = `(${EXTRACT.toString()})(${JSON.stringify({ selector: a.selector, design: a.design,
      reset: a.reset, noteSelectors: a.noteSelectors })})`;
    const res = await evalRetry(expr, 4, '抽取');

    // 图片路径：尽量给成相对 deck 目录的路径（消费侧拼 --img-base 即可）
    const dir = path.dirname(deck);
    for (const s of res.slides) {
      for (const im of s.imgs) {
        if (String(im.src).startsWith('file://')) {
          const abs = fileURLToPath(im.src);
          const rel = path.relative(dir, abs);
          im.src = rel.startsWith('..') ? abs : rel;
        }
      }
    }

    const payload = {
      source: deck, sourceUrl: deckUrl, title: res.title,
      design: res.design, extractedAt: new Date().toISOString(),
      slides: res.slides,
    };
    writeFileSync(outPath, JSON.stringify(payload, null, 1) + '\n', 'utf8');
    console.log(`抽到 ${res.counts.slides} 页 · 盒子 ${res.counts.boxes} · 文本 ${res.counts.texts} · 图片 ${res.counts.imgs}`
      + `（画布 ${res.design[0]}×${res.design[1]}）`);
    console.log('已写出：', outPath, `(${(Buffer.byteLength(JSON.stringify(payload)) / 1024).toFixed(0)} KB)`);

    for (const err of res.slides.flatMap((s) => s.imgs).filter((i) => !i.nw)) {
      console.warn('  ! 图片没量到原始尺寸（可能没加载成功）：', err.src);
    }
  } finally {
    if (cdp) cdp.close();
    if (child) child.kill();
    await sleep(150);
    try { rmSync(profile, { recursive: true, force: true }); } catch {}
  }
}

main().catch((e) => { console.error('抽取失败：', e.message); process.exit(1); });
