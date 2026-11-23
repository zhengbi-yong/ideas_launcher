'use strict';
// Minimal .NET CLI (PE/COFF) metadata reader.
// Extracts TypeDef / MethodDef / Field inventory, method signatures, and IL-level
// references (called members, string literals, field accesses) without executing code.
//
// Usage: node clr.js <assembly.dll> [--dump-ill <outdir>]

const fs = require('fs');

const ECMA = {
  ELEMENT_TYPE_VOID: 0x01, ELEMENT_TYPE_BOOLEAN: 0x02, ELEMENT_TYPE_CHAR: 0x03,
  ELEMENT_TYPE_I1: 0x04, ELEMENT_TYPE_U1: 0x05, ELEMENT_TYPE_I2: 0x06, ELEMENT_TYPE_U2: 0x07,
  ELEMENT_TYPE_I4: 0x08, ELEMENT_TYPE_U4: 0x09, ELEMENT_TYPE_I8: 0x0a, ELEMENT_TYPE_U8: 0x0b,
  ELEMENT_TYPE_R4: 0x0c, ELEMENT_TYPE_R8: 0x0d, ELEMENT_TYPE_STRING: 0x0e,
  ELEMENT_TYPE_PTR: 0x0f, ELEMENT_TYPE_BYREF: 0x10, ELEMENT_TYPE_VALUETYPE: 0x11,
  ELEMENT_TYPE_CLASS: 0x12, ELEMENT_TYPE_VAR: 0x13, ELEMENT_TYPE_ARRAY: 0x14,
  ELEMENT_TYPE_GENERICINST: 0x15, ELEMENT_TYPE_TYPEDBYREF: 0x16, ELEMENT_TYPE_I: 0x18,
  ELEMENT_TYPE_U: 0x19, ELEMENT_TYPE_FNPTR: 0x1b, ELEMENT_TYPE_OBJECT: 0x1c,
  ELEMENT_TYPE_SZARRAY: 0x1d, ELEMENT_TYPE_MVAR: 0x1e, ELEMENT_TYPE_CMOD_REQD: 0x1f,
  ELEMENT_TYPE_CMOD_OPT: 0x20, ELEMENT_TYPE_INTERNAL: 0x21, ELEMENT_TYPE_MODIFIER: 0x40,
  ELEMENT_TYPE_SENTINEL: 0x41, ELEMENT_TYPE_PINNED: 0x45,
};

// ---------------------------------------------------------------- PE parsing
function readPE(buf) {
  if (buf.readUInt16LE(0) !== 0x5a4d) throw new Error('not a PE (no MZ)');
  const peOff = buf.readUInt32LE(0x3c);
  if (buf.readUInt32LE(peOff) !== 0x00004550) throw new Error('bad PE signature');
  const coff = peOff + 4;
  const machine = buf.readUInt16LE(coff);
  const numSections = buf.readUInt16LE(coff + 2);
  const optSize = buf.readUInt16LE(coff + 16);
  const opt = coff + 20;
  const magic = buf.readUInt16LE(opt);
  const pe32plus = magic === 0x20b;
  const ddOffset = opt + (pe32plus ? 112 : 96);
  const sectionsOff = opt + optSize;

  const sections = [];
  for (let i = 0; i < numSections; i++) {
    const s = sectionsOff + i * 40;
    sections.push({
      virtualSize: buf.readUInt32LE(s + 8),
      virtualAddress: buf.readUInt32LE(s + 12),
      rawSize: buf.readUInt32LE(s + 16),
      rawOffset: buf.readUInt32LE(s + 20),
    });
  }
  const rvaToOff = (rva) => {
    for (const s of sections) {
      if (rva >= s.virtualAddress && rva < s.virtualAddress + Math.max(s.virtualSize, s.rawSize)) {
        return s.rawOffset + (rva - s.virtualAddress);
      }
    }
    return null;
  };
  const numDD = buf.readUInt32LE(opt + (pe32plus ? 108 : 92));
  const dirs = [];
  for (let i = 0; i < numDD; i++) {
    dirs.push({ rva: buf.readUInt32LE(ddOffset + i * 8), size: buf.readUInt32LE(ddOffset + i * 8 + 4) });
  }
  return { machine, pe32plus, dirs, rvaToOff, buf };
}

