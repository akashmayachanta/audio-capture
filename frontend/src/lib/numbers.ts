/** A whole number in [min, max]; empty or invalid input becomes `min`. */
export function clampInt(v: unknown, min: number, max: number): number {
  const n = v === '' || v == null ? NaN : Number(v)
  if (!Number.isFinite(n)) return min
  return Math.min(max, Math.max(min, Math.round(n)))
}
