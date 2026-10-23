"""Pure (hardware-free) parts of the Parakeet TDT pipeline: chunk planning,
TDT greedy decoding, token -> word -> segment grouping and overlap merging.

The model graphs are static-shape, so audio is cut into fixed windows. Each
window is decoded independently and the windows are stitched together at the
middle of their overlap, which needs no token-level de-duplication heuristics.
"""
from dataclasses import dataclass

SAMPLE_RATE = 16000
WINDOW = 240000          # samples the mel graph accepts (15 s)
OVERLAP = 48000          # 3 s shared by neighbouring windows
FRAME_SECS = 0.08        # one encoder frame
BLANK = 8192
NUM_TOKENS = 8193        # 8192 pieces + blank; duration logits follow
DURATIONS = (0, 1, 2, 3, 4)
MAX_SYMBOLS_PER_FRAME = 10
WORD_MARK = "▁"

# Segment grouping
PAUSE_SPLIT_SECS = 1.2
SENTENCE_SPLIT_SECS = 10.0
SENTENCE_SPLIT_WORDS = 25
HARD_SPLIT_SECS = 30.0
HARD_SPLIT_WORDS = 60
DUPLICATE_WORD_JITTER_SECS = 0.2


@dataclass(frozen=True)
class Chunk:
    start: int  # sample index, inclusive
    end: int    # sample index, exclusive


@dataclass(frozen=True)
class Word:
    text: str
    start: float  # seconds from the start of the audio
    end: float


def plan_chunks(n_samples, window=WINDOW, overlap=OVERLAP):
    """Fixed windows, each sharing `overlap` samples with the previous one.
    The last window may be shorter; the caller pads it."""
    if n_samples <= 0:
        return []
    stride = window - overlap
    chunks, start = [], 0
    while True:
        end = min(start + window, n_samples)
        chunks.append(Chunk(start, end))
        if end >= n_samples:
            return chunks
        start += stride


def tdt_greedy(n_frames, predictor, joint, init_state):
    """Greedy TDT decoding of one window. Returns [(token_id, frame)].

    predictor(token, state) -> (dec_out, new_state): the prediction network run
        on the last emitted token. new_state is committed only when a
        non-blank token is emitted.
    joint(t, dec_out) -> logits: `NUM_TOKENS` token scores followed by one score
        per entry of DURATIONS.
    """
    out = []
    state = init_state
    last = BLANK
    dec_out, new_state = predictor(last, state)
    t = 0
    symbols = 0
    while t < n_frames:
        logits = joint(t, dec_out)
        token = int(logits[:NUM_TOKENS].argmax())
        dur = DURATIONS[int(logits[NUM_TOKENS:NUM_TOKENS + len(DURATIONS)].argmax())]
        if token != BLANK:
            out.append((token, t))
            state, last = new_state, token
            dec_out, new_state = predictor(last, state)
            symbols += 1
        else:
            dur = max(dur, 1)  # a blank must move forward
        if dur > 0:
            t += dur
            symbols = 0
        elif symbols >= MAX_SYMBOLS_PER_FRAME:
            t += 1
            symbols = 0
    return out


def load_vocab(path):
    """{id: piece} from either {"0": "x", ...} or {"id_to_token": {...}}."""
    import json
    with open(path, encoding="utf-8") as f:
        raw = json.load(f)
    raw = raw.get("id_to_token", raw)
    return {int(k): v for k, v in raw.items()}


def _is_special(piece):
    return piece.startswith("<") and piece.endswith(">") and len(piece) > 2


def words_from_pieces(pieces, vocab):
    """[(token_id, time_secs)] -> [Word]. Word boundaries are the sentencepiece
    marker; punctuation pieces attach to the word before them."""
    words, cur, cur_start, cur_end = [], "", 0.0, 0.0
    for token, time in pieces:
        piece = vocab.get(token)
        if token == BLANK or piece is None or piece == "" or _is_special(piece):
            continue
        starts_word = piece.startswith(WORD_MARK)
        piece = piece.lstrip(WORD_MARK)
        if not piece:
            if cur:
                words.append(Word(cur, cur_start, cur_end))
                cur = ""
            continue
        if starts_word or not cur:
            if cur:
                words.append(Word(cur, cur_start, cur_end))
            cur, cur_start = piece, time
        else:
            cur += piece
        cur_end = time + FRAME_SECS
    if cur:
        words.append(Word(cur, cur_start, cur_end))
    return words


def merge_chunk_words(chunks, per_chunk_words):
    """Stitch per-window words: each window owns the span between the midpoints
    of its overlaps with its neighbours. Words are assigned by their start time."""
    n = len(chunks)
    cuts = [(chunks[i].end + chunks[i + 1].start) / 2 / SAMPLE_RATE for i in range(n - 1)]
    merged = []
    for i, words in enumerate(per_chunk_words):
        lo = cuts[i - 1] if i > 0 else float("-inf")
        hi = cuts[i] if i < n - 1 else float("inf")
        merged.extend(w for w in words if lo <= w.start < hi)
    # A word straddling a cut can be heard by both windows a frame or two apart.
    deduped = []
    for w in merged:
        if deduped and deduped[-1].text == w.text and abs(deduped[-1].start - w.start) < DUPLICATE_WORD_JITTER_SECS:
            continue
        deduped.append(w)
    return deduped


def _ends_sentence(text):
    return text[-1:] in (".", "?", "!")


def segments_from_words(words):
    """Group words into transcript lines: a long pause, a sentence end once the
    line is long enough, or a hard length cap each start a new line."""
    segments, cur = [], []

    def flush():
        if cur:
            segments.append({"start": cur[0].start, "end": cur[-1].end, "text": " ".join(w.text for w in cur)})
            cur.clear()

    for w in words:
        if cur and w.start - cur[-1].end > PAUSE_SPLIT_SECS:
            flush()
        cur.append(w)
        span = w.end - cur[0].start
        if len(cur) >= HARD_SPLIT_WORDS or span >= HARD_SPLIT_SECS:
            flush()
        elif _ends_sentence(w.text) and (span >= SENTENCE_SPLIT_SECS or len(cur) >= SENTENCE_SPLIT_WORDS):
            flush()
    flush()
    return segments
