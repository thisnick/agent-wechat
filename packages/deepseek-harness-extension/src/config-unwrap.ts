/**
 * Volatile config container unwrapping.
 *
 * Schemastery's `.volatile()` fields materialize as frozen reference
 * containers (`{ get(): value }`, cosmokit `createVolatile`) rather than raw
 * values. The settings service reads and writes through these containers; a
 * plugin that wants plain values must unwrap them at read time. Unwrapping is
 * idempotent and safe on plain objects/arrays/primitives.
 */
export function unwrapVolatileConfig<T>(value: T, seen: Set<object> = new Set()): T {
  if (typeof value !== 'object' || value === null) return value;
  const node = value as object;
  if (seen.has(node)) return value;
  seen.add(node);
  const candidate = value as { get?: unknown };
  if (Object.isFrozen(node) && typeof candidate.get === 'function') {
    return unwrapVolatileConfig((candidate.get as () => T)(), seen);
  }
  if (Array.isArray(value)) {
    return value.map((item) => unwrapVolatileConfig(item, seen)) as unknown as T;
  }
  const out: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    out[key] = unwrapVolatileConfig(item, seen);
  }
  return out as T;
}
