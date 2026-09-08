#!/usr/bin/env python3
"""验证 native-shell 数字报告；通过仅覆盖明确请求的 GL/几何或 X11 输入范围。"""
import argparse
import json
import math
from pathlib import Path
import sys

COUNTS = {
    "frames", "errors", "motions", "presses", "releases", "key_presses", "key_releases",
    "focus_enters", "focus_leaves", "header_actions",
}
SUMMARY_INTS = COUNTS | {
    "summary", "passed", "timed", "backend", "mapped", "scale", "separated", "viewport_w", "viewport_h",
}
SUMMARY_FLOATS = {
    "header_x", "header_y", "header_w", "header_h", "content_x", "content_y", "content_w", "content_h",
    "center_error", "pointer_x", "pointer_y",
}
DETAIL_INTS = {"geometry_detail", "resizes", "content_focus", "window_active", "hardware_verified"}
DETAIL_FLOATS = {"pointer_pixel_x", "pointer_pixel_y"}
GL_KEYS = {"gl_vendor", "gl_renderer", "gl_version"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "报告含重复 JSON 字段")
        result[key] = value
    return result


def reject_constant(_value):
    raise ValueError("报告包含非有限 JSON 数字")


def numeric_record(record, integers, decimals):
    require(set(record) == integers | decimals, "数字报告字段集合不匹配")
    for name in integers:
        value = record[name]
        require(type(value) is int and 0 <= value < 2**64, f"字段 {name} 必须为非负整数")
    for name in decimals:
        value = record[name]
        require(type(value) in (int, float) and math.isfinite(value) and abs(value) <= 2**32,
                f"字段 {name} 必须为有限数字")


def verify_text(text, expected_backend, expected_scale, require_input=False):
    require(expected_backend in ("x11", "wayland"), "不支持的预期 backend")
    require(type(expected_scale) is int and 1 <= expected_scale <= 8, "预期 scale 必须为 1..8")
    require(not require_input or expected_backend == "x11", "Wayland 输入验证尚未实现")
    require(len(text.encode("utf-8")) <= 65536, "报告超过大小限制")
    lines = text.splitlines()
    require(1 <= len(lines) <= 16 and all(0 < len(line) <= 8192 for line in lines), "报告行数或长度无效")
    summary = detail = None
    gl_records = set()
    for line in lines:
        record = json.loads(line, object_pairs_hook=unique_object, parse_constant=reject_constant)
        require(type(record) is dict, "报告行必须为 JSON 对象")
        if "summary" in record:
            require(summary is None, "必须只有一条 summary")
            numeric_record(record, SUMMARY_INTS, SUMMARY_FLOATS)
            summary = record
        elif "geometry_detail" in record:
            require(detail is None, "必须只有一条 geometry_detail")
            numeric_record(record, DETAIL_INTS, DETAIL_FLOATS)
            detail = record
        elif len(record) == 1 and next(iter(record)) in GL_KEYS:
            name, value = next(iter(record.items()))
            require(name not in gl_records, "GL 信息重复")
            require(type(value) is str and 0 < len(value) <= 256 and
                    all(32 <= ord(c) <= 126 and c not in '\\"' for c in value), "GL 信息不是有界清理字符串")
            gl_records.add(name)
        else:
            raise ValueError("报告包含错误或未知记录")
    require(summary is not None and detail is not None and gl_records == GL_KEYS, "报告不完整")
    for name in ("summary", "passed", "timed", "mapped", "separated"):
        require(summary[name] == 1, f"字段 {name} 未通过")
    require(detail["geometry_detail"] == 1 and detail["hardware_verified"] == 0,
            "报告不得将 GL pipeline 夸大为硬件证明")
    for name in ("content_focus", "window_active"):
        require(detail[name] in (0, 1), "焦点状态无效")
    require(summary["backend"] == {"x11": 1, "wayland": 2}[expected_backend], "实际 backend 不匹配")
    require(summary["scale"] == expected_scale, "实际 scale 不匹配")
    require(summary["frames"] > 0 and summary["errors"] == 0 and detail["resizes"] > 0,
            "缺少成功 GL 绘制或分配证据")
    for name in ("header_w", "header_h", "content_w", "content_h", "viewport_w", "viewport_h"):
        require(summary[name] > 0, "分配或 viewport 为空")
    for name in ("header_x", "header_y", "content_x", "content_y"):
        require(summary[name] >= 0, "分配坐标不能为负")
    require(0 <= summary["center_error"] <= 1, "中心控件偏离窗口中心")
    gap = summary["content_y"] - summary["header_y"] - summary["header_h"]
    require(-1 <= gap <= 2, "标题栏与内容重叠或存在额外空白工具条")
    for axis in ("w", "h"):
        require(abs(summary[f"viewport_{axis}"] - summary[f"content_{axis}"] * expected_scale) <= 0.05,
                "GL viewport 与内容分配/DPI 不一致")
    for axis in ("x", "y"):
        logical = summary[f"pointer_{axis}"]
        pixel = detail[f"pointer_pixel_{axis}"]
        require(abs(pixel - logical * expected_scale) <= 0.05, "指针与 GL viewport 坐标比例不一致")
    if require_input:
        for name in ("motions", "presses", "releases", "key_presses", "key_releases", "focus_enters"):
            require(summary[name] > 0, f"没有真实 X11 {name} 事件证据")
        require(summary["presses"] == summary["releases"] and
                summary["key_presses"] == summary["key_releases"], "输入按下与释放未配对")
        require(detail["content_focus"] == 1 and detail["window_active"] == 1, "实际内容输入焦点不成立")
        require(summary["header_actions"] == 0, "输入测试误命中了标题栏控件")
        require(0 <= summary["pointer_x"] < summary["content_w"] and
                0 <= summary["pointer_y"] < summary["content_h"], "输入未落在实际内容区域")
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--expect-backend", choices=("x11", "wayland"), required=True)
    parser.add_argument("--expect-scale", type=int, required=True)
    parser.add_argument("--require-input", action="store_true")
    args = parser.parse_args()
    try:
        with args.report.open("rb") as stream:
            content = stream.read(65537)
        require(len(content) <= 65536, "报告超过大小限制")
        verify_text(content.decode("utf-8"), args.expect_backend, args.expect_scale, args.require_input)
    except (ValueError, OSError, UnicodeError, OverflowError) as error:
        print(f"原生窗口报告验证失败：{error}", file=sys.stderr)
        return 1
    scope = "X11 内容区真实输入及 GL/几何" if args.require_input else "GL/几何（未验证输入）"
    print(f"原生窗口报告验证通过：{args.expect_backend} scale={args.expect_scale}；{scope}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
