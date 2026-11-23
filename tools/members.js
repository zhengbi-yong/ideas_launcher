#!/usr/bin/env node
// Extract type/method/call-edge architecture from an exported REA evidence bundle.
// Usage: node members.js <bundle.json> [outDir]
const fs = require('fs');
const path = require('path');

const bundle = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const outDir = process.argv[3] || process.argv[2].replace(/\.json$/, '');

function dump(name, text) {
  fs.writeFileSync(path.join(outDir, name), text, 'utf8');
  console.log(`  wrote ${name} (${Buffer.byteLength(text, 'utf8')} bytes)`);
}

for (const rec of bundle.records) {
  if (rec.operation !== 'inspect_managed_members') continue;
  const nr = rec.normalized_result || {};
  const short = (rec.subject.name.split('\\').pop() || 'x').replace(/\.dll$/i, '');
  console.log(`\n### ${short}  ev=${rec.evidence_id}`);

  const types = nr.types || [];
  const methods = nr.methods || [];
  const fields = nr.fields || [];
  const edges = nr.call_edges || [];
  const facc = nr.field_accesses || [];
  console.log(`  types=${types.length} methods=${methods.length} fields=${fields.length} edges=${edges.length} field_accesses=${facc.length}`);
  console.log('  nr.types[0] keys: ' + (types[0] ? Object.keys(types[0]).join(', ') : 'n/a'));
  console.log('  nr.methods[0]: ' + JSON.stringify(methods[0]).slice(0, 400));
  console.log('  nr.call_edges[0]: ' + JSON.stringify(edges[0]).slice(0, 400));

  // Type inventory grouped by namespace
  const byNs = new Map();
  for (const t of types) {
    const ns = t.namespace || t.declaring_namespace || (t.full_name || '').split('.').slice(0, -1).join('.');
    if (!byNs.has(ns)) byNs.set(ns, []);
    byNs.get(ns).push(t);
  }
  let tOut = '';
  for (const [ns, list] of [...byNs.entries()].sort()) {
    tOut += `\n== namespace ${ns || '(global)'} (${list.length} types) ==\n`;
    for (const t of list.sort((a, b) => (a.name || '').localeCompare(b.name || ''))) {
      tOut += `  ${t.kind || '?'} ${t.name || t.full_name}` +
        (t.base_type ? ` : ${t.base_type}` : '') +
        (t.member_count != null ? ` [members=${t.member_count}]` : '') +
        (t.field_count != null ? ` [fields=${t.field_count}]` : '') +
        (t.method_count != null ? ` [methods=${t.method_count}]` : '') + '\n';
    }
  }
  dump(`${short}-types.txt`, tOut);

  // Method inventory
  let mOut = '';
  for (const m of methods) {
    mOut += [m.declaring_type || m.type || '', m.name || '', m.signature || '', m.token || '', m.kind || ''].filter(Boolean).join(' | ') + '\n';
  }
  dump(`${short}-methods.txt`, mOut);

  // Call edges
  let eOut = '';
  for (const e of edges) {
    eOut += [e.from || e.caller || e.source, e.to || e.callee || e.target, e.kind || e.call_kind || ''].filter(Boolean).join(' -> ') + '\n';
  }
  dump(`${short}-edges.txt`, eOut);
}
