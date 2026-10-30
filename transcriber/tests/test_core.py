import json
import tempfile
import time
import unittest
from pathlib import Path

import numpy as np

from app import audio, chunking, markdown, recordings, settings

RATE = 16000


def tone(secs, amp=0.1):
    t = np.arange(int(secs * RATE)) / RATE
    return (amp * np.sin(2 * np.pi * 220 * t)).astype(np.float32)


def silence(secs):
    return np.zeros(int(secs * RATE), np.float32)


class ChunkingTest(unittest.TestCase):
    def test_silence_has_no_pieces(self):
        self.assertEqual(chunking.plan(silence(10)), [])
        self.assertEqual(chunking.plan(np.zeros(0, np.float32)), [])

    def test_speech_separated_by_long_pauses_is_split_and_padded(self):
        x = np.concatenate([silence(1), tone(2), silence(3), tone(1.5), silence(1)])
        pieces = chunking.plan(x)
        self.assertEqual(len(pieces), 2)
        (a0, a1), (b0, b1) = (p.secs(RATE) for p in pieces)
        self.assertAlmostEqual(a0, 0.8, delta=0.05)
        self.assertAlmostEqual(a1, 3.2, delta=0.05)
        self.assertAlmostEqual(b0, 5.8, delta=0.05)
        self.assertAlmostEqual(b1, 7.7, delta=0.05)

    def test_short_pauses_stay_in_one_piece(self):
        x = np.concatenate([tone(1), silence(0.3), tone(1)])
        self.assertEqual(len(chunking.plan(x)), 1)

    def test_clicks_shorter_than_min_speech_are_dropped(self):
        x = np.concatenate([silence(2), tone(0.1), silence(2)])
        self.assertEqual(chunking.plan(x), [])

    def test_long_speech_is_cut_below_max_at_the_quietest_frame(self):
        x = np.concatenate([tone(15), tone(0.2, amp=0.02), tone(15)])
        pieces = chunking.plan(x, max_secs=20)
        self.assertEqual(len(pieces), 2)
        cut = pieces[0].end / RATE
        self.assertTrue(15.0 <= cut <= 15.2, cut)
        self.assertEqual(pieces[0].end, pieces[1].start)
        self.assertTrue(all((p.end - p.start) / RATE <= 20 for p in pieces))

    def test_pieces_never_run_past_the_audio(self):
        x = tone(3)
        p, = chunking.plan(x)
        self.assertEqual((p.start, p.end), (0, len(x)))


class MarkdownTest(unittest.TestCase):
    def transcript(self, segments):
        return markdown.Transcript(
            audio_file="2026-10-19_1216_Teams.opus", title="2026-10-19 12:16 · Teams", started="2026-10-19 12:16",
            apps=["Teams"], duration_secs=1043.6, model="underdog-bark-3.0", model_credit="credit",
            device="CPU", rtfx=12.34, segments=segments)

    def test_merge_orders_by_time_and_me_first_on_ties(self):
        me = [markdown.Segment("me", 5, 6, "b"), markdown.Segment("me", 1, 2, "a")]
        others = [markdown.Segment("others", 1, 3, "x")]
        self.assertEqual([(s.speaker, s.text) for s in markdown.merge(me, others)],
                         [("me", "a"), ("others", "x"), ("me", "b")])

    def test_round_trip(self):
        segs = [markdown.Segment("me", 3.7, 5, " Hello   there "), markdown.Segment("others", 3725, 3730, "Hi.")]
        text = markdown.to_markdown(self.transcript(segs))
        self.assertIn("**[00:00:03] Me:** Hello there", text)
        self.assertIn("**[01:02:05] Others:** Hi.", text)
        self.assertIn("- **Duration:** 17:24", text)
        self.assertIn("- **Transcribed with:** underdog-bark-3.0 on CPU, 12.3x real time", text)
        parsed = markdown.parse(text)
        self.assertEqual(parsed["title"], "2026-10-19 12:16 · Teams")
        self.assertEqual(parsed["fields"]["Recording"], "2026-10-19_1216_Teams.opus")
        self.assertEqual(parsed["segments"], [{"speaker": "me", "start": 3, "text": "Hello there"},
                                              {"speaker": "others", "start": 3725, "text": "Hi."}])

    def test_empty_transcript_says_so(self):
        self.assertIn("_No speech detected._", markdown.to_markdown(self.transcript([])))

    def test_write_is_next_to_the_recording(self):
        with tempfile.TemporaryDirectory() as d:
            opus = Path(d) / "a.opus"
            path = markdown.write(str(opus), self.transcript([]))
            self.assertEqual(Path(path), Path(d) / "a.md")
            self.assertFalse((Path(d) / "a.md.tmp").exists())


class RecordingsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        month = self.root / "2026" / "10"
        month.mkdir(parents=True)
        (month / "2026-10-19_1216_Teams.opus").write_bytes(b"x")
        (month / "2026-10-19_1216_Teams.json").write_text(json.dumps(
            {"start": "2026-10-19T12:16:03.123456789+05:30", "apps": ["Teams"], "duration_secs": 61.0}))
        (month / "2026-10-19_1216_Teams.md").write_text("# Transcript")
        (month / "2026-10-20_0900_Manual.opus").write_bytes(b"x")
        (month / "2026-10-21_0900_Zoom.opus.part").write_bytes(b"x")

    def tearDown(self):
        self.tmp.cleanup()

    def test_lists_newest_first_with_transcript_status_and_skips_part_files(self):
        found = recordings.list_recordings(self.root)
        self.assertEqual([r["id"] for r in found],
                         ["2026/10/2026-10-20_0900_Manual.opus", "2026/10/2026-10-19_1216_Teams.opus"])
        manual, teams = found
        self.assertFalse(manual["transcribed"])
        self.assertEqual(manual["app"], "Manual")
        self.assertEqual(manual["started"], "2026-10-20 09:00")
        self.assertTrue(teams["transcribed"])
        self.assertEqual((teams["app"], teams["started"], teams["duration_secs"]), ("Teams", "2026-10-19 12:16", 61.0))

    def test_missing_folder_lists_nothing(self):
        self.assertEqual(recordings.list_recordings(self.root / "nope"), [])

    def test_resolve_rejects_paths_outside_the_folder_and_non_recordings(self):
        self.assertIsNotNone(recordings.resolve(self.root, "2026/10/2026-10-20_0900_Manual.opus"))
        for bad in ["../x.opus", "2026/10/2026-10-19_1216_Teams.json", "2026/10/missing.opus",
                    "2026/10/2026-10-21_0900_Zoom.opus.part"]:
            self.assertIsNone(recordings.resolve(self.root, bad), bad)
        outside = Path(self.tmp.name).parent / "outside.opus"
        self.assertIsNone(recordings.resolve(self.root, str(outside)))


