"""Hardware-free tests for the chunk planner, overlap merge, TDT decoder and
word/segment grouping. Run: <cache_root>\\venv\\Scripts\\python -m pytest tools/transcribe
(or `python -m unittest` from this folder; pytest is optional)."""
import unittest

import numpy as np

import pipeline as p


def logits(token, dur_idx):
    out = np.full(p.NUM_TOKENS + len(p.DURATIONS), -10.0, dtype=np.float32)
    out[token] = 5.0
    out[p.NUM_TOKENS + dur_idx] = 5.0
    return out


class PlanChunks(unittest.TestCase):
    def test_short_audio_is_one_chunk(self):
        self.assertEqual(p.plan_chunks(1000), [p.Chunk(0, 1000)])

    def test_exact_window_is_one_chunk(self):
        self.assertEqual(p.plan_chunks(p.WINDOW), [p.Chunk(0, p.WINDOW)])

    def test_long_audio_overlaps_and_covers_everything(self):
        n = 3 * p.WINDOW + 777
        chunks = p.plan_chunks(n)
        self.assertEqual(chunks[0].start, 0)
        self.assertEqual(chunks[-1].end, n)
        for a, b in zip(chunks, chunks[1:]):
            self.assertEqual(a.end - b.start, p.OVERLAP)
            self.assertEqual(a.end - a.start, p.WINDOW)

    def test_empty_audio_has_no_chunks(self):
        self.assertEqual(p.plan_chunks(0), [])


class Vocabulary(unittest.TestCase):
    def test_pieces_join_into_words(self):
        vocab = {0: "▁hel", 1: "lo", 2: "▁world", 3: "."}
        pieces = [(0, 1.0), (1, 1.08), (2, 1.5), (3, 1.6)]
        words = p.words_from_pieces(pieces, vocab)
        self.assertEqual([w.text for w in words], ["hello", "world."])
        self.assertAlmostEqual(words[0].start, 1.0)
        self.assertAlmostEqual(words[1].end, 1.6 + p.FRAME_SECS)

    def test_blank_and_unknown_ids_are_skipped(self):
        vocab = {0: "▁a"}
        words = p.words_from_pieces([(p.BLANK, 0.0), (0, 0.1), (99999, 0.2)], vocab)
        self.assertEqual([w.text for w in words], ["a"])

    def test_first_piece_without_marker_still_starts_a_word(self):
        words = p.words_from_pieces([(0, 0.0)], {0: "ok"})
        self.assertEqual([w.text for w in words], ["ok"])


class Merge(unittest.TestCase):
    def w(self, text, start):
        return p.Word(text, start, start + 0.2)

    def test_overlap_is_cut_at_the_midpoint_without_duplicates(self):
        chunks = [p.Chunk(0, 15 * p.SAMPLE_RATE), p.Chunk(12 * p.SAMPLE_RATE, 20 * p.SAMPLE_RATE)]
        # overlap is 12..15 s, midpoint 13.5 s
        first = [self.w("a", 1), self.w("b", 12.5), self.w("c", 13.0), self.w("d", 14.0)]
        second = [self.w("c", 13.0), self.w("d", 14.0), self.w("e", 16.0)]
        merged = p.merge_chunk_words(chunks, [first, second])
        self.assertEqual([x.text for x in merged], ["a", "b", "c", "d", "e"])

    def test_cut_prefers_the_chunk_with_more_context(self):
        chunks = [p.Chunk(0, 15 * p.SAMPLE_RATE), p.Chunk(12 * p.SAMPLE_RATE, 20 * p.SAMPLE_RATE)]
        first = [self.w("x", 14.0)]  # past the 13.5 s cut: belongs to chunk two
        second = [self.w("y", 14.0)]
        merged = p.merge_chunk_words(chunks, [first, second])
        self.assertEqual([x.text for x in merged], ["y"])

    def test_single_chunk_is_passed_through(self):
        words = [self.w("a", 0), self.w("b", 1)]
        self.assertEqual(p.merge_chunk_words([p.Chunk(0, 10)], [words]), words)

    def test_same_word_twice_within_jitter_is_collapsed(self):
        chunks = [p.Chunk(0, 15 * p.SAMPLE_RATE), p.Chunk(12 * p.SAMPLE_RATE, 20 * p.SAMPLE_RATE)]
        first = [self.w("so", 13.46)]
        second = [self.w("so", 13.54)]
        merged = p.merge_chunk_words(chunks, [first, second])
        self.assertEqual([x.text for x in merged], ["so"])

    def test_repeated_word_further_apart_is_kept(self):
        words = [self.w("no", 1.0), self.w("no", 1.5)]
        self.assertEqual(len(p.merge_chunk_words([p.Chunk(0, 10)], [words])), 2)


