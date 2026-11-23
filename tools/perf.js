#!/usr/bin/env node
// Aggregate Flow Launcher NLog timing lines into a performance baseline table.
const fs = require('fs');
const path = require('path');

const root = process.argv[2];
const files = [];
(function walk(d) {
  for (const e of fs.readdirSync(d, { withFileTypes: true })) {
    const p = path.join(d, e.name);
    if (e.isDirectory()) walk(p);
    else if (e.name.endsWith('.log')) files.push(p);
  }
})(root);

const patterns = [
  ['Startup cost', /Startup cost <(\d+)ms>/],
  ['Preload images', /Preload images cost <(\d+)ms>/],
  ['Preload programs', /Preload programs cost <(\d+)ms>/],
  ['UWPProgram index', /UWPProgram index cost <(\d+)ms>/],
  ['Win32Program index', /Win32Program index cost <(\d+)ms>/],
];
const pluginInit = /Total init cost for <(.+?)> is <(\d+)ms>/;

const agg = new Map();          // label -> { name, samples[] }
const pluginAgg = new Map();    // plugin -> samples[]

for (const f of files) {
  let txt;
  try { txt = fs.readFileSync(f, 'utf8'); } catch { continue; }
  for (const line of txt.split(/\r?\n/)) {
    for (const [label, re] of patterns) {
      const m = line.match(re);
      if (m) {
        if (!agg.has(label)) agg.set(label, { name: label, samples: [] });
        agg.get(label).samples.push(+m[1]);
      }
    }
    const pm = line.match(pluginInit);
    if (pm) {
      if (!pluginAgg.has(pm[1])) pluginAgg.set(pm[1], []);
      pluginAgg.get(pm[1]).push(+pm[2]);
    }
  }
}

function stats(a) {
  if (!a.length) return null;
  const s = [...a].sort((x, y) => x - y);
  const sum = a.reduce((x, y) => x + y, 0);
  return {
    n: a.length, min: s[0], p50: s[Math.floor(s.length * 0.5)], p90: s[Math.floor(s.length * 0.9)],
    max: s[s.length - 1], mean: Math.round(sum / a.length),
  };
}

console.log(`# Flow Launcher 2.1.4 性能基线  (来源: NLog 日志, ${files.length} 个日志文件)\n`);
console.log('| 指标 | 样本数 | min | p50 | p90 | max | mean |');
console.log('|---|---|---|---|---|---|---|');
for (const [label, v] of agg) {
  const s = stats(v.samples);
  if (!s) continue;
  console.log(`| ${label} | ${s.n} | ${s.min}ms | ${s.p50}ms | ${s.p90}ms | ${s.max}ms | ${s.mean}ms |`);
}
console.log('\n## 插件初始化耗时 (Total init cost)\n');
console.log('| 插件 | 样本数 | min | p50 | max | mean |');
console.log('|---|---|---|---|---|---|');
const ps = [...pluginAgg.entries()].map(([k, v]) => [k, stats(v)]).sort((a, b) => b[1].mean - a[1].mean);
for (const [k, s] of ps) {
  console.log(`| ${k} | ${s.n} | ${s.min}ms | ${s.p50}ms | ${s.max}ms | ${s.mean}ms |`);
}
