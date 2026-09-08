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
TARGET_INTS = {
    "changes", "compatible", "core", "current_matches",
    "dimensions_known", "draw_buffer0", "encoding", "extra_draw_buffers",
    "fbo_nonzero", "height", "internal_format", "is_es",
    "major", "minor", "object_type", "observations",
    "query_errors", "samples", "status", "target_observation",
    "texture_target", "viewport_h", "viewport_w", "viewport_x",
    "viewport_y", "width",
}
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


def verify_target(t, summary):
    require(t["target_observation"] == 1 and t["observations"] == summary["frames"] and
            0 <= t["changes"] < t["observations"], "目标观察次数不一致")
    for field in ("current_matches", "is_es", "core", "fbo_nonzero", "dimensions_known", "compatible"):
        require(t[field] in (0, 1), "目标布尔字段无效")
    require(t["current_matches"] == 1 and t["query_errors"] == 0, "目标上下文或查询失败")
    require(3 <= t["major"] <= 4 and 0 <= t["minor"] <= 6 and not (t["is_es"] and t["core"]), "目标GL版本无效")
    require(t["status"] == 0x8CD5 and t["samples"] <= 64 and t["extra_draw_buffers"] <= 32, "目标FBO状态无效")
    require(t["draw_buffer0"] in (0, 0x0405, 0x8CE0), "未知draw buffer")
    require(t["object_type"] in (0, 0x1702, 0x8D41), "未知附件类型")
    require(t["encoding"] in (0, 0x2601, 0x8C40), "未知颜色编码")
    require(t["texture_target"] in (0, 0x0DE0, 0x0DE1, 0x806F, 0x8513, 0x84F5, 0x8C18, 0x8C1A, 0x9009, 0x9100, 0x9102), "未知纹理target")
    require(t["viewport_x"] == 0 and t["viewport_y"] == 0 and
            t["viewport_w"] == summary["viewport_w"] and t["viewport_h"] == summary["viewport_h"], "目标viewport不一致")
    if t["object_type"] == 0:
        require(not t["dimensions_known"] and t["encoding"] == 0 and t["texture_target"] == 0, "空附件数据矛盾")
    else:
        require(t["fbo_nonzero"] == 1 and t["encoding"] != 0, "附件缺少FBO或编码")
    if t["object_type"] == 0x8D41:
        require(t["dimensions_known"] == 1 and t["texture_target"] == 0, "renderbuffer数据矛盾")
    if t["texture_target"]:
        require(t["object_type"] == 0x1702 and not t["is_es"] and (t["major"],t["minor"]) >= (4,5), "纹理target缺少安全DSA查询支持")
    if t["object_type"] == 0x1702:
        dsa = not t["is_es"] and (t["major"],t["minor"]) >= (4,5)
        require(bool(t["texture_target"]) == dsa, "纹理target查询哨兵与GL版本矛盾")
        require(t["dimensions_known"] == int(t["texture_target"] == 0x0DE1), "纹理尺寸查询与target矛盾")
    if t["dimensions_known"]:
        require(0 < t["width"] <= 65536 and 0 < t["height"] <= 65536 and
                t["internal_format"] in (0x8051,0x8058,0x8059,0x8C41,0x8C43,0x881A,0x8814), "目标尺寸或格式未知")
        require((t["internal_format"] in (0x8C41,0x8C43)) == (t["encoding"] == 0x8C40), "格式与颜色编码矛盾")
    else:
        require(t["width"] == t["height"] == t["internal_format"] == 0, "未查询尺寸必须使用零哨兵")
    compatible = (not t["is_es"] and t["core"] and (t["major"],t["minor"]) >= (3,3) and
                  t["fbo_nonzero"] and t["draw_buffer0"] == 0x8CE0 and not t["extra_draw_buffers"] and
                  t["object_type"] == 0x1702 and t["encoding"] == 0x8C40 and t["dimensions_known"] and
                  t["texture_target"] == 0x0DE1 and not t["samples"] and
                  t["width"] == t["viewport_w"] and t["height"] == t["viewport_h"])
    require(t["compatible"] == int(bool(compatible)), "目标兼容判定伪造或不一致")


def verify_text(text, expected_backend, expected_scale, require_input=False):
    require(expected_backend in ("x11", "wayland"), "不支持的预期 backend")
    require(type(expected_scale) is int and 1 <= expected_scale <= 8, "预期 scale 必须为 1..8")
    require(not require_input or expected_backend == "x11", "Wayland 输入验证尚未实现")
    require(len(text.encode("utf-8")) <= 65536, "报告超过大小限制")
    lines = text.splitlines()
    require(1 <= len(lines) <= 16 and all(0 < len(line) <= 8192 for line in lines), "报告行数或长度无效")
    summary = detail = target = None
    gl_records = set()
    for line in lines:
        record = json.loads(line, object_pairs_hook=unique_object, parse_constant=reject_constant)
        require(type(record) is dict, "报告行必须为 JSON 对象")
        if "summary" in record:
            require(summary is None, "必须只有一条 summary")
            numeric_record(record, SUMMARY_INTS, SUMMARY_FLOATS)
            summary = record
        elif "target_observation" in record:
            require(target is None, "必须只有一条 target_observation")
            numeric_record(record, TARGET_INTS, set())
            target = record
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
    require(summary is not None and detail is not None and target is not None and gl_records == GL_KEYS, "报告不完整")
    verify_target(target, summary)
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
    # 固定 CI GTK 主题的边界盒可向外扩 1 逻辑点；不是任意主题的产品保证。
    # 内容保持窗口左边对齐，标题栏必须左右对称覆盖它，不能借负坐标掩盖偏移。
    require(summary["content_x"] == 0 and summary["content_y"] >= 0,
            "内容原点必须非负且与窗口左边对齐")
    require(-1 <= summary["header_y"] <= 0, "标题栏顶部外扩超出固定探针边界")
    left_outset = summary["content_x"] - summary["header_x"]
    right_outset = (summary["header_x"] + summary["header_w"] -
                    summary["content_x"] - summary["content_w"])
    require(0 <= left_outset <= 1 and 0 <= right_outset <= 1 and
            abs(left_outset - right_outset) <= 0.02,
            "标题栏必须在一逻辑点内左右对称覆盖内容")
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
