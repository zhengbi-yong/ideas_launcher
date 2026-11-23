#!/usr/bin/env node
// Pretty-print a REA artifact text file (single-line JSON) into an indented .json
// Usage: node pp.js <in.txt> <out.json>
const fs = require('fs');
const [, , src, dst] = process.argv;
if (!src || !dst) { console.error('usage: node pp.js <in.txt> <out.json>'); process.exit(2); }
const txt = fs.readFileSync(src, 'utf8');
let obj;
try { obj = JSON.parse(txt); } catch (e) { console.error('parse failed: ' + e.message); process.exit(1); }
fs.writeFileSync(dst, JSON.stringify(obj, null, 2), 'utf8');
const nr = obj.normalized_result || {};
console.log('keys(normalized_result): ' + Object.keys(nr).join(', '));
console.log('bytes=' + fs.statSync(dst).size + ' -> ' + dst);
