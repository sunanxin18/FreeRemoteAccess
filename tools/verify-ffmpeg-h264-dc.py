#!/usr/bin/env python3
"""Build a private fixture against existing FFmpeg objects; never build/package FFmpeg.

Run only with an explicit native target and --execute. Cross compilation is
compile-only. The pristine initializer is compiled privately with a renamed
symbol, allowing every selected slot to be compared to the original dispatcher.
No network, device, signing, production export, or source modification occurs.
"""
import argparse
import hashlib
import json
import platform
from pathlib import Path
import shlex
import struct
import subprocess
import sys

PINS = {
    "libavcodec/h264idct_template.c": "13b6d5b4fc41f4c22350c84209970af86aac7d445e867b362a8808f076487735",
    "libavcodec/aarch64/h264idct_neon.S": "963ffe5f31b5a6a422e13b0d394cf5630126927abfb23aa214f7cbe83d60683f",
    "libavcodec/aarch64/h264dsp_init_aarch64.c": "f8f12b10c797ad875ccbf2481447e2d5a2b6b7c6de70e8dc92da7cfc0de80cb0",
}
TARGETS = {"aarch64-apple-darwin": "Darwin", "aarch64-linux-gnu": "Linux",
           "aarch64-linux-ohos": None}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def binary_target(path, target):
    header = path.read_bytes()[:64]
    if TARGETS[target] == "Darwin":
        if header[:4] != b"\xcf\xfa\xed\xfe" or struct.unpack_from("<I", header, 4)[0] != 0x100000C:
            raise ValueError("fixture is not a thin ARM64 Mach-O")
    elif header[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", header, 18)[0] != 183:
        raise ValueError("fixture is not an AArch64 little-endian ELF64")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--source", type=Path, required=True, help="configured tree whose prebuilt objects are tested")
    p.add_argument("--upstream-source", type=Path, required=True, help="pristine pinned extraction, read-only")
    p.add_argument("--output", type=Path, required=True, help="new private result directory (must not exist)")
    p.add_argument("--target", required=True, choices=TARGETS)
    p.add_argument("--cc", default="clang", help="compiler command, parsed with shlex")
    p.add_argument("--cflag", action="append", default=[])
    p.add_argument("--ldflag", action="append", default=[])
    p.add_argument("--object", type=Path, action="append", default=[], help="existing object/private archive; defaults to two static libraries")
    p.add_argument("--execute", action="store_true", help="explicit native execution; unavailable for OHOS")
    p.add_argument("--benchmark", action="store_true", help="separate timing mode, requires --execute")
    args = p.parse_args()
    if args.benchmark and not args.execute:
        p.error("--benchmark requires --execute")
    if args.execute and (TARGETS[args.target] is None or platform.system() != TARGETS[args.target]
                         or platform.machine().lower() not in ("arm64", "aarch64")):
        p.error("execution requires this exact native ARM64 host target; cross-target execution is forbidden")
    src, original, out = args.source.resolve(), args.upstream_source.resolve(), args.output.resolve()
    if out == src or src in out.parents or out == original or original in out.parents:
        p.error("output must be separate from both source trees")
    for name, expected in PINS.items():
        if sha(original / name) != expected:
            p.error(f"pristine source hash mismatch: {name}")
    if sha(src / "libavcodec/h264idct_template.c") != PINS["libavcodec/h264idct_template.c"]:
        p.error("original C oracle must remain unchanged")
    if not (src / "config.h").is_file():
        p.error("source must already be configured")
    out.mkdir(parents=True, exist_ok=False)
    repo = Path(__file__).resolve().parent.parent
    fixtures = repo / "third_party/ffmpeg/8.1.2/tests"
    inputs = [fixtures / "h264-luma-dc-parity.c", fixtures / "h264-luma-dc-abi.S"]
    objects = [o.resolve() for o in args.object] or [src / "libavcodec/libavcodec.a", src / "libavutil/libavutil.a"]
    commands = []
    metadata = {"target": args.target, "host": {"system": platform.system(), "machine": platform.machine(),
                 "platform": platform.platform()}, "commands": commands, "inputs": {}, "mode": "benchmark" if args.benchmark else "parity"}

    def run(argv):
        item = {"argv": [str(x) for x in argv], "cwd": str(src)}
        commands.append(item)
        result = subprocess.run(item["argv"], cwd=src, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        item.update(returncode=result.returncode, output=result.stdout)
        (out / "results.json").write_text(json.dumps(metadata, indent=2) + "\n")
        if result.returncode:
            print(result.stdout, end="")
            raise RuntimeError(f"command failed ({result.returncode}): {shlex.join(item['argv'])}")
        return result.stdout

    cc = shlex.split(args.cc)
    if not cc:
        p.error("empty compiler command")
    try:
        metadata["compiler"] = run(cc + ["--version"])
        # Explicit compiler selection/flags are recorded; executable identity is independently checked below.
        common = cc + ["-O3", "-std=c11", "-D_GNU_SOURCE", "-I", str(src)] + args.cflag
        baseline = out / "baseline-init.o"
        run(common + ["-Dff_h264dsp_init_aarch64=ff_h264dsp_init_aarch64_baseline", "-c",
                      str(original / "libavcodec/aarch64/h264dsp_init_aarch64.c"), "-o", str(baseline)])
        fixture = out / "parity.o"
        run(common + ["-Wall", "-Wextra", "-Werror", "-c", str(inputs[0]), "-o", str(fixture)])
        abi = out / "abi.o"
        run(cc + ["-I", str(src)] + args.cflag + ["-c", str(inputs[1]), "-o", str(abi)])
        exe = out / "h264-luma-dc-parity"
        run(cc + args.cflag + [str(fixture), str(abi), str(baseline)] + [str(o) for o in objects]
            + ["-lm", "-pthread"] + args.ldflag + ["-o", str(exe)])
        binary_target(exe, args.target)
        identities = inputs + objects + [src / name for name in PINS] + [src / "config.h", src / "ffbuild/config.mak", exe]
        identities += [src / "libavcodec/aarch64/h264idct_neon.o", src / "libavcodec/h264idct.o", src / "libavcodec/aarch64/h264dsp_init_aarch64.o"]
        metadata["inputs"] = {str(path): sha(path) for path in identities}
        if args.execute:
            print(run([exe] + (["--benchmark"] if args.benchmark else [])), end="")
        else:
            print(f"compile-only: {exe}; no target process executed")
        metadata["status"] = "passed" if args.execute else "compiled-only"
    except (RuntimeError, ValueError, OSError) as error:
        metadata["status"] = "failed"
        metadata["error"] = str(error)
        print(str(error), file=sys.stderr)
        return 1
    finally:
        (out / "results.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
