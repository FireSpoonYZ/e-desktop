"""Pure offline tests. No HWND, screen capture, new dependencies, or fixture launch."""
import importlib.util
from pathlib import Path
import struct
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("handoff_analysis", Path(__file__).with_name("analyze-handoff.py"))
analysis = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = analysis
SPEC.loader.exec_module(analysis)


def synthetic(width=160, height=128, generation=7, role="C", corner_generations=None):
    # Independent encoder, not the analyzer's decoder. Test interior changes too.
    raw = bytearray((40, 30, 20, 255) * (width * height))
    for corner in range(4):
        g = corner_generations[corner] if corner_generations else generation
        payload = bytearray(struct.pack("<IHHB", g, width, height, ord(role)))
        checksum = 0xA7
        for byte in payload:
            checksum ^= byte
        payload.append(checksum)
        ox = 8 if corner % 2 == 0 else width-56
        oy = 8 if corner < 2 else height-48
        for row in range(10):
            for col in range(12):
                if row in (0, 9) or col in (0, 11):
                    color = analysis.BORDERS[corner]
                else:
                    bit = (row-1)*10+col-1
                    color = (240, 240, 240) if payload[bit//8] & (1 << (bit % 8)) else (16, 16, 16)
                r, green, b = color
                for y in range(oy+row*4, oy+(row+1)*4):
                    for x in range(ox+col*4, ox+(col+1)*4):
                        i = (y*width+x)*4
                        raw[i:i+4] = bytes((b, green, r, 255))
    return analysis.Pixels(width, height, bytes(raw))


def manifest(width=160, height=128, generation=7):
    def rect(x, y, w, h):
        return dict(x=x, y=y, width=w, height=h)
    return {
        "schemaVersion": 1, "sampling": "nearest",
        "expected": dict(generation=generation, width=width, height=height, role="C"),
        "sourceRect": rect(-200, 80, width, height), "clientRect": rect(-200, 80, width, height),
        "destinationRect": rect(0, 0, width, height), "clipRect": rect(0, 0, width, height)
    }


def altered(pixels, x, y, color):
    raw = bytearray(pixels.bgra)
    r, g, b = color
    i = (y*pixels.width+x)*4
    raw[i:i+4] = bytes((b, g, r, 255))
    return analysis.Pixels(pixels.width, pixels.height, bytes(raw))


def bmp_bytes(pixels, bits=32, top_down=False):
    stride = ((pixels.width*bits+31)//32)*4
    rows = bytearray()
    ys = range(pixels.height) if top_down else range(pixels.height-1, -1, -1)
    for y in ys:
        row = bytearray()
        for x in range(pixels.width):
            i = (y*pixels.width+x)*4
            row.extend(pixels.bgra[i:i+bits//8])
        row.extend(bytes(stride-len(row)))
        rows.extend(row)
    header = b"BM" + struct.pack("<IHHI", 54+len(rows), 0, 0, 54)
    dib = struct.pack("<IiiHHIIiiII", 40, pixels.width,
                      -pixels.height if top_down else pixels.height,
                      1, bits, 0, len(rows), 0, 0, 0, 0)
    return header+dib+rows


class MarkerAnalysisTests(unittest.TestCase):
    def setUp(self):
        self.reference = synthetic()
        self.record = manifest()

    def assess(self, frame=None, reference=True):
        return analysis.analyze(self.record, frame or self.reference, self.reference if reference else None)

    def test_complete_client_is_pixel_pass_not_semantic_ready(self):
        result = self.assess()
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["comparedPixels"], 160*128)
        self.assertIsNone(result["semanticReady"])
        self.assertEqual(len(result["markers"]), 4)

    def test_marker_agreement_without_reference_is_unknown(self):
        result = self.assess(reference=False)
        self.assertEqual(result["status"], "unknown")
        self.assertTrue(result["markerConsistent"])

    def test_four_consistent_stale_generations_are_mismatch(self):
        self.assertEqual(self.assess(synthetic(generation=6))["status"], "mismatch")

    def test_mixed_generations_are_mismatch(self):
        self.assertEqual(self.assess(synthetic(corner_generations=[7, 7, 6, 7]))["status"], "mismatch")

    def test_wrong_role_is_mismatch(self):
        self.assertEqual(self.assess(synthetic(role="D"))["status"], "mismatch")

    def test_interior_corruption_is_not_pass_even_with_all_markers(self):
        result = self.assess(altered(self.reference, 80, 64, (99, 99, 99)))
        self.assertEqual(result["status"], "unknown")
        self.assertTrue(result["markerConsistent"])

    def test_compressed_color_is_unknown(self):
        self.assertEqual(self.assess(altered(self.reference, 10, 10, (0, 207, 208)))["status"], "unknown")

    def test_occluded_corner_is_unknown(self):
        self.assertEqual(self.assess(altered(self.reference, 10, 10, (0, 0, 0)))["status"], "unknown")

    def test_bad_checksum_is_unknown(self):
        # Change the sampled first payload bit to the opposite valid color.
        x, y = 14, 14
        current = self.reference.rgb(x, y)
        color = analysis.ZERO if current == analysis.ONE else analysis.ONE
        self.assertEqual(self.assess(altered(self.reference, x, y, color))["status"], "unknown")

    def test_clipped_client_is_unknown(self):
        self.record["clipRect"]["width"] -= 1
        self.assertEqual(self.assess()["status"], "unknown")

    def test_missing_geometry_is_unknown(self):
        del self.record["clientRect"]
        self.assertEqual(self.assess()["status"], "unknown")

    def test_unsupported_filter_is_unknown(self):
        self.record["sampling"] = "linear"
        self.assertEqual(self.assess()["status"], "unknown")

    def test_nonfinite_geometry_is_unknown(self):
        self.record["sourceRect"]["x"] = float("nan")
        self.assertEqual(self.assess()["status"], "unknown")

    def test_wrong_client_size_is_unknown(self):
        self.record["clientRect"]["width"] -= 1
        self.assertEqual(self.assess()["status"], "unknown")

    def test_wrong_reference_generation_is_unknown(self):
        result = analysis.analyze(self.record, self.reference, synthetic(generation=8))
        self.assertEqual(result["status"], "unknown")

    def test_outer_client_offset_and_2x_proxy_transform(self):
        # Source outer has nonclient borders. Destination is translated and scaled.
        width, height = 360, 300
        raw = bytearray(width*height*4)
        for y in range(128*2):
            for x in range(160*2):
                src = ((y//2)*160+x//2)*4
                dst = ((y+26)*width+x+16)*4
                raw[dst:dst+4] = self.reference.bgra[src:src+4]
        self.record["sourceRect"] = dict(x=-204, y=70, width=168, height=144)
        self.record["destinationRect"] = dict(x=8, y=6, width=336, height=288)
        self.record["clipRect"] = dict(x=0, y=0, width=width, height=height)
        result = self.assess(analysis.Pixels(width, height, bytes(raw)))
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["comparedPixels"], 160*128*4)

    def test_half_scale_nearest_is_supported(self):
        raw = bytearray()
        for y in range(64):
            for x in range(80):
                i = ((y*2+1)*160+x*2+1)*4
                raw.extend(self.reference.bgra[i:i+4])
        self.record["destinationRect"] = dict(x=0, y=0, width=80, height=64)
        self.record["clipRect"] = dict(x=0, y=0, width=80, height=64)
        self.assertEqual(self.assess(analysis.Pixels(80, 64, bytes(raw)))["status"], "pass")

    def test_undersampled_markers_are_unknown(self):
        self.record["destinationRect"]["width"] = 40
        self.assertEqual(self.assess()["status"], "unknown")

    def test_invalid_expected_generation_is_unknown(self):
        self.record["expected"]["generation"] = 0
        self.assertEqual(self.assess()["status"], "unknown")

    def test_malformed_root_and_bool_schema_are_unknown(self):
        self.assertEqual(analysis.analyze(None, self.reference)["status"], "unknown")
        self.record["schemaVersion"] = True
        self.assertEqual(self.assess()["status"], "unknown")

    def test_bmp_24_and_32_top_down_and_bottom_up(self):
        image = analysis.Pixels(3, 2, bytes((3, 2, 1, 255)*6))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/"offline.bmp"
            for bits in (24, 32):
                for top_down in (False, True):
                    path.write_bytes(bmp_bytes(image, bits, top_down))
                    self.assertEqual(analysis.load_bmp(path), image)

    def test_truncated_and_compressed_bmp_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/"offline.bmp"
            data = bmp_bytes(self.reference)
            path.write_bytes(data[:-1])
            with self.assertRaises(ValueError):
                analysis.load_bmp(path)
            compressed = bytearray(data)
            struct.pack_into("<I", compressed, 30, 1)
            path.write_bytes(compressed)
            with self.assertRaises(ValueError):
                analysis.load_bmp(path)

    def test_bgra_length_validation(self):
        with self.assertRaises(ValueError):
            analysis.Pixels(1, 1, bytes(3))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/"offline.bgra"
            path.write_bytes(self.reference.bgra)
            decoded = analysis.load_pixels(dict(path=str(path), format="bgra", width=160, height=128))
            self.assertEqual(decoded, self.reference)

    def test_missing_file_or_nonfile_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                analysis.load_pixels(dict(path=directory, format="video", width=160, height=128))
            with self.assertRaises(ValueError):
                analysis.load_pixels(dict(path=str(Path(directory)/"missing.bmp"), format="bmp"))


class StateObservationTests(unittest.TestCase):
    def state(self):
        return dict(schemaVersion=1, clock="QueryPerformanceCounter", clockUnit="ticks",
                    qpcTicks=100, qpcFrequency=10000000, desiredGeneration=7, paintedGeneration=7,
                    desiredClientSize=dict(width=160, height=128),
                    paintedClientSize=dict(width=160, height=128), pending=False)

    def test_published_is_not_semantic_ready(self):
        result = analysis.assess_fixture_state(self.state())
        self.assertEqual(result["status"], "bitmap-published")
        self.assertIsNone(result["semanticReady"])

    def test_delayed_state_is_pending(self):
        state = self.state()
        state.update(desiredGeneration=8, pending=True)
        state["desiredClientSize"]["width"] = 240
        self.assertEqual(analysis.assess_fixture_state(state)["status"], "pending")

    def test_inconsistent_state_is_unknown(self):
        state = self.state()
        state["pending"] = True
        self.assertEqual(analysis.assess_fixture_state(state)["status"], "unknown")

    def test_bad_clock_missing_state_and_future_generation_are_unknown(self):
        state = self.state()
        state["paintedGeneration"] = 8
        self.assertEqual(analysis.assess_fixture_state(state)["status"], "unknown")
        state = self.state()
        state["clockUnit"] = "ms"
        self.assertEqual(analysis.assess_fixture_state(state)["status"], "unknown")
        self.assertEqual(analysis.assess_fixture_state({})["status"], "unknown")

    def test_state_does_not_turn_missing_visual_reference_into_pass(self):
        record = manifest()
        record["fixtureState"] = self.state()
        result = analysis.analyze(record, synthetic())
        self.assertEqual(result["stateObservation"]["status"], "bitmap-published")
        self.assertEqual(result["status"], "unknown")


if __name__ == "__main__":
    unittest.main()
