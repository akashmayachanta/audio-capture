"""Tests for the cache-folder handling in transcribe.py (no model, no hardware).
Run from this folder: python -m unittest discover"""
import json
import os
import tempfile
import unittest
import wave

import numpy as np

import engine as eng
import transcribe as t


def write_model(model_dir, sizes=None):
    os.makedirs(model_dir, exist_ok=True)
    for i, name in enumerate(eng.REQUIRED_FILES):
        with open(os.path.join(model_dir, name), "wb") as f:
            f.write(b"x" * (10 + i))
    t.write_json(os.path.join(model_dir, "manifest.json"), {"files": sizes or t.file_sizes(model_dir)})


class ModelIntegrity(unittest.TestCase):
    def test_complete_model_with_matching_manifest_is_intact(self):
        with tempfile.TemporaryDirectory() as d:
            write_model(d)
            self.assertTrue(t.model_is_intact(d))

    def test_missing_file_is_not_intact(self):
        with tempfile.TemporaryDirectory() as d:
            write_model(d)
            os.remove(os.path.join(d, "parakeet_encoder.bin"))
            self.assertFalse(t.model_is_intact(d))

    def test_truncated_file_is_not_intact(self):
        with tempfile.TemporaryDirectory() as d:
            write_model(d)
            with open(os.path.join(d, "parakeet_encoder.bin"), "ab") as f:
                f.write(b"junk")
            self.assertFalse(t.model_is_intact(d))

    def test_no_manifest_means_not_intact(self):
        with tempfile.TemporaryDirectory() as d:
            write_model(d)
            os.remove(os.path.join(d, "manifest.json"))
            self.assertFalse(t.model_is_intact(d))

    def test_empty_folder_is_not_intact(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertFalse(t.model_is_intact(d))


class OvCache(unittest.TestCase):
    def setUp(self):
        try:
            import openvino  # noqa: F401
        except ImportError:
            self.skipTest("openvino not installed")

    def test_blobs_survive_when_the_runtime_is_unchanged(self):
        with tempfile.TemporaryDirectory() as d:
            cache = os.path.join(d, "ov_cache")
            t.prepare_ov_cache(cache)
            open(os.path.join(cache, "blob"), "w").close()
            t.prepare_ov_cache(cache)
            self.assertTrue(os.path.exists(os.path.join(cache, "blob")))

    def test_blobs_are_wiped_when_the_driver_or_openvino_changes(self):
        with tempfile.TemporaryDirectory() as d:
            cache = os.path.join(d, "ov_cache")
            t.prepare_ov_cache(cache)
            open(os.path.join(cache, "blob"), "w").close()
            manifest = os.path.join(cache, "cache_manifest.json")
            fp = t.read_json(manifest)
            fp["npu_driver"] = "0"
            t.write_json(manifest, fp)
            t.prepare_ov_cache(cache)
            self.assertFalse(os.path.exists(os.path.join(cache, "blob")))
            self.assertNotEqual(t.read_json(manifest).get("npu_driver"), "0")

    def test_a_cache_without_a_manifest_is_wiped(self):
        with tempfile.TemporaryDirectory() as d:
            cache = os.path.join(d, "ov_cache")
            os.makedirs(cache)
            open(os.path.join(cache, "blob"), "w").close()
            t.prepare_ov_cache(cache)
            self.assertFalse(os.path.exists(os.path.join(cache, "blob")))
            self.assertTrue(os.path.exists(os.path.join(cache, "cache_manifest.json")))


class Wav(unittest.TestCase):
    def write(self, path, rate=16000, channels=1, samples=None):
        samples = np.array([0, 16384, -16384, 32767] if samples is None else samples, dtype="<i2")
        with wave.open(path, "wb") as w:
            w.setnchannels(channels)
            w.setsampwidth(2)
            w.setframerate(rate)
            w.writeframes(samples.tobytes())

    def test_reads_16k_mono_as_unit_floats(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "a.wav")
            self.write(p)
            out = t.read_wav(p)
            self.assertEqual(out.dtype, np.float32)
            np.testing.assert_allclose(out, [0, 0.5, -0.5, 32767 / 32768], atol=1e-6)

    def test_rejects_other_formats(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "a.wav")
            self.write(p, rate=48000)
            with self.assertRaises(ValueError):
                t.read_wav(p)
            self.write(p, channels=2, samples=[0, 0, 1, 1])
            with self.assertRaises(ValueError):
                t.read_wav(p)


class Plan(unittest.TestCase):
    def test_auto_tries_the_npu_first_for_every_part_except_mel(self):
        plan = eng.device_plan("auto")
        self.assertEqual(plan["mel"], "CPU")
        for part in ("encoder", "decoder", "joint"):
            self.assertTrue(plan[part].startswith("NPU"), plan)

    def test_a_named_device_applies_to_all_parts_but_mel(self):
        plan = eng.device_plan("gpu")
        self.assertEqual((plan["encoder"], plan["decoder"], plan["joint"], plan["mel"]), ("GPU",) * 3 + ("CPU",))

    def test_overrides_replace_single_parts(self):
        plan = t.parse_plan("auto", "joint=cpu; decoder=GPU")
        self.assertEqual((plan["joint"], plan["decoder"]), ("CPU", "GPU"))
        with self.assertRaises(ValueError):
            t.parse_plan("auto", "nope=CPU")


class Output(unittest.TestCase):
    def test_emit_writes_one_json_line(self):
        import io
        import contextlib
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            t.emit({"progress": 0.5, "text": "café"})
        line = buf.getvalue()
        self.assertTrue(line.endswith("\n") and line.count("\n") == 1)
        self.assertEqual(json.loads(line), {"progress": 0.5, "text": "café"})


if __name__ == "__main__":
    unittest.main()