class Segments(unittest.TestCase):
    def test_pause_splits_segments(self):
        words = [p.Word("hi", 0.0, 0.3), p.Word("there", 0.4, 0.8), p.Word("later", 5.0, 5.4)]
        segs = p.segments_from_words(words)
        self.assertEqual([s["text"] for s in segs], ["hi there", "later"])
        self.assertEqual(segs[1]["start"], 5.0)
        self.assertEqual(segs[0]["end"], 0.8)

    def test_long_run_without_pauses_is_split(self):
        words = [p.Word(f"w{i}", i * 0.3, i * 0.3 + 0.2) for i in range(200)]
        segs = p.segments_from_words(words)
        self.assertGreater(len(segs), 1)
        self.assertEqual(sum(len(s["text"].split()) for s in segs), 200)

    def test_sentence_end_splits_once_segment_is_long_enough(self):
        words = [p.Word(f"w{i}", i * 0.5, i * 0.5 + 0.3) for i in range(30)]
        words[24] = p.Word("end.", 24 * 0.5, 24 * 0.5 + 0.3)
        segs = p.segments_from_words(words)
        self.assertTrue(segs[0]["text"].endswith("end."))

    def test_empty(self):
        self.assertEqual(p.segments_from_words([]), [])


class TdtDecode(unittest.TestCase):
    def run_decode(self, n_frames, table, default=(p.BLANK, 1)):
        """table: {(t, last_token): (token, duration_index)}; predictor output is
        just the last committed token, so the joint can key off it."""
        def predictor(token, state):
            return token, state

        def joint(t, dec_out):
            return logits(*table.get((t, dec_out), default))

        return p.tdt_greedy(n_frames, predictor, joint, init_state=None)

    def test_all_blank_skips_by_duration(self):
        calls = []
        def predictor(token, state):
            return token, state
        def joint(t, dec_out):
            calls.append(t)
            return logits(p.BLANK, 4)  # duration 4
        out = p.tdt_greedy(10, predictor, joint, init_state=None)
        self.assertEqual(out, [])
        self.assertEqual(calls, [0, 4, 8])

    def test_token_is_emitted_at_its_frame_and_advances_by_duration(self):
        out = self.run_decode(6, {(1, p.BLANK): (7, 1), (3, 7): (9, 2)})
        self.assertEqual(out, [(7, 1), (9, 3)])

    def test_zero_duration_blank_still_advances(self):
        calls = []
        def joint(t, dec_out):
            calls.append(t)
            return logits(p.BLANK, 0)
        p.tdt_greedy(3, lambda tok, st: (tok, st), joint, init_state=None)
        self.assertEqual(calls, [0, 1, 2])

    def test_zero_duration_token_allows_second_token_on_same_frame(self):
        out = self.run_decode(2, {(0, p.BLANK): (5, 0), (0, 5): (6, 1)})
        self.assertEqual(out, [(5, 0), (6, 0)])

    def test_symbols_per_frame_are_capped(self):
        def joint(t, dec_out):
            return logits(5, 0)  # never blank, never advances
        out = p.tdt_greedy(2, lambda tok, st: (tok, st), joint, init_state=None)
        self.assertEqual(len(out), 2 * p.MAX_SYMBOLS_PER_FRAME)

    def test_state_only_advances_on_emission(self):
        seen = []
        def predictor(token, state):
            seen.append((token, state))
            return token, state + 1
        def joint(t, dec_out):
            return logits(5, 1) if t == 1 else logits(p.BLANK, 1)
        p.tdt_greedy(4, predictor, joint, init_state=0)
        # initial predictor run on blank/state 0; after emitting at t=1 it is rerun
        # with the committed token and the state produced by the first run.
        self.assertEqual(seen, [(p.BLANK, 0), (5, 1)])

    def test_zero_frames(self):
        self.assertEqual(p.tdt_greedy(0, lambda t, s: (t, s), lambda t, d: logits(5, 1), init_state=None), [])


if __name__ == "__main__":
    unittest.main()
