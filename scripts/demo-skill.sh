#!/usr/bin/env bash
# Skill 全流程示例（curl 版）
# 覆盖：注册 → 上传 SKILL.md → 发布 public → 检索 → 升级 Pro → 发布 private → 公开/私有可见性验证
# 前置：服务已启动（npm start 于 :8181，演示模式未配 Stripe 时 checkout 直充 Pro）
set -euo pipefail
BASE="${NCC_BASE:-http://localhost:8181}"
EMAIL="demo-$(date +%s)@ncc.dev"
PASS="demo1234"
# 服务端启用注册门禁时（NCC_INVITE_CODE）需带邀请码；默认与工程默认值一致
INVITE="${NCC_INVITE_CODE:-NCC-2026-INVITE}"

# 从 stdin 读取 JSON，按点路径取值（支持数组下标 .0）
jget() {
  node -e '
    let d="";
    process.stdin.on("data", c => d += c).on("end", () => {
      try {
        let v = JSON.parse(d);
        for (const k of process.argv[1].split(".")) {
          if (v == null) break;
          v = (Array.isArray(v) && /^\d+$/.test(k)) ? v[+k] : v[k];
        }
        process.stdout.write(v == null ? "" : String(v));
      } catch (e) { process.stdout.write(""); }
    });
  ' "$1"
}

echo "══════════════════════════════════════════"
echo "① 注册账户（自动创建个人 namespace）"
echo "══════════════════════════════════════════"
REG=$(curl -sf -X POST "$BASE/api/auth/register" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$EMAIL\",\"password\":\"$PASS\",\"name\":\"Demo\",\"inviteCode\":\"$INVITE\"}")
TOKEN=$(echo "$REG" | jget token)
ME=$(curl -sf "$BASE/api/auth/me" -H "Authorization: Bearer $TOKEN")
NS=$(echo "$ME" | jget namespaces.0.slug)
echo "email=$EMAIL  namespace=$NS"

echo
echo "══════════════════════════════════════════"
echo "② 上传 SKILL.md 字节 → 获得 storageUrl + sha256"
echo "══════════════════════════════════════════"
SKILL_FILE=$(mktemp /tmp/ncc-skill-XXXXXX.md)
cat > "$SKILL_FILE" <<'EOF'
---
name: hotel-skill
description: 根据目的地推荐酒店并生成行程摘要
---
# Hotel Skill
输入城市与预算，返回酒店列表与行程摘要。
EOF
UP=$(curl -sf -X POST "$BASE/api/registry/uploads" \
  -H "Authorization: Bearer $TOKEN" -H "X-Filename: hotel.SKILL.md" \
  --data-binary @"$SKILL_FILE")
URL=$(echo "$UP" | jget storageUrl)
SHA=$(echo "$UP" | jget sha256)
SIZE=$(echo "$UP" | jget size)
echo "storageUrl=$URL"
echo "sha256=$SHA size=$SIZE"

echo
echo "══════════════════════════════════════════"
echo "③ 发布 public Skill（status=published, visibility=public）"
echo "══════════════════════════════════════════"
PUB=$(curl -sf -X POST "$BASE/api/registry" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"kind\":\"skill\",\"name\":\"Hotel Skill\",\"slug\":\"hotel-skill\",\"version\":\"1.0.0\",\"summary\":\"酒店推荐技能\",\"tags\":[\"hotel\",\"travel\"],\"status\":\"published\",\"storage\":{\"url\":\"$URL\",\"sha256\":\"$SHA\",\"size\":$SIZE}}")
ITEM_ID=$(echo "$PUB" | jget item.id)
echo "published → $NS/hotel-skill  id=$ITEM_ID"

echo
echo "══════════════════════════════════════════"
echo "④ 检索：kind=skill（匿名公开可见）"
echo "══════════════════════════════════════════"
curl -sf "$BASE/api/registry?kind=skill&q=hotel" | node -e '
  let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{
    const o=JSON.parse(d);
    console.log("total="+o.total);
    for (const it of o.items) console.log(`  [${it.kind}] ${it.namespace.slug}/${it.slug}@${it.version}  vis=${it.visibility} status=${it.status}  ${it.summary}`);
  });'

echo
echo "══════════════════════════════════════════"
echo "⑤ 升级 Pro（演示模式直充 30 天）→ 解锁 private"
echo "══════════════════════════════════════════"
curl -sf -X POST "$BASE/api/billing/checkout" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{}' >/dev/null
echo "plan=$(curl -sf "$BASE/api/billing/me" -H "Authorization: Bearer $TOKEN" | jget plan)"

echo
echo "══════════════════════════════════════════"
echo "⑥ 发布 private Skill（visibility=private）"
echo "══════════════════════════════════════════"
PRIV=$(curl -sf -X POST "$BASE/api/registry" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"kind\":\"skill\",\"name\":\"Internal Ops\",\"slug\":\"internal-ops\",\"version\":\"0.1.0\",\"summary\":\"内部运营技能（私有）\",\"tags\":[\"internal\"],\"status\":\"published\",\"visibility\":\"private\",\"storage\":{\"url\":\"$URL\",\"sha256\":\"$SHA\",\"size\":$SIZE}}")
PRIV_ID=$(echo "$PRIV" | jget item.id)
echo "published(私有) → $NS/internal-ops  id=$PRIV_ID"

echo
echo "══════════════════════════════════════════"
echo "⑦ 可见性验证：匿名视角 vs 登录(owner)视角"
echo "══════════════════════════════════════════"
echo "--- 匿名访问 $NS 目录（应只见 public，canManage=false）---"
curl -sf "$BASE/api/registry?namespace=$NS" | node -e '
  let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{
    const o=JSON.parse(d);
    console.log(`canManage=${o.canManage} total=${o.total}`);
    for (const it of o.items) console.log(`  ${it.slug}  vis=${it.visibility} status=${it.status}`);
  });'

echo "--- owner 登录访问 $NS 目录（应见 private，canManage=true）---"
curl -sf "$BASE/api/registry?namespace=$NS" -H "Authorization: Bearer $TOKEN" | node -e '
  let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{
    const o=JSON.parse(d);
    console.log(`canManage=${o.canManage} total=${o.total}`);
    for (const it of o.items) console.log(`  ${it.slug}  vis=${it.visibility} status=${it.status}`);
  });'

echo "--- 私有条目下载：匿名(应 403) vs owner(应 200) ---"
echo -n "anonymous download: "; curl -s -o /dev/null -w "%{http_code}\n" "$BASE/api/registry/$PRIV_ID/download"
echo -n "owner download:     "; curl -s -o /dev/null -w "%{http_code}\n" "$BASE/api/registry/$PRIV_ID/download" -H "Authorization: Bearer $TOKEN"

echo
echo "══════════════════════════════════════════"
echo "⑧ CLI 等价命令（ncc <command>）"
echo "══════════════════════════════════════════"
cat <<EOF
ncc --base http://localhost:8181 register --email you@x.com --password demo1234 --name You --invite $INVITE
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --slug hotel-skill --tags hotel,travel
ncc publish --file ./ops.SKILL.md --kind skill --name "Internal Ops" --slug internal-ops --visibility private
ncc search skill
ncc info @you/hotel-skill
ncc download @you/internal-ops -o ops.md        # 私有：仅 owner/成员可下载
EOF

rm -f "$SKILL_FILE"
echo
echo "DEMO OK  (user=$EMAIL  ns=$NS  public=$ITEM_ID  private=$PRIV_ID)"