// ------------------------------------------------------------ metadata model
const TABLE = {
  Module: 0x00, TypeRef: 0x01, TypeDef: 0x02, FieldPtr: 0x03, Field: 0x04,
  MethodPtr: 0x05, MethodDef: 0x06, ParamPtr: 0x07, Param: 0x08,
  InterfaceImpl: 0x09, MemberRef: 0x0a, Constant: 0x0b, CustomAttribute: 0x0c,
  FieldMarshal: 0x0d, DeclSecurity: 0x0e, ClassLayout: 0x0f, FieldLayout: 0x10,
  StandAloneSig: 0x11, EventMap: 0x12, EventPtr: 0x13, Event: 0x14,
  PropertyMap: 0x15, PropertyPtr: 0x16, Property: 0x17, MethodSemantics: 0x18,
  MethodImpl: 0x19, ModuleRef: 0x1a, TypeSpec: 0x1b, ImplMap: 0x1c,
  FieldRVA: 0x1d, EncLog: 0x1e, EncMap: 0x1f, Assembly: 0x20, AssemblyProcessor: 0x21,
  AssemblyOS: 0x22, AssemblyRef: 0x23, AssemblyRefProcessor: 0x24, AssemblyRefOS: 0x25,
  File: 0x26, ExportedType: 0x27, ManifestResource: 0x28, NestedClass: 0x29,
  GenericParam: 0x2a, MethodSpec: 0x2b, GenericParamConstraint: 0x2c,
};
const T = {};
for (const [k, v] of Object.entries(TABLE)) T[v] = k;

function readMetadata(buf, metaRva, rvaToOff) {
  const mdRva = rvaToOff(metaRva);
  const md = buf.subarray(mdRva);
  if (md.readUInt32LE(0) !== 0x424a5342) throw new Error('bad metadata signature');
  let p = 12;
  const verLen = md.readUInt32LE(12); p = 16 + verLen;
  p += 2; // flags
  const numStreams = md.readUInt16LE(p); p += 2;
  const streams = {};
  for (let i = 0; i < numStreams; i++) {
    const off = md.readUInt32LE(p); p += 4;
    const size = md.readUInt32LE(p); p += 4;
    let name = '';
    while (md[p] !== 0) { name += String.fromCharCode(md[p]); p++; }
    p++;
    p = (p + 3) & ~3;
    streams[name] = { off, size };
  }
  return { md, streams };
}

function strAt(streams, md, index) {
  const s = streams['#Strings'];
  if (!s || index >= s.size) return '';
  const start = s.off + index;
  let end = start;
  while (end < s.off + s.size && md[end] !== 0) end++;
  return md.toString('utf8', start, end);
}

// ECMA-335 II.23.2 compressed unsigned integer.
function readCompressedUint(buf, off) {
  const b0 = buf[off];
  if ((b0 & 0x80) === 0) return { value: b0, next: off + 1 };
  if ((b0 & 0xC0) === 0x80) {
    return { value: ((b0 & 0x3F) << 8) | buf[off + 1], next: off + 2 };
  }
  if ((b0 & 0xE0) === 0xC0) {
    const v = ((b0 & 0x1F) << 24) | (buf[off + 1] << 16) | (buf[off + 2] << 8) | buf[off + 3];
    return { value: v >>> 0, next: off + 4 };
  }
  return { value: 0, next: off + 1 };
}

function userString(streams, md, index) {
  const s = streams['#US'];
  if (!s) return null;
  const off = s.off + index;
  if (off >= s.off + s.size) return null;
  const { value: len, next: p } = readCompressedUint(md, off);
  if (len <= 0) return '';
  return md.toString('utf16le', p, Math.min(md.length, p + len - 1));
}

function blobAt(streams, md, index) {
  const s = streams['#Blob'];
  if (!s) return Buffer.alloc(0);
  const off = s.off + index;
  if (off >= s.off + s.size) return Buffer.alloc(0);
  const { value: len, next: p } = readCompressedUint(md, off);
  return md.subarray(p, Math.min(md.length, p + len));
}