class SettingsAndAudioTest(unittest.TestCase):
    def test_output_dir_comes_from_meetrec_config(self):
        with tempfile.TemporaryDirectory() as d:
            cfg = Path(d) / "config.toml"
            cfg.write_text("output_dir = 'D:\\\\Rec'\n[hotkeys]\npause = \"x\"\n")
            self.assertEqual(settings.meetrec_output_dir(cfg), Path("D:\\Rec"))
            cfg.write_text("not toml [")
            self.assertTrue(str(settings.meetrec_output_dir(cfg)).endswith("MeetRec"))

    def test_settings_file_is_optional_and_sanitized(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(settings.load(Path(d) / "missing.toml"), settings.Settings())
            f = Path(d) / "s.toml"
            f.write_text('model = "nope"\ndevice = "CUDA"\nunknown = 1\nport = 9000\n')
            s = settings.load(f)
            self.assertEqual((s.model, s.device, s.port), ("bark", "cuda", 9000))

    def test_channel_map_to_speakers(self):
        self.assertEqual(audio.speakers_for({"left": "mic", "right": "system"}, 2), ["me", "others"])
        self.assertEqual(audio.speakers_for(None, 2), ["me", "others"])
        self.assertEqual(audio.speakers_for({"mono": "system"}, 1), ["others"])
        self.assertEqual(audio.speakers_for({"mono": "mic"}, 1), ["me"])
        self.assertEqual(audio.speakers_for({}, 1), ["me"])


class FakeEngine:
    name, credit, device_label = "fake", "Fake model", "CPU"

    def transcribe(self, samples, pieces, on_piece=lambda i: None, cancelled=lambda: False):
        out = []
        for i, p in enumerate(pieces):
            out.append(f"piece {p.start // RATE}")
            on_piece(i)
        return out


class FakeEngines:
    def get(self, model):
        return FakeEngine()


class ServerTest(unittest.TestCase):
    def setUp(self):
        from fastapi.testclient import TestClient
        from app.jobs import Jobs
        from app.server import create_app
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.opus = self.root / "2026-10-19_1216_Teams.opus"
        self.opus.write_bytes(b"x")
        self.decoded = {"me": np.concatenate([silence(1), tone(2)]), "others": np.concatenate([silence(4), tone(1)])}
        import app.jobs as jobs_mod
        self._decode = jobs_mod.audio.decode
        jobs_mod.audio.decode = lambda path, channels=None: self.decoded
        s = settings.Settings(recordings_dir=str(self.root))
        self.client = TestClient(create_app(s, Jobs(FakeEngines())))

    def tearDown(self):
        import app.jobs as jobs_mod
        jobs_mod.audio.decode = self._decode
        self.tmp.cleanup()

    def wait_for_job(self):
        for _ in range(200):
            jobs = self.client.get("/api/state").json()["jobs"]
            if jobs and jobs[-1]["state"] not in ("queued", "running"):
                return jobs[-1]
            time.sleep(0.02)
        self.fail("job did not finish")

    def test_transcribe_writes_markdown_that_the_page_can_read(self):
        state = self.client.get("/api/state").json()
        self.assertEqual([r["id"] for r in state["recordings"]], [self.opus.name])
        self.assertFalse(state["recordings"][0]["transcribed"])
        self.assertEqual(self.client.get("/api/transcript", params={"id": self.opus.name}).status_code, 404)

        r = self.client.post("/api/transcribe", json={"id": self.opus.name, "model": "bark"})
        self.assertEqual(r.status_code, 200)
        job = self.wait_for_job()
        self.assertEqual((job["state"], job["progress"]), ("done", 1.0), job)

        t = self.client.get("/api/transcript", params={"id": self.opus.name}).json()
        self.assertEqual(t["segments"], [{"speaker": "me", "start": 0, "text": "piece 0"},
                                         {"speaker": "others", "start": 3, "text": "piece 3"}])
        self.assertEqual(t["fields"]["Transcribed with"].split(",")[0], "fake on CPU")
        self.assertTrue(self.client.get("/api/state").json()["recordings"][0]["transcribed"])

    def test_rejects_unknown_models_paths_and_foreign_hosts(self):
        self.assertEqual(self.client.post("/api/transcribe", json={"id": self.opus.name, "model": "x"}).status_code, 400)
        self.assertEqual(self.client.post("/api/transcribe", json={"id": "../a.opus", "model": "bark"}).status_code, 404)
        self.assertEqual(self.client.get("/api/state", headers={"host": "evil.example:8765"}).status_code, 403)

    def test_engine_failure_is_reported_on_the_job(self):
        class Broken(FakeEngine):
            def transcribe(self, *a, **k):
                raise RuntimeError("boom")
        from app.jobs import Jobs
        from app.server import create_app
        from fastapi.testclient import TestClient
        engines = FakeEngines()
        engines.get = lambda model: Broken()
        client = TestClient(create_app(settings.Settings(recordings_dir=str(self.root)), Jobs(engines)))
        client.post("/api/transcribe", json={"id": self.opus.name, "model": "whistle"})
        self.client = client
        job = self.wait_for_job()
        self.assertEqual((job["state"], job["error"]), ("error", "RuntimeError: boom"))
        self.assertFalse(self.opus.with_suffix(".md").exists())


if __name__ == "__main__":
    unittest.main()
