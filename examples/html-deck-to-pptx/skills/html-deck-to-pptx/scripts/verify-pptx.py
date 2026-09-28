#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""verify-pptx.py —— 校验「原生重建」的 PPTX 是否可交付（本机无 Office 时的替代自检）。

本机通常没装 PowerPoint / LibreOffice / Keynote，无法渲染 눈检，所以用「结构 + 度量」
四件套来兜底：

  1) 文字完整性：PPTX 每页文字 vs 浏览器抽出的文字（逐页比字符数，缺字报警）
  2) 换行风险：用真实字形的宽度，按文本框宽度重算行数，与 HTML 里的行数比对，
     报出「字体度量差异导致可能多出一行」的条目
  3) 字体设置：latin(Arial) 与 ea(PingFang SC) 必须都设了、且数量一致
  4) a:rPr 子元素顺序：必须符合 OOXML schema（顺序错会让 PowerPoint 弹「修复」）
  5) 备注：每页都应有演讲者备注

用法：
    python3 verify-pptx.py <_layout.json> <deck.pptx> [--cjk-font 路径] [--latin-font 路径]
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import zipfile

from lxml import etree
from pptx import Presentation
from pptx.oxml.ns import qn

# ── 度量用的字体（只用于估行宽，不必与 PPT 指定的字体一致，但需 CJK 等宽特性一致）──
CJK_CANDIDATES = [
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
]
LATIN_CANDIDATES = [
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/Library/Fonts/Arial.ttf",
]
# a:rPr 子元素的 schema 顺序（只需覆盖我们会写的那些）
RPR_ORDER = ["ln", "noFill", "solidFill", "gradFill", "blipFill", "pattFill", "grpFill",
             "effectLst", "effectDag", "highlight", "uLnTx", "uLn", "uFillTx", "uFill",
             "latin", "ea", "cs", "sym", "hlinkClick", "hlinkMouseOver", "rtl", "extLst"]


def first_existing(paths):
    for p in paths:
        if os.path.exists(p):
            return p
    return None


def load_layout(path: str) -> dict:
    raw = open(path, encoding="utf-8").read()
    if "Result: " in raw and not raw.lstrip().startswith("{"):
        seg = raw.split("Result: ", 1)[1].strip()
        i, j = seg.index('"'), seg.rindex('"')
        raw = json.loads(seg[i:j + 1])
    data = json.loads(raw)
    if isinstance(data, list):
        return {"design": [1920, 1080], "slides": data}
    return data