// ------------------------------------------------------------- table reading
function parseTables(md, streams) {
  const ts = streams['#~'] || streams['#-'];
  if (!ts) throw new Error('no #~ stream');
  const base = ts.off;
  const heapSizes = md[base + 6];
  const valid = md.readBigUInt64LE(base + 8);
  const rows = {};
  let p = base + 24;
  const rowCounts = [];
  for (let i = 0; i < 64; i++) {
    if ((valid >> BigInt(i)) & 1n) { rowCounts[i] = md.readUInt32LE(p); p += 4; }
  }
  for (let i = 0; i < 64; i++) rows[i] = rowCounts[i] || 0;

  // HeapSizes bit0=#Strings, bit1=#GUID, bit2=#Blob use 4-byte indices when set.
  const strIdx = (heapSizes & 0x01) ? 4 : 2;
  const guidIdx = (heapSizes & 0x02) ? 4 : 2;
  const blobIdx = (heapSizes & 0x04) ? 4 : 2;
  const sz = (t) => (rows[t] < 65536 ? 2 : 4);
  const cidx = (tables, bits) => {
    let max = 0;
    for (const t of tables) max = Math.max(max, rows[t]);
    return max < (1 << (16 - bits)) ? 2 : 4;
  };

  const coded = {
    TypeDefOrRef: cidx([2, 1, 27], 2),
    HasConstant: cidx([4, 8, 23], 2),
    HasCustomAttribute: cidx([6, 4, 1, 2, 8, 9, 10, 0, 14, 23, 20, 17, 26, 27, 32, 35, 38, 39, 40, 42, 44, 43], 5),
    HasFieldMarshal: cidx([4, 8], 1),
    HasDeclSecurity: cidx([2, 6, 32], 2),
    MemberRefParent: cidx([2, 1, 26, 6, 27], 3),
    HasSemantics: cidx([20, 23], 1),
    MethodDefOrRef: cidx([6, 10], 1),
    MemberForwarded: cidx([4, 6], 1),
    Implementation: cidx([38, 35, 39], 2),
    CustomAttributeType: cidx([6, 10], 3),
    ResolutionScope: cidx([0, 26, 35, 1], 2),
    TypeOrMethodDef: cidx([2, 6], 1),
  };

  const col = {
    Module: [['gen', 2], ['name', strIdx], ['mvid', guidIdx], ['encid', guidIdx], ['encbase', guidIdx]],
    TypeRef: [['scope', coded.ResolutionScope, 'coded'], ['name', strIdx], ['ns', strIdx]],
    TypeDef: [['flags', 4], ['name', strIdx], ['ns', strIdx], ['extends', coded.TypeDefOrRef, 'coded'], ['fieldlist', sz(4)], ['methodlist', sz(6)]],
    Field: [['flags', 2], ['name', strIdx], ['sig', blobIdx]],
    MethodDef: [['rva', 4], ['implflags', 2], ['flags', 2], ['name', strIdx], ['sig', blobIdx], ['paramlist', sz(8)]],
    Param: [['flags', 2], ['seq', 2], ['name', strIdx]],
    InterfaceImpl: [['class', sz(2)], ['interface', coded.TypeDefOrRef, 'coded']],
    MemberRef: [['class', coded.MemberRefParent, 'coded'], ['name', strIdx], ['sig', blobIdx]],
    Constant: [['type', 1], ['pad', 1], ['parent', coded.HasConstant, 'coded'], ['value', blobIdx]],
    CustomAttribute: [['parent', coded.HasCustomAttribute, 'coded'], ['type', coded.CustomAttributeType, 'coded'], ['value', blobIdx]],
    FieldMarshal: [['parent', coded.HasFieldMarshal, 'coded'], ['native', blobIdx]],
    DeclSecurity: [['action', 2], ['parent', coded.HasDeclSecurity, 'coded'], ['perm', blobIdx]],
    ClassLayout: [['pack', 2], ['size', 4], ['parent', sz(2)]],
    FieldLayout: [['offset', 4], ['field', sz(4)]],
    StandAloneSig: [['sig', blobIdx]],
    EventMap: [['parent', sz(2)], ['eventlist', sz(20)]],
    Event: [['flags', 2], ['name', strIdx], ['type', coded.TypeDefOrRef, 'coded']],
    PropertyMap: [['parent', sz(2)], ['proplist', sz(23)]],
    Property: [['flags', 2], ['name', strIdx], ['type', blobIdx]],
    MethodSemantics: [['sem', 2], ['method', sz(6)], ['assoc', coded.HasSemantics, 'coded']],
    MethodImpl: [['class', sz(2)], ['body', coded.MethodDefOrRef, 'coded'], ['decl', coded.MethodDefOrRef, 'coded']],
    ModuleRef: [['name', strIdx]],
    TypeSpec: [['sig', blobIdx]],
    ImplMap: [['flags', 2], ['forwarded', coded.MemberForwarded, 'coded'], ['importname', strIdx], ['scope', sz(26)]],
    FieldRVA: [['rva', 4], ['field', sz(4)]],
    Assembly: [['hashalg', 4], ['major', 2], ['minor', 2], ['build', 2], ['rev', 2], ['flags', 4], ['pk', blobIdx], ['name', strIdx], ['culture', strIdx]],
    AssemblyRef: [['major', 2], ['minor', 2], ['build', 2], ['rev', 2], ['flags', 4], ['pkt', blobIdx], ['name', strIdx], ['culture', strIdx], ['hash', blobIdx]],
    File: [['flags', 4], ['name', strIdx], ['hash', blobIdx]],
    ExportedType: [['flags', 4], ['typedefid', 4], ['name', strIdx], ['ns', strIdx], ['impl', coded.Implementation, 'coded']],
    ManifestResource: [['offset', 4], ['flags', 4], ['name', strIdx], ['impl', coded.Implementation, 'coded']],
    NestedClass: [['nested', sz(2)], ['enclosing', sz(2)]],
    GenericParam: [['number', 2], ['flags', 2], ['owner', coded.TypeOrMethodDef, 'coded'], ['name', strIdx]],
    MethodSpec: [['method', coded.MethodDefOrRef, 'coded'], ['inst', blobIdx]],
    GenericParamConstraint: [['owner', sz(42)], ['constraint', coded.TypeDefOrRef, 'coded']],
  };

  const tableData = {};
  const metaSize = streams['#~'] ? streams['#~'].size : 0;
  for (let t = 0; t < 64; t++) {
    const name = T[t];
    if (!name || !rows[t]) continue;
    const cols = col[name];
    if (!cols) { console.error('MISSING COLUMNS for table ' + name); continue; }
    const rowSize = cols.reduce((a, c) => a + c[1], 0);
    const start = p;
    tableData[name] = { start, rowSize, count: rows[t], cols };
    p += rowSize * rows[t];
  }
  if (metaSize && p > metaSize) {
    console.error(`WARNING: table data end ${p} exceeds #~ stream size ${metaSize} (overflow ${p - metaSize})`);
    for (const [name, td] of Object.entries(tableData)) {
      if (td.start + td.rowSize * td.count > metaSize) {
        console.error(`  overflow candidate: ${name} start=${td.start} rowSize=${td.rowSize} count=${td.count} end=${td.start + td.rowSize * td.count}`);
      }
    }
  }
  return { rows, tableData, start: p, heapSizes, coded, col, metaSize };
}

