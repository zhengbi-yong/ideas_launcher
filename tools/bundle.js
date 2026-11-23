#!/usr/bin/env node
// Inspect the structure of an exported REA evidence bundle.
const fs = require('fs');
const j = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
console.log('top keys: ' + Object.keys(j).join(', '));
for (const k of Object.keys(j)) {
  const v = j[k];
  if (Array.isArray(v)) console.log(`  ${k}: array(${v.length})`);
  else if (v && typeof v === 'object') console.log(`  ${k}: object keys=[${Object.keys(v).join(', ')}]`);
  else console.log(`  ${k}: ${typeof v} ${String(v).slice(0, 80)}`);
}
const recs = j.records || j.evidence || [];
console.log('\nrecords: ' + recs.length);
for (const r of recs) {
  console.log(`  ${r.evidence_id}  op=${r.operation}  subject=${(r.subject && r.subject.name || '').split('\\').pop()}`);
  const nr = r.normalized_result || {};
  console.log('     nr keys: ' + Object.keys(nr).join(', '));
}
