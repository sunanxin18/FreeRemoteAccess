"""合成报告拒绝路径；不运行窗口，不产生 X11/Wayland 输入证明。"""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("report_verifier", Path(__file__).with_name("verify_report.py"))
v = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v)


def fixture(scale=1, backend=1, with_input=False):
    summary = {name: 0 for name in v.SUMMARY_INTS | v.SUMMARY_FLOATS}
    summary.update(summary=1, passed=1, timed=1, backend=backend, mapped=1, frames=29, scale=scale,
                   header_w=960, header_h=56, content_y=56, content_w=960, content_h=640,
                   separated=1, viewport_w=960 * scale, viewport_h=640 * scale)
    detail = dict(geometry_detail=1, resizes=1, content_focus=0, window_active=0,
                  pointer_pixel_x=0, pointer_pixel_y=0, hardware_verified=0)
    if with_input:
        summary.update(motions=1, presses=1, releases=1, key_presses=1, key_releases=1,
                       focus_enters=1, pointer_x=480, pointer_y=400)
        detail.update(content_focus=1, window_active=1, pointer_pixel_x=480 * scale,
                      pointer_pixel_y=400 * scale)
    return [{"gl_vendor": "Mesa"}, {"gl_renderer": "llvmpipe (LLVM)"},
            {"gl_version": "4.5 (Core Profile) Mesa"}, summary, detail]


def observed_border_fixture(scale=1):
    """复刻 a60 x64 X11 scale1 报告数字；scale2 只是比例合成 fixture。"""
    rows = fixture(scale, with_input=True)
    rows[1]["gl_renderer"] = "llvmpipe (LLVM 20.1.2, 256 bits)"
    rows[2]["gl_version"] = "4.5 (Core Profile) Mesa 25.2.8-0ubuntu0.24.04.2"
    rows[-2].update(frames=89, header_x=-1, header_y=-1, header_w=952, header_h=47,
                    content_x=0, content_y=46, content_w=950, content_h=584,
                    viewport_w=950 * scale, viewport_h=584 * scale,
                    pointer_x=475, pointer_y=429)
    rows[-1].update(pointer_pixel_x=475 * scale, pointer_pixel_y=429 * scale)
    return rows


def text(records):
    return "\n".join(json.dumps(row) for row in records) + "\n"


class ReportValidation(unittest.TestCase):
    def test_valid_native_geometry_scales_and_backends(self):
        for scale in (1, 2):
            for backend, name in ((1, "x11"), (2, "wayland")):
                v.verify_text(text(fixture(scale, backend)), name, scale)

    def test_valid_x11_input_scales(self):
        for scale in (1, 2):
            v.verify_text(text(fixture(scale, with_input=True)), "x11", scale, True)

    def test_observed_gtk_border_bounds_with_input(self):
        for scale in (1, 2):
            rows = observed_border_fixture(scale)
            v.verify_text(text(rows), "x11", scale, True)

    def test_header_border_and_content_alignment_rejected(self):
        mutations = ({"header_x": -2}, {"header_y": -2},
                     {"header_x": 1}, {"header_y": 1},
                     {"header_w": 951}, {"header_w": 953},
                     {"content_x": -1}, {"content_y": -1},
                     {"content_x": 1},
                     {"header_x": 0, "content_x": 1},
                     {"header_y": 4, "content_y": 51})
        for mutation in mutations:
            rows = observed_border_fixture()
            rows[-2].update(mutation)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                v.verify_text(text(rows), "x11", 1, True)

    def test_wayland_input_claim_rejected(self):
        with self.assertRaisesRegex(ValueError, "Wayland 输入"):
            v.verify_text(text(fixture(backend=2, with_input=True)), "wayland", 1, True)

    def test_duplicate_summary_rejected(self):
        rows = fixture()
        rows.append(rows[-2].copy())
        with self.assertRaisesRegex(ValueError, "只有一条 summary"):
            v.verify_text(text(rows), "x11", 1)

    def test_empty_or_incomplete_report_rejected(self):
        for rows in ([], fixture()[:-1], fixture()[1:]):
            with self.assertRaises(ValueError):
                v.verify_text(text(rows), "x11", 1)

    def test_backend_and_scale_revalidated(self):
        for backend, scale in (("wayland", 1), ("x11", 2)):
            with self.assertRaises(ValueError):
                v.verify_text(text(fixture()), backend, scale)

    def test_claimed_pass_does_not_hide_invalid_geometry(self):
        mutations = {"frames": 0, "errors": 1, "mapped": 0, "timed": 0,
                     "content_h": 0, "viewport_w": 1920, "center_error": 2,
                     "content_y": 0, "header_h": 10, "scale": True,
                     "key_presses": "1"}
        for field, value in mutations.items():
            rows = fixture()
            rows[-2][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                v.verify_text(text(rows), "x11", 1)

    def test_numeric_data_rejects_nan_and_infinity(self):
        for value in (float("nan"), float("inf"), -float("inf")):
            rows = fixture()
            rows[-2]["pointer_x"] = value
            with self.assertRaises(ValueError):
                v.verify_text(text(rows), "x11", 1)

    def test_duplicate_json_key_rejected(self):
        payload = text(fixture()).replace('"passed": 1', '"passed": 0, "passed": 1')
        with self.assertRaisesRegex(ValueError, "重复 JSON"):
            v.verify_text(payload, "x11", 1)

    def test_pointer_scale_mismatch_rejected(self):
        rows = fixture(scale=2, with_input=True)
        rows[-1]["pointer_pixel_x"] = 480
        with self.assertRaisesRegex(ValueError, "坐标比例"):
            v.verify_text(text(rows), "x11", 2, True)

    def test_no_input_cannot_pass_input_gate(self):
        with self.assertRaisesRegex(ValueError, "真实 X11"):
            v.verify_text(text(fixture()), "x11", 1, True)

    def test_incomplete_or_wrong_focus_input_rejected(self):
        for destination, field, value in ((-2, "releases", 0), (-2, "key_releases", 2),
                                          (-2, "header_actions", 1), (-1, "window_active", 0),
                                          (-1, "content_focus", 0)):
            rows = fixture(with_input=True)
            rows[destination][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                v.verify_text(text(rows), "x11", 1, True)

    def test_hardware_claim_rejected(self):
        rows = fixture()
        rows[-1]["hardware_verified"] = 1
        with self.assertRaisesRegex(ValueError, "硬件证明"):
            v.verify_text(text(rows), "x11", 1)

    def test_unknown_text_error_and_oversize_rejected(self):
        for extra in ('{"typed_text":"invalid"}', '{"error_code": 5}', 'x' * 8193):
            with self.assertRaises(ValueError):
                v.verify_text(text(fixture()) + extra, "x11", 1)


if __name__ == "__main__":
    unittest.main()
