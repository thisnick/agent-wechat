#!/usr/bin/env node
// Writes interfaces/crates/<crate>.txt: the public API of each published Rust library,
// read from rustdoc JSON built with the pinned toolchain (RUSTC_BOOTSTRAP enables the
// JSON output on stable). Usage: node scripts/interfaces/rust-crates.mjs [--check]
import { execFileSync } from 'node:child_process';
import { mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { crateNames } from '../native-crates.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const manifest = join(root, 'packages/native-client-rust/Cargo.toml');
const outputDir = join(root, 'interfaces/crates');
export const MISSING = 'MISSING DOC';

// The first paragraph, on one line: rustdoc's summary of the item.
const firstLine = docs => ((docs ?? '').trim().split(/\n\s*\n/)[0] ?? '').split(/\s+/).filter(Boolean).join(' ');

/** Renders rustdoc JSON (format 57) into sorted `KIND PATH | SIGNATURE | DOC` rows. */
export function renderCrate(doc) {
  const { index, paths } = doc;
  const item = id => index[String(id)];
  const kindOf = entry => Object.keys(entry.inner)[0];
  const canonical = id => paths[String(id)]?.path?.join('::');

  const args = a => {
    if (!a) return '';
    if (a.angle_bracketed) {
      const parts = a.angle_bracketed.args.map(arg => (arg === 'infer' ? '_' : arg.type ? ty(arg.type) : arg.lifetime ?? arg.const?.expr ?? '_'));
      for (const c of a.angle_bracketed.constraints ?? []) {
        const binding = c.binding?.equality ? ` = ${c.binding.equality.type ? ty(c.binding.equality.type) : c.binding.equality.constant?.expr}`
          : c.binding?.constraint ? `: ${c.binding.constraint.map(bound).join(' + ')}` : '';
        parts.push(`${c.name}${args(c.args)}${binding}`);
      }
      return parts.length ? `<${parts.join(', ')}>` : '';
    }
    if (a.parenthesized) return `(${a.parenthesized.inputs.map(ty).join(', ')})${a.parenthesized.output ? ` -> ${ty(a.parenthesized.output)}` : ''}`;
    return '(..)';
  };
  const path = p => `${canonical(p.id) ?? p.path}${args(p.args)}`;
  const hrtb = params => (params?.length ? `for<${params.map(param => param.name).join(', ')}> ` : '');
  const bound = b => {
    if (typeof b === 'string') return b;
    if (b.trait_bound) return `${b.trait_bound.modifier === 'maybe' ? '?' : ''}${hrtb(b.trait_bound.generic_params)}${path(b.trait_bound.trait)}`;
    if (b.outlives) return b.outlives;
    if (b.use) return `use<${b.use.map(arg => arg.lifetime ?? arg.param ?? arg).join(', ')}>`;
    return JSON.stringify(b);
  };
  const ty = t => {
    if (t == null) return '()';
    if (typeof t === 'string') return t === 'infer' ? '_' : t;
    const [kind, v] = Object.entries(t)[0];
    switch (kind) {
      case 'resolved_path': return path(v);
      case 'dyn_trait': return `dyn ${v.traits.map(pt => `${hrtb(pt.generic_params)}${path(pt.trait)}`).join(' + ')}${v.lifetime ? ` + ${v.lifetime}` : ''}`;
      case 'generic': case 'primitive': return v;
      case 'tuple': return `(${v.map(ty).join(', ')}${v.length === 1 ? ',' : ''})`;
      case 'slice': return `[${ty(v)}]`;
      case 'array': return `[${ty(v.type)}; ${v.len}]`;
      case 'pat': return ty(v.type);
      case 'impl_trait': return `impl ${v.map(bound).join(' + ')}`;
      case 'raw_pointer': return `*${v.is_mutable ? 'mut' : 'const'} ${ty(v.type)}`;
      case 'borrowed_ref': return `&${v.lifetime ? `${v.lifetime} ` : ''}${v.is_mutable ? 'mut ' : ''}${ty(v.type)}`;
      case 'qualified_path': return v.trait ? `<${ty(v.self_type)} as ${path(v.trait)}>::${v.name}${args(v.args)}` : `${ty(v.self_type)}::${v.name}${args(v.args)}`;
      case 'function_pointer': return `${hrtb(v.generic_params)}${header(v.header)}fn(${v.sig.inputs.map(([, input]) => ty(input)).join(', ')})${v.sig.output ? ` -> ${ty(v.sig.output)}` : ''}`;
      default: return JSON.stringify(t);
    }
  };
  const header = h => {
    if (!h) return '';
    const abi = h.abi && h.abi !== 'Rust' ? `extern "${typeof h.abi === 'string' ? h.abi : Object.keys(h.abi)[0]}" ` : '';
    return `${h.is_const ? 'const ' : ''}${h.is_async ? 'async ' : ''}${h.is_unsafe ? 'unsafe ' : ''}${abi}`;
  };
  const generics = g => {
    const params = (g?.params ?? []).filter(param => !param.kind.type?.is_synthetic).map(param => {
      if (param.kind.lifetime) return `${param.name}${param.kind.lifetime.outlives.length ? `: ${param.kind.lifetime.outlives.join(' + ')}` : ''}`;
      if (param.kind.const) return `const ${param.name}: ${ty(param.kind.const.type)}${param.kind.const.default ? ` = ${param.kind.const.default}` : ''}`;
      const bounds = param.kind.type?.bounds ?? [];
      return `${param.name}${bounds.length ? `: ${bounds.map(bound).join(' + ')}` : ''}${param.kind.type?.default ? ` = ${ty(param.kind.type.default)}` : ''}`;
    });
    return params.length ? `<${params.join(', ')}>` : '';
  };
  const where = g => {
    const predicates = (g?.where_predicates ?? []).map(p => {
      if (p.bound_predicate) return `${hrtb(p.bound_predicate.generic_params)}${ty(p.bound_predicate.type)}: ${p.bound_predicate.bounds.map(bound).join(' + ')}`;
      if (p.lifetime_predicate) return `${p.lifetime_predicate.lifetime}: ${p.lifetime_predicate.outlives.join(' + ')}`;
      if (p.eq_predicate) return `${ty(p.eq_predicate.lhs)} = ${p.eq_predicate.rhs.type ? ty(p.eq_predicate.rhs.type) : p.eq_predicate.rhs.constant?.expr}`;
      return JSON.stringify(p);
    });
    return predicates.length ? ` where ${predicates.join(', ')}` : '';
  };
  const selfInput = t => {
    if (t.generic === 'Self') return 'self';
    if (t.borrowed_ref?.type?.generic === 'Self') return `&${t.borrowed_ref.lifetime ? `${t.borrowed_ref.lifetime} ` : ''}${t.borrowed_ref.is_mutable ? 'mut ' : ''}self`;
    return `self: ${ty(t)}`;
  };
  const signature = f => `${header(f.header)}fn${generics(f.generics)}(${f.sig.inputs.map(([name, input]) => (name === 'self' ? selfInput(input) : `${name}: ${ty(input)}`)).join(', ')})${f.sig.output ? ` -> ${ty(f.sig.output)}` : ''}${where(f.generics)}`;
  const markers = entry => {
    const attrs = JSON.stringify(entry.attrs ?? []);
    const found = [];
    for (const match of attrs.matchAll(/#\[<cfg_trace>\((.*?)\)\]/g)) found.push(`#[cfg(${match[1]})]`);
    if (attrs.includes('non_exhaustive')) found.push('#[non_exhaustive]');
    if (entry.deprecation) found.push('#[deprecated]');
    return found.length ? `${found.join(' ')} ` : '';
  };

  const rows = [];
  const add = (key, sig, docs, required = true) => rows.push([key, sig || '-', firstLine(docs) || (required ? MISSING : '-')]);
  const fields = (owner, ids, stripped) => {
    ids.forEach((id, position) => {
      const field = id == null ? null : item(id);
      if (!field) return;
      const named = !/^\d+$/.test(field.name);
      add(`field ${owner}::${field.name}`, `${markers(field)}${ty(field.inner.struct_field)}`, field.docs, named);
      void position;
    });
    return stripped;
  };
  const structShape = kind => {
    if (kind === 'unit') return { text: 'unit', ids: [], stripped: false };
    if (kind.tuple) return { text: `(${kind.tuple.map(id => (id == null ? '_' : ty(item(id)?.inner.struct_field))).join(', ')})`, ids: kind.tuple, stripped: kind.tuple.includes(null) };
    const plain = kind.plain ?? kind.struct;
    return { text: plain.has_stripped_fields ? '{ private fields }' : '{ public fields }', ids: plain.fields, stripped: plain.has_stripped_fields };
  };
  const impls = (owner, ids) => {
    for (const id of ids ?? []) {
      const block = item(id)?.inner.impl;
      if (!block || block.is_synthetic || block.blanket_impl) continue;
      if (block.trait) {
        add(`impl ${path(block.trait)} for ${owner}`, `${block.is_negative ? '!' : ''}impl${generics(block.generics)}${where(block.generics)}`.trim(), null, false);
        continue;
      }
      for (const memberId of block.items) {
        const member = item(memberId);
        if (!member || member.visibility !== 'public') continue;
        const kind = kindOf(member);
        if (kind === 'function') add(`fn ${owner}::${member.name}`, `${markers(member)}${signature(member.inner.function)}`, member.docs);
        else if (kind === 'assoc_const') add(`const ${owner}::${member.name}`, ty(member.inner.assoc_const.type), member.docs);
        else if (kind === 'assoc_type') add(`type ${owner}::${member.name}`, member.inner.assoc_type.type ? `= ${ty(member.inner.assoc_type.type)}` : '-', member.docs);
      }
    }
  };

  const emit = (entry, at, chain) => {
    const kind = kindOf(entry);
    const v = entry.inner[kind];
    switch (kind) {
      case 'module':
        if (chain.includes(entry.id)) return;
        add(`mod ${at}`, markers(entry).trim(), entry.docs);
        walk(entry, at, [...chain, entry.id]);
        return;
      case 'struct': case 'union': {
        const shape = kind === 'union' ? { text: '{ union }', ids: v.fields, stripped: v.has_stripped_fields } : structShape(v.kind);
        add(`${kind} ${at}`, `${markers(entry)}${kind}${generics(v.generics)} ${shape.text}${where(v.generics)}`, entry.docs);
        fields(at, shape.ids, shape.stripped);
        impls(at, v.impls);
        return;
      }
      case 'enum':
        add(`enum ${at}`, `${markers(entry)}enum${generics(v.generics)}${v.has_stripped_variants ? ' { private variants }' : ''}${where(v.generics)}`, entry.docs);
        for (const variantId of v.variants) {
          const variant = item(variantId);
          const variantKind = variant.inner.variant.kind;
          const shape = variantKind === 'plain' ? '' : structShape(variantKind).text;
          add(`variant ${at}::${variant.name}`, `${markers(variant)}${shape}${variant.inner.variant.discriminant ? ` = ${variant.inner.variant.discriminant.expr}` : ''}`.trim(), variant.docs);
          if (variantKind !== 'plain') { const s = structShape(variantKind); fields(`${at}::${variant.name}`, s.ids, s.stripped); }
        }
        impls(at, v.impls);
        return;
      case 'function':
        add(`fn ${at}`, `${markers(entry)}${signature(v)}`, entry.docs);
        return;
      case 'trait': {
        add(`trait ${at}`, `${markers(entry)}${v.is_unsafe ? 'unsafe ' : ''}trait${generics(v.generics)}${v.bounds.length ? `: ${v.bounds.map(bound).join(' + ')}` : ''}${where(v.generics)}${v.is_dyn_compatible === false ? ' (not dyn-compatible)' : ''}`, entry.docs);
        for (const memberId of v.items) {
          const member = item(memberId);
          const memberKind = kindOf(member);
          if (memberKind === 'function') add(`fn ${at}::${member.name}`, `${member.inner.function.has_body ? 'provided ' : 'required '}${signature(member.inner.function)}`, member.docs);
          else if (memberKind === 'assoc_type') add(`type ${at}::${member.name}`, `${member.inner.assoc_type.bounds.map(bound).join(' + ') || '-'}${member.inner.assoc_type.type ? ` = ${ty(member.inner.assoc_type.type)}` : ''}`, member.docs);
          else if (memberKind === 'assoc_const') add(`const ${at}::${member.name}`, `${ty(member.inner.assoc_const.type)}${member.inner.assoc_const.value ? ' (provided)' : ''}`, member.docs);
        }
        return;
      }
      case 'type_alias':
        add(`type ${at}`, `${markers(entry)}type${generics(v.generics)} = ${ty(v.type)}`, entry.docs);
        return;
      case 'constant':
        add(`const ${at}`, `${markers(entry)}${ty(v.type)}`, entry.docs);
        return;
      case 'static':
        add(`static ${at}`, `${markers(entry)}${v.is_mutable ? 'mut ' : ''}${ty(v.type)}`, entry.docs);
        return;
      case 'macro': case 'proc_macro':
        add(`macro ${at}`, kind === 'macro' ? 'macro_rules!' : v.kind, entry.docs);
        return;
      case 'trait_alias':
        add(`trait-alias ${at}`, `${v.params.map(bound).join(' + ')}`, entry.docs);
        return;
      default:
        add(`${kind} ${at}`, '-', entry.docs);
    }
  };

  function walk(module, at, chain) {
    for (const id of module.inner.module.items) {
      const entry = item(id);
      if (!entry) continue;
      if (kindOf(entry) === 'use') {
        const use = entry.inner.use;
        const target = use.id == null ? undefined : item(use.id);
        if (use.is_glob) {
          if (target && kindOf(target) === 'module') walk(target, at, chain);
          else add(`use ${at}::*`, `re-export ${use.source}::*`, null, false);
        } else if (target && target.crate_id === 0) {
          emit(target, `${at}::${use.name}`, chain);
        } else {
          add(`use ${at}::${use.name}`, `re-export ${use.source}`, null, false);
        }
        continue;
      }
      if (entry.name) emit(entry, `${at}::${entry.name}`, chain);
    }
  }

  const crate = item(doc.root);
  add(`mod ${crate.name}`, '-', crate.docs);
  walk(crate, crate.name, [crate.id]);
  const seen = new Map();
  for (const row of rows) {
    const line = row.join(' | ');
    const key = row[0];
    if (!seen.has(line)) seen.set(line, key);
  }
  // Same key with different signatures (e.g. methods from two impl blocks) stays distinguishable.
  const counts = new Map();
  const out = [];
  for (const [line, key] of [...seen].sort((a, b) => (a[1] === b[1] ? (a[0] < b[0] ? -1 : 1) : a[1] < b[1] ? -1 : 1))) {
    const n = (counts.get(key) ?? 0) + 1;
    counts.set(key, n);
    out.push(n === 1 ? line : line.replace(key, `${key} #${n}`));
  }
  return out;
}

function rustdocJson(name) {
  // RUSTC_BOOTSTRAP is part of cargo's build fingerprint, so these builds get their own target
  // directory; sharing one would make every later normal build start from scratch.
  const target = process.env.INTERFACES_RUSTDOC_TARGET_DIR ?? join(root, 'packages/native-client-rust/target/interfaces-rustdoc');
  execFileSync('cargo', ['rustdoc', '--locked', '--quiet', '--manifest-path', manifest, '-p', name, '--lib', '--', '-Z', 'unstable-options', '--output-format', 'json'], {
    cwd: root, stdio: ['ignore', 'inherit', 'inherit'], env: { ...process.env, RUSTC_BOOTSTRAP: '1', CARGO_TARGET_DIR: target },
  });
  return join(target, 'doc', `${name.replaceAll('-', '_')}.json`);
}

export async function render() {
  const files = {};
  for (const name of crateNames) {
    const doc = JSON.parse(await readFile(rustdocJson(name), 'utf8'));
    if (doc.format_version !== 57) throw new Error(`Unexpected rustdoc JSON format ${doc.format_version} for ${name}; update scripts/interfaces/rust-crates.mjs with the toolchain`);
    const header = `# Public API of the ${name} crate (default features, ${doc.target?.triple ?? 'host target'}). Generated from rustdoc JSON; do not edit.\n# Run \`pnpm interfaces\` after changing the crate. Format: ITEM | SIGNATURE | DOC SUMMARY\n`;
    files[`${name}.txt`] = `${header}${renderCrate(doc).join('\n')}\n`;
  }
  return files;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const check = process.argv.includes('--check');
  const files = await render();
  await mkdir(outputDir, { recursive: true });
  let stale = false;
  for (const name of await readdir(outputDir)) {
    if (files[name]) continue;
    if (check) { stale = true; console.error(`interfaces/crates/${name} is not a published crate listing`); } else await rm(join(outputDir, name));
  }
  for (const [name, content] of Object.entries(files)) {
    const file = join(outputDir, name);
    const current = await readFile(file, 'utf8').catch(() => '');
    if (current.replaceAll('\r\n', '\n') === content) continue;
    if (check) { stale = true; console.error(`interfaces/crates/${name} does not match the crate. Run \`pnpm interfaces\`.`); } else await writeFile(file, content);
  }
  if (stale) process.exitCode = 1;
}