def main() -> int:
    ap = argparse.ArgumentParser(description="校验原生重建的 PPTX")
    ap.add_argument("layout")
    ap.add_argument("pptx")
    ap.add_argument("--cjk-font")
    ap.add_argument("--latin-font")
    a = ap.parse_args()

    layout = load_layout(a.layout)
    pages = layout["slides"]
    design = layout.get("design") or [1920, 1080]
    emu_per_px = 12192000 / design[0]

    prs = Presentation(a.pptx)
    print(f"页数：{len(prs.slides)}（期望 {len(pages)}）"
          f" · 画布 {prs.slide_width / 914400:.3f}in × {prs.slide_height / 914400:.3f}in"
          f" · 设计 {design[0]}×{design[1]}")

    # ── 度量字体（可选：没有 PIL 就跳过换行评估）──
    text_width = None
    cjk_path = a.cjk_font or first_existing(CJK_CANDIDATES)
    latin_path = a.latin_font or first_existing(LATIN_CANDIDATES)
    try:
        from PIL import ImageFont
        if cjk_path and latin_path:
            cache: dict = {}

            def font(px_size, is_cjk):
                key = (round(px_size), is_cjk)
                if key not in cache:
                    cache[key] = ImageFont.truetype(cjk_path if is_cjk else latin_path,
                                                    max(int(round(px_size * 2)), 8))
                return cache[key]

            def text_width(text, px_size):     # noqa: F811
                total, buf, is_cjk = 0.0, "", None

                def flush():
                    if not buf:
                        return 0.0
                    return font(px_size, is_cjk).getlength(buf) / 2.0   # 设备px(2×) → 设计px

                for ch in text:
                    cjk = ("\u2e80" <= ch <= "\u9fff") or ("\uff00" <= ch <= "\uffef") \
                        or ch in "·—…“”（）《》、，。：；！？　"
                    if is_cjk is None:
                        is_cjk = cjk
                    if cjk != is_cjk:
                        total += flush()
                        buf, is_cjk = "", cjk
                    buf += ch
                return total + flush()
    except ImportError:
        print("  (未装 Pillow → 跳过换行评估)")

    problems = 0
    total_risk = total_hit = 0
    for idx, (slide, page) in enumerate(zip(prs.slides, pages), 1):
        want = "".join("".join(str(r.get("t", "")) for r in t.get("runs") or [])
                       for t in page.get("texts") or []).replace(" ", "")
        got = "".join(sh.text_frame.text for sh in slide.shapes if sh.has_text_frame).replace(" ", "")
        missing = [c for c in set(want) if want.count(c) > got.count(c)]
        if len(got) < len(want) or missing:
            print(f"  ! 第 {idx} 页文字可能缺失：HTML {len(want)} 字 vs PPTX {len(got)} 字"
                  f"{' · 缺 ' + ''.join(sorted(missing)[:8]) if missing else ''}")
            problems += 1

        risk = []
        for sh in slide.shapes:
            if not sh.has_text_frame or not sh.text_frame.text.strip():
                continue
            if not sh.text_frame.word_wrap:            # 单行文本框：不参与换行评估
                continue
            if text_width is None:
                continue
            box_w = sh.width / emu_per_px
            goal = (960.0 / design[0])                 # 1 设计 px = 多少 pt
            need_px, lines, sample = 0.0, 0, ""
            # 必须逐段估：整框文本里的 \n 会被当成普通字符，白白多算出一两行（假阳性）
            for p in sh.text_frame.paragraphs:
                txt = p.text
                if not txt.strip():
                    continue
                size_pt, lh_pt, mult = 0, 0.0, 0.0
                ls = p.line_spacing
                if ls is not None:
                    # 注意：Length 是 int 子类，必须先用 hasattr(pt) 判出来，否则 EMU 被当 pt
                    if hasattr(ls, "pt"):
                        lh_pt = ls.pt
                    elif isinstance(ls, (int, float)):
                        mult = float(ls)
                for r in p.runs:
                    if r.font.size:
                        size_pt = max(size_pt, r.font.size.pt)
                if not size_pt:
                    size_pt = 10
                px = size_pt / goal
                lh = (lh_pt / goal) if lh_pt else px * (mult or 1.25)
                n, cur = 1, 0.0
                for ch in txt:
                    w = text_width(ch, px)
                    if cur + w > box_w and cur > 0:
                        n, cur = n + 1, 0.0
                    cur += w
                lines += n
                need_px += n * lh
                sample = sample or txt
            if need_px > sh.height / emu_per_px + 3:
                # 多出来的那几行会不会真的压到下面的元素？会 = 叠字，不会 = 只是框外排字
                x1, x2 = (sh.left or 0) / emu_per_px, ((sh.left or 0) + (sh.width or 0)) / emu_per_px
                y2 = (sh.top or 0) / emu_per_px
                bottom = y2 + sh.height / emu_per_px
                hit = 0
                for o in slide.shapes:
                    if o is sh or o.top is None or not o.has_text_frame or not o.text_frame.text.strip():
                        continue
                    oy, oh = o.top / emu_per_px, (o.height or 0) / emu_per_px
                    ox1, ox2 = o.left / emu_per_px, (o.left + (o.width or 0)) / emu_per_px
                    if bottom - 3 <= oy < y2 + need_px and min(ox2, x2) - max(ox1, x1) > 8:
                        hit += 1
                risk.append((lines, round(need_px / max(lines, 1)), sample[:26].replace("\n", "/"), hit))
        print(f"  第 {idx} 页：形状 {len(slide.shapes)}"
              f" · 文本 {sum(1 for s in slide.shapes if s.has_text_frame and s.text_frame.text.strip())}"
              f" · 换行风险 {len(risk)}" + (f"（其中 {sum(1 for r in risk if r[3])} 处会叠字）" if risk else ""))
        total_risk += len(risk)
        total_hit += sum(1 for r in risk if r[3])
        for n, lh, t, hit in risk[:6]:
            tag = f"会与下方 {hit} 个元素叠字" if hit else "下方无元素 → 仅框外排字，不影响版面"
            print(f"      ↑ 预计 {n} 行（行高 {lh}px）· {tag}：{t}")

    # ── 字体设置 ──
    fcount = {"latin": 0, "ea": 0}
    for slide in prs.slides:
        for sh in slide.shapes:
            if not sh.has_text_frame:
                continue
            for p in sh.text_frame.paragraphs:
                for r in p.runs:
                    rPr = r._r.find(qn("a:rPr"))
                    if rPr is None:
                        continue
                    if rPr.find(qn("a:latin")) is not None:
                        fcount["latin"] += 1
                    if rPr.find(qn("a:ea")) is not None:
                        fcount["ea"] += 1
    fb = fcount["latin"] != fcount["ea"] or fcount["latin"] == 0
    print(f"字体：latin {fcount['latin']} 处 · ea {fcount['ea']} 处（应相同且 > 0）"
          + ("  ! 不一致" if fb else ""))
    if fb:
        problems += 1

    # ── a:rPr 子元素顺序（schema 合规）──
    idx_of = {t: i for i, t in enumerate(RPR_ORDER)}
    bad_order = checked = 0
    with zipfile.ZipFile(a.pptx) as z:
        for n in z.namelist():
            if not re.match(r"ppt/slides/slide\d+\.xml$", n):
                continue
            root = etree.fromstring(z.read(n))
            for rPr in root.iter(qn("a:rPr")):
                checked += 1
                pos = [idx_of.get(etree.QName(c).localname, -1) for c in rPr if isinstance(c.tag, str)]
                if -1 in pos:
                    continue
                if pos != sorted(pos):
                    bad_order += 1
    print(f"rPr 检查：{checked} 处 · 顺序异常 {bad_order}")
    if bad_order:
        problems += 1

    notes = sum(1 for s in prs.slides if s.notes_slide.notes_text_frame.text.strip())
    print(f"备注：{notes} / {len(prs.slides)} 页有内容")
    if total_risk:
        print(f"换行风险合计 {total_risk} 条（其中会叠字 {total_hit} 条）—— 叠字的才必须处理："
              f"用 --nowrap-classes / --scale-pt 0.97 压下去，或回 HTML 收字")
        problems += total_hit          # 只是框外排字（下方空着）不影响版面，不算不通过
    print("结论：", "全部通过" if problems == 0 else f"{problems} 项需检查")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
