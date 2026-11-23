#!/usr/bin/env node
// Summarize a REA inspect_managed_artifact pretty JSON.
// Usage: node sum.js <file.json>
const fs = require('fs');
const f = process.argv[2];
const j = JSON.parse(fs.readFileSync(f, 'utf8'));
const nr = j.normalized_result || {};

console.log('=== SUBJECT ===');
console.log('file: ' + (j.subject && j.subject.name));
console.log('sha256: ' + (j.subject && j.subject.digest && j.subject.digest.sha256));
console.log('evidence_id: ' + j.evidence_id);

console.log('\n=== ASSEMBLY ===');
console.log(JSON.stringify(nr.assembly, null, 1));

console.log('\n=== TARGET FRAMEWORKS ===');
console.log(JSON.stringify(nr.target_frameworks, null, 1));

console.log('\n=== REFERENCES (count=' + ((nr.references || []).length) + ') ===');
const refs = nr.references || [];
const byType = {};
for (const r of refs) {
  const t = (r && (r.assembly_reference_kind || r.kind || r.type)) || 'unknown';
  (byType[t] = byType[t] || []).push(r);
}
for (const t of Object.keys(byType)) {
  console.log('  [' + t + '] x' + byType[t].length);
  for (const r of byType[t].slice(0, 60)) {
    console.log('     ' + JSON.stringify(r));
  }
}

console.log('\n=== RESOURCES ===');
const res = nr.resources;
if (Array.isArray(res)) {
  console.log('count=' + res.length);
  for (const r of res.slice(0, 120)) console.log('  ' + JSON.stringify(r));
} else {
  console.log(JSON.stringify(res, null, 1).slice(0, 6000));
}

console.log('\n=== ATTRIBUTES ===');
console.log(JSON.stringify(nr.attributes, null, 1).slice(0, 3000));

console.log('\n=== COVERAGE / LIMITATIONS ===');
console.log(JSON.stringify(nr.coverage, null, 1));
console.log(JSON.stringify(nr.limitations, null, 1));
