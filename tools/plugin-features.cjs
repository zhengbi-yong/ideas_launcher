// Summarise the parsed plugin type inventories into a feature map.
const fs = require('fs');
const path = require('path');

const dir = process.argv[2];
const files = fs.readdirSync(dir).filter(f => /-clr-types\.txt$/.test(f) && /^Flow\.Launcher\.Plugin\./.test(f));

const SKIP = /^(<>|\.)|d__\d|<.*>d$|<>c|PrivateImplementationDetails|__/;

for (const f of files.sort()) {
  const text = fs.readFileSync(path.join(dir, f), 'utf8');
  const lines = text.split(/\r?\n/);
  const plugin = f.replace('-clr-types.txt', '');
  const types = [];
  let ns = '';
  for (const line of lines) {
    const m = line.match(/^## (.+?) \(\d+\)$/);
    if (m) { ns = m[1]; continue; }
    const t = line.match(/^- ([A-Za-z_][\w`]*)\s+\[/);
    if (t && !SKIP.test(t[1])) {
      types.push(ns && ns !== '(global)' ? `${ns}.${t[1]}` : t[1]);
    }
  }
  console.log(`\n### ${plugin}  (${types.length} types)`);
  console.log(types.join(' | '));
}
