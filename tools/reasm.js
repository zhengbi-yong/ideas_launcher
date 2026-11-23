#!/usr/bin/env node
// Reconstruct a REA chunked-JSONL tool-output artifact into plain text.
// Usage: node reasm.js <artifact.jsonl> <out.txt>
const fs = require('fs');

const [, , src, dst] = process.argv;
if (!src || !dst) {
  console.error('usage: node reasm.js <artifact.jsonl> <out.txt>');
  process.exit(2);
}

const raw = fs.readFileSync(src, 'utf8');
const rows = [];
for (const line of raw.split(/\r?\n/)) {
  if (!line.trim()) continue;
  let o;
  try { o = JSON.parse(line); } catch { continue; }
  if (o && typeof o.text === 'string') rows.push({ i: rows.length, t: o.text });
}
rows.sort((a, b) => a.i - b.i);
const out = rows.map(r => r.t).join('');
fs.writeFileSync(dst, out, 'utf8');
console.log(`chunks=${rows.length} bytes=${Buffer.byteLength(out, 'utf8')} -> ${dst}`);
