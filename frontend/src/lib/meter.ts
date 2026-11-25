// The mic test's meter. Levels come from the backend on `level_of`'s 0..1
// scale (dictation/segmenter.rs): -50 dBFS is 0, full scale is 1.

export const levelDb = (level: number): number => Math.round(level * 50 - 50)

export const levelPct = (level: number): number => Math.min(100, Math.max(0, level * 100))
