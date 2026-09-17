#!/usr/bin/env bash
# ncc.ai 端到端冒烟（需先 `npm run dev` 或 `npm run start` 于 :8181，且 data 目录为空干净）
set -euo pipefail
BASE="${NCC_BASE:-http://localhost:8181}"
EMAIL="smoke-$(date +%s)@ncc.dev"
PASS="demo1234"
echo "== register =="
REG=$(curl -sf -X POST "$BASE/api/auth/register" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$EMAIL\",\"password\":\"$PASS\",\"name\":\"Smoke\"}")
echo "$REG" | head -c 400; echo
TOKEN=$(echo "$REG" | node -e "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>process.stdout.write(JSON.parse(d).token))")

echo "== me =="
curl -sf "$BASE/api/auth/me" -H "Authorization: Bearer $TOKEN" | head -c 300; echo

echo "== publish (BYO url) =="
ITEM=$(curl -sf -X POST "$BASE/api/registry" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"kind":"api","name":"Ping API","slug":"ping","version":"1.0.0","summary":"smoke item","tags":["demo","api"],"storage":{"url":"https://example.com/ping.json","sha256":"abc","size":12}}')
echo "$ITEM" | head -c 400; echo
ID=$(echo "$ITEM" | node -e "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>process.stdout.write(JSON.parse(d).id))")

echo "== publish (status published) =="
curl -sf -X POST "$BASE/api/registry" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"kind":"skill","name":"Hotel Skill","slug":"hotel-skill","version":"0.2.0","summary":"smoke skill","tags":["hotel","skill"],"status":"published","storage":{"url":"https://example.com/hotel.SKILL.md"}}' | head -c 200; echo

echo "== public search =="
curl -sf "$BASE/api/registry?q=ping&kind=api" | head -c 400; echo

echo "== download =="
curl -sf "$BASE/api/registry/$ID/download" | head -c 300; echo

echo "== kinds =="
curl -sf "$BASE/api/registry/kinds" | head -c 400; echo

echo "== api key =="
KEY=$(curl -sf -X POST "$BASE/api/auth/keys" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"label":"ci"}')
echo "$KEY" | head -c 200; echo
SECRET=$(echo "$KEY" | node -e "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>process.stdout.write(JSON.parse(d).secret))")
echo "== list via api key =="
curl -sf "$BASE/api/registry?kind=skill" -H "Authorization: Bearer $SECRET" | head -c 200; echo
echo "SMOKE OK"