function readRow(md, tables, table, index) {
  const td = tables.tableData[table];
  const o = td.start + index * td.rowSize;
  const out = {}; let p = o;
  for (const [name, size] of td.cols) {
    if (name === 'pad') { p += size; continue; }
    let v;
    if (size === 2) { v = md.readUInt16LE(p); p += 2; }
    else { v = md.readUInt32LE(p); p += 4; }
    out[name] = v;
  }
  out.__offset = o;
  return out;
}

// ---------------------------------------------------------- type name builder
function decodeTypeName(sig, ctx, pos, depth = 0) {
  if (pos >= sig.length || depth > 16) return ['?', sig.length];
  const rd16 = (p) => (p + 2 <= sig.length ? sig.readUInt16LE(p) : 0);
  const rd32 = (p) => (p + 4 <= sig.length ? sig.readUInt32LE(p) : 0);
  const et = sig[pos];
  switch (et) {
    case ECMA.ELEMENT_TYPE_VOID: return ['System.Void', pos + 1];
    case ECMA.ELEMENT_TYPE_BOOLEAN: return ['System.Boolean', pos + 1];
    case ECMA.ELEMENT_TYPE_CHAR: return ['System.Char', pos + 1];
    case ECMA.ELEMENT_TYPE_I1: return ['System.SByte', pos + 1];
    case ECMA.ELEMENT_TYPE_U1: return ['System.Byte', pos + 1];
    case ECMA.ELEMENT_TYPE_I2: return ['System.Int16', pos + 1];
    case ECMA.ELEMENT_TYPE_U2: return ['System.UInt16', pos + 1];
    case ECMA.ELEMENT_TYPE_I4: return ['System.Int32', pos + 1];
    case ECMA.ELEMENT_TYPE_U4: return ['System.UInt32', pos + 1];
    case ECMA.ELEMENT_TYPE_I8: return ['System.Int64', pos + 1];
    case ECMA.ELEMENT_TYPE_U8: return ['System.UInt64', pos + 1];
    case ECMA.ELEMENT_TYPE_R4: return ['System.Single', pos + 1];
    case ECMA.ELEMENT_TYPE_R8: return ['System.Double', pos + 1];
    case ECMA.ELEMENT_TYPE_STRING: return ['System.String', pos + 1];
    case ECMA.ELEMENT_TYPE_OBJECT: return ['System.Object', pos + 1];
    case ECMA.ELEMENT_TYPE_I: return ['System.IntPtr', pos + 1];
    case ECMA.ELEMENT_TYPE_U: return ['System.UIntPtr', pos + 1];
    case ECMA.ELEMENT_TYPE_TYPEDBYREF: return ['System.TypedReference', pos + 1];
    case ECMA.ELEMENT_TYPE_PTR: { const [n, np] = decodeTypeName(sig, ctx, pos + 1, depth + 1); return [n + '*', np]; }
    case ECMA.ELEMENT_TYPE_BYREF: { const [n, np] = decodeTypeName(sig, ctx, pos + 1, depth + 1); return [n + '&', np]; }
    case ECMA.ELEMENT_TYPE_SZARRAY: { const [n, np] = decodeTypeName(sig, ctx, pos + 1, depth + 1); return [n + '[]', np]; }
    case ECMA.ELEMENT_TYPE_VAR: { const n = rd16(pos + 1); return [`!${n}`, pos + 3]; }
    case ECMA.ELEMENT_TYPE_MVAR: { const n = rd16(pos + 1); return [`!!${n}`, pos + 3]; }
    case ECMA.ELEMENT_TYPE_VALUETYPE:
    case ECMA.ELEMENT_TYPE_CLASS: {
      const tok = rd32(pos + 1);
      return [ctx.typeRefName(tok), pos + 5];
    }
    case ECMA.ELEMENT_TYPE_GENERICINST: {
      const tok = rd32(pos + 1);
      const argc = Math.min(rd32(pos + 5), Math.max(0, sig.length - pos - 9));
      let p = pos + 9;
      const args = [];
      for (let i = 0; i < argc; i++) { const [n, np] = decodeTypeName(sig, ctx, p, depth + 1); args.push(n); p = np; }
      const base = ctx.typeRefName(tok).replace(/`\d+$/, '');
      return [`${base}<${args.join(', ')}>`, p];
    }
    case ECMA.ELEMENT_TYPE_ARRAY: {
      const [n, np] = decodeTypeName(sig, ctx, pos + 1, depth + 1);
      const rank = rd32(np);
      let p = np + 4;
      const nsizes = rd32(p); p += 4;
      p += nsizes * 4;
      const nlo = rd32(p); p += 4;
      p += nlo * 4;
      return [`${n}[,]`, p];
    }
    case ECMA.ELEMENT_TYPE_CMOD_REQD:
    case ECMA.ELEMENT_TYPE_CMOD_OPT: {
      const tok = rd32(pos + 1);
      const [n, np] = decodeTypeName(sig, ctx, pos + 5);
      return [n, np];
    }
    case ECMA.ELEMENT_TYPE_SENTINEL: { const [n, np] = decodeTypeName(sig, ctx, pos + 1, depth + 1); return [n, np]; }
    default: return [`?et${et}`, pos + 1];
  }
}

function safeMethodSig(sig, ctx) {
  try {
    const r = decodeMethodSig(sig, ctx);
    return r;
  } catch (e) {
    return { ret: '?', params: [], error: e.message };
  }
}

function decodeMethodSig(sig, ctx) {
  if (!sig || sig.length === 0) return { ret: '?', params: [] };
  const r16 = (p) => (p + 2 <= sig.length ? sig.readUInt16LE(p) : 0);
  const r32 = (p) => (p + 4 <= sig.length ? sig.readUInt32LE(p) : 0);
  let p = 0;
  const cc = sig[p++];
  const generic = (cc & 0x10) ? r16(p) : 0;
  if (generic) p += 2;
  const paramCount = Math.min(r32(p), 64); p += 4;
  const [ret, pr] = decodeTypeName(sig, ctx, p, 0); p = pr;
  const params = [];
  for (let i = 0; i < paramCount; i++) {
    if (p >= sig.length) break;
    const [t, np] = decodeTypeName(sig, ctx, p, 0); p = np;
    params.push(t);
  }
  return { ret, params, generic };
}

// --------------------------------------------------------------- IL decoding
// operand sizes: 0=none,1=int8,2=int32,3=int64,4=float32,5=float64,6=token,
//                7=string(token),8=branch32,9=branch8,10=switch
const OP = new Array(0x100).fill(null);
function defOp(code, operand) { OP[code] = operand; }
// single-byte opcodes with operand sizes
const ONE = {
  0x0e: 1, 0x0f: 1, 0x10: 1, 0x11: 1, 0x12: 1, 0x13: 1,
  0x1f: 1, 0x20: 4, 0x21: 8, 0x22: 4, 0x23: 8, 0x25: 1,
  0x26: 1, 0x27: 1, 0x28: 6, 0x29: 6,
  0x2a: 0, 0x2b: 1, 0x2c: 1, 0x2d: 1, 0x2e: 1, 0x2f: 1, 0x30: 1, 0x31: 1, 0x32: 1, 0x33: 1, 0x34: 1, 0x35: 1, 0x36: 1, 0x37: 1,
  0x38: 8, 0x39: 8, 0x3a: 8, 0x3b: 8, 0x3c: 8, 0x3d: 8, 0x3e: 8, 0x3f: 8, 0x40: 8, 0x41: 8,
  0x42: 8, 0x43: 8, 0x44: 8, 0x45: 8, 0x46: 8, 0x47: 8, 0x48: 8, 0x49: 8, 0x4a: 8, 0x4b: 8,
  0x4c: 8, 0x4d: 8, 0x4e: 8, 0x4f: 8, 0x50: 8, 0x51: 8, 0x52: 8, 0x53: 8, 0x54: 8, 0x55: 8,
  0x56: 8, 0x57: 8, 0x58: 8, 0x59: 8, 0x5a: 8, 0x5b: 8, 0x5c: 8, 0x5d: 8, 0x5e: 8, 0x5f: 8,
  0x60: 8, 0x61: 8, 0x62: 8, 0x63: 8, 0x64: 8, 0x65: 8, 0x66: 8, 0x67: 8, 0x68: 8, 0x69: 8,
  0x6a: 0, 0x6b: 0, 0x6c: 0, 0x6d: 0, 0x6e: 0,
  0x6f: 6, 0x70: 6, 0x71: 6, 0x72: 7, 0x73: 6, 0x74: 6, 0x75: 6, 0x76: 6, 0x77: 6, 0x78: 6,
  0x79: 6, 0x7a: 0, 0x7b: 6, 0x7c: 6, 0x7d: 6, 0x7e: 6, 0x7f: 0, 0x80: 6, 0x81: 6, 0x82: 6,
  0x83: 6, 0x84: 6, 0x85: 6, 0x86: 6, 0x87: 6, 0x88: 6, 0x89: 6, 0x8a: 6, 0x8b: 6, 0x8c: 6,
  0x8d: 6, 0x8e: 6, 0x8f: 6, 0x90: 6, 0x91: 6, 0x92: 6, 0x93: 6, 0x94: 6, 0x95: 6, 0x96: 6,
  0x97: 6, 0x98: 6, 0x99: 6, 0x9a: 6, 0x9b: 6, 0x9c: 6, 0x9d: 6, 0x9e: 6, 0x9f: 6,
  0xa0: 6, 0xa1: 6, 0xa2: 6, 0xa3: 6, 0xa4: 6, 0xa5: 6,
  0xb3: 8, 0xb4: 8, 0xb5: 8, 0xb6: 8, 0xb7: 8, 0xb8: 8,
  0xb9: 8, 0xba: 8, 0xbb: 8, 0xbc: 8, 0xbd: 8, 0xbe: 8, 0xbf: 8, 0xc0: 8, 0xc1: 8, 0xc2: 8, 0xc3: 8,
  0xd0: 6, 0xd1: 6, 0xd2: 6, 0xd3: 6, 0xd4: 6, 0xd5: 6, 0xd6: 6, 0xd7: 6, 0xd8: 6, 0xd9: 6,
  0xda: 6, 0xdb: 6, 0xdc: 6, 0xdd: 6, 0xde: 6,
  0xfe: 1,
};
for (const [c, o] of Object.entries(ONE)) defOp(+c, o);

// two-byte opcodes 0xFE xx
const TWO = {
  0x00: 0, 0x01: 0, 0x02: 8, 0x03: 6, 0x04: 6, 0x05: 6, 0x06: 6, 0x07: 6, 0x08: 6, 0x09: 6,
  0x0a: 0, 0x0b: 0, 0x0c: 0, 0x0d: 0, 0x0e: 0, 0x0f: 0,
  0x12: 1, 0x15: 1, 0x16: 1, 0x17: 1, 0x18: 1, 0x19: 2, 0x1a: 2, 0x1b: 2, 0x1c: 2, 0x1d: 2, 0x1e: 2,
  0x1f: 2, 0x20: 2, 0x21: 2,
};
const OP2 = TWO;

function decodeIL(body, ctx) {
  const calls = [], strings = [], fields = [], types = [];
  let p = 0;
  while (p < body.length) {
    const op = body[p++];
    let code = op, operandSize = 0, prefix2 = false;
    if (op === 0xfe) { prefix2 = true; code = body[p++]; operandSize = OP2[code] ?? 0; }
    else operandSize = ONE[op] ?? 0;

    if (operandSize === 7) { // ldstr
      const tok = body.readUInt32LE(p);
      const s = userString(ctx.streams, ctx.md, tok & 0xffffff);
      if (s !== null) strings.push(s);
      p += 4;
    } else if (operandSize === 6) {
      const tok = body.readUInt32LE(p);
      const table = (tok >>> 24) & 0xff;
      const row = tok & 0xffffff;
      if (table === 0x06) { const m = ctx.methodName(row); if (m) calls.push(m); }
      else if (table === 0x0a) { const m = ctx.memberRefName(row); if (m) calls.push(m); }
      else if (table === 0x2b) { const m = ctx.methodSpecName(row); if (m) calls.push(m); }
      else if (table === 0x04) { const f = ctx.fieldName(row); if (f) fields.push(f); }
      else if (table === 0x01) { const t = ctx.typeRefNameByRow(row); if (t) types.push(t); }
      else if (table === 0x02) { const t = ctx.typeDefNameByRow(row); if (t) types.push(t); }
      p += 4;
    } else if (operandSize === 10) {
      const n = body.readUInt32LE(p); p += 4 + n * 4;
    } else if (operandSize === 1) p += 1;
    else if (operandSize === 2) p += 2;
    else if (operandSize === 4) p += 4;
    else if (operandSize === 8) p += 8;
  }
  return { calls, strings, fields, types };
}

// ------------------------------------------------------------------- driver
function analyse(file) {
  const buf = fs.readFileSync(file);
  const pe = readPE(buf);
  const cliDir = pe.dirs[14];
  if (!cliDir || !cliDir.rva) throw new Error('not a managed assembly (no CLI header)');
  const cliOff = pe.rvaToOff(cliDir.rva);
  const cli = {
    metaRva: buf.readUInt32LE(cliOff + 8),
    metaSize: buf.readUInt32LE(cliOff + 12),
    flags: buf.readUInt32LE(cliOff + 16),
    entryToken: buf.readUInt32LE(cliOff + 20),
  };
  const mdInfo = readMetadata(buf, cli.metaRva, (rva) => pe.rvaToOff(rva));
  const md = mdInfo.md;
  const tables = parseTables(md, mdInfo.streams);

  const typeDefNames = new Map();
  const typeDefRows = [];
  for (let i = 0; i < tables.rows[2]; i++) {
    const r = readRow(md, tables, 'TypeDef', i);
    const ns = strAt(mdInfo.streams, md, r.ns);
    const nm = strAt(mdInfo.streams, md, r.name);
    const full = ns ? `${ns}.${nm}` : nm;
    typeDefNames.set(i + 1, full);
    typeDefRows.push({ ...r, ns, name: nm, full, __idx: i });
  }
  const typeRefNames = new Map();
  for (let i = 0; i < tables.rows[1]; i++) {
    const r = readRow(md, tables, 'TypeRef', i);
    const ns = strAt(mdInfo.streams, md, r.ns);
    const nm = strAt(mdInfo.streams, md, r.name);
    typeRefNames.set(i + 1, ns ? `${ns}.${nm}` : nm);
  }

  const ctx = {
    md, streams: mdInfo.streams, tables,
    typeRefName(tok) {
      const tag = tok & 3, row = tok >>> 2;
      if (tag === 0) return typeDefNames.get(row) || `TypeDef#${row}`;
      if (tag === 1) return typeRefNames.get(row) || `TypeRef#${row}`;
      return `TypeSpec#${row}`;
    },
    typeRefNameByRow(row) { return typeRefNames.get(row); },
    typeDefNameByRow(row) { return typeDefNames.get(row); },
    methodName(row) {
      if (row > tables.rows[6]) return null;
      const r = readRow(md, tables, 'MethodDef', row - 1);
      return strAt(mdInfo.streams, md, r.name);
    },
    memberRefName(row) {
      if (row > tables.rows[10]) return null;
      const r = readRow(md, tables, 'MemberRef', row - 1);
      const nm = strAt(mdInfo.streams, md, r.name);
      let owner = '';
      if ((r.class & 3) === 3) owner = typeRefNames.get(r.class >>> 2) || '';
      else if ((r.class & 3) === 0) owner = typeDefNames.get(r.class >>> 2) || '';
      return owner ? `${owner}::${nm}` : nm;
    },
    methodSpecName(row) {
      if (row > tables.rows[43]) return null;
      const r = readRow(md, tables, 'MethodSpec', row - 1);
      const blob = blobAt(mdInfo.streams, md, r.inst);
      return blob.length ? 'MethodSpec<>' : 'MethodSpec<>';
    },
    fieldName(row) {
      if (row > tables.rows[4]) return null;
      const r = readRow(md, tables, 'Field', row - 1);
      return strAt(mdInfo.streams, md, r.name);
    },
  };

  // TypeDef -> method ranges
  const methods = [];
  const methodsByType = new Map();
  for (let i = 0; i < typeDefRows.length; i++) {
    const td = typeDefRows[i];
    const start = td.methodlist;
    const end = (i + 1 < typeDefRows.length) ? typeDefRows[i + 1].methodlist : tables.rows[6] + 1;
    const list = [];
    for (let m = start; m < end; m++) {
      const r = readRow(md, tables, 'MethodDef', m - 1);
      const sig = safeMethodSig(blobAt(mdInfo.streams, md, r.sig), ctx);
      const rec = {
        token: 0x06000000 | m,
        row: m, typeIndex: i, type: td.full, name: strAt(mdInfo.streams, md, r.name),
        rva: r.rva, flags: r.flags, implFlags: r.implflags,
        ret: sig.ret, params: sig.params,
      };
      list.push(rec);
      methods.push(rec);
    }
    methodsByType.set(i, list);
  }

  // fields per type (by type index)
  const fieldsByType = new Map();
  for (let i = 0; i < typeDefRows.length; i++) {
    const td = typeDefRows[i];
    const start = td.fieldlist;
    const end = (i + 1 < typeDefRows.length) ? typeDefRows[i + 1].fieldlist : tables.rows[4] + 1;
    const list = [];
    if (end > start && end - start < 4096) {
      for (let f = start; f < end; f++) {
        const r = readRow(md, tables, 'Field', f - 1);
        list.push({ token: 0x04000000 | f, name: strAt(mdInfo.streams, md, r.name), flags: r.flags });
      }
    }
    fieldsByType.set(i, list);
  }

  return { file, buf, pe, cli, mdInfo, tables, md, ctx, typeDefRows, typeRefNames, methods, methodsByType, fieldsByType };
}

module.exports = { analyse, readPE, decodeIL, userString, blobAt, strAt };

if (require.main === module) {
  const file = process.argv[2];
  const A = analyse(file);
  const outDir = process.argv[3];
  const short = require('path').basename(file, '.dll');

  // types
  let t = `# Types in ${short}\n\n`;
  const byNs = new Map();
  for (const td of A.typeDefRows) {
    const ns = td.ns || '(global)';
    if (!byNs.has(ns)) byNs.set(ns, []);
    byNs.get(ns).push(td);
  }
  for (const [ns, list] of [...byNs.entries()].sort((a, b) => a[0].localeCompare(b[0]))) {
    t += `\n## ${ns} (${list.length})\n`;
    for (const td of list.sort((a, b) => a.name.localeCompare(b.name))) {
      const ms = A.methodsByType.get(td.__idx) || [];
      const fs_ = A.fieldsByType.get(td.__idx) || [];
      t += `- ${td.name}  [methods=${ms.length} fields=${fs_.length} flags=0x${td.flags.toString(16)}]\n`;
      for (const f of fs_) t += `    field ${f.name}\n`;
      for (const m of ms) t += `    ${m.name}(${m.params.join(', ')}) : ${m.ret}\n`;
    }
  }
  fs.writeFileSync(`${outDir}/${short}-clr-types.txt`, t, 'utf8');
  console.log(`${short}: types=${A.typeDefRows.length} methods=${A.methods.length} -> ${outDir}/${short}-clr-types.txt`);
}
