#!/usr/bin/env node
// Compact summary of inspect_managed_artifact JSON.
// Usage: node sum2.js <file.json>
const fs = require('fs');
const j = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const nr = j.normalized_result || {};
const a = nr.assembly || {};
console.log('ASSEMBLY: ' + a.name + ' v' + a.version + '  tfm=' + JSON.stringify((nr.target_frameworks || []).map(t => t.framework || t)));
console.log('PE: ' + (nr.pe ? (nr.pe.optional_header + ' mach=' + nr.pe.machine_hex + ' sections=' + nr.pe.section_count) : '?'));
console.log('evidence_id: ' + j.evidence_id);
console.log('sha256: ' + (j.subject && j.subject.digest && j.subject.digest.sha256));
const refs = (nr.references || []).map(r => r.name + ' ' + r.version);
console.log('REFERENCES(' + refs.length + '): ' + refs.join(' | '));
const md = nr.metadata || {};
console.log('METADATA keys: ' + Object.keys(md).join(', '));
if (md.module) console.log('module: ' + JSON.stringify(md.module));
const res = nr.resources;
if (Array.isArray(res) && res.length) {
  console.log('RESOURCES(' + res.length + '):');
  for (const r of res) console.log('  ' + JSON.stringify(r).slice(0, 300));
} else console.log('RESOURCES: ' + (Array.isArray(res) ? '0' : JSON.stringify(res).slice(0, 400)));
console.log('COVERAGE: ' + JSON.stringify(nr.coverage));
