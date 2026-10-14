# 私有 Windows guard/QPC adapter（SOURCE/STATIC）

此材料属于 Task4C5C3B2A；只有源码和待执行命令，尚未编译或执行。它不接线产品、共享 main、ABI、benchmark loop、host/process/avutil admission 或 native runner。共享 `h264-luma-dc-platform.h` 保持只读。adapter 无 main/constructor。

真实 translation unit 仅 `_WIN32` 原生 ARM64、64-bit pointer，拒绝 ARM64EC；直接使用 `<windows.h>` 的 GetSystemInfo、VirtualAlloc、VirtualProtect、VirtualFree、QueryPerformanceFrequency、QueryPerformanceCounter、GetLastError。没有 POSIX fallback、环境开关、时钟零值降级或 scalar codec 路径。

独立 contract TU 自己定义 `FRD_DC_PLATFORM_CONTRACT`、mock Win32 名字和类型后 include adapter C。该模式只测试注入的调用/所有权/时钟逻辑；其 SYSTEM_INFO mock 的 page 字段故意为 size_t，以覆盖真实 ARM64 DWORD page size 无法达到的乘法溢出。它不是 Win32 ABI、真实系统调用、Windows compiler/header、host/process 或 Windows 能力证明。任何 GREEN composition 必须拒绝该宏及此 TU。

guard 输入全零，reserve 3 页而仅 commit 中页。访问只保护中页，旧 protection 为 RO 或 RW 均允许（包括重复 RO/RW）；其它旧状态失败。commit/address 失败立即尝试 exact base/0/MEM_RELEASE；free 失败保留 owner 并输出原错误及 cleanup 错误，成功清零。owner 校验拒绝尺寸/地址溢出，零 destroy 无 syscall。

QPF 每进程仅尝试一次（失败也缓存且后续继续失败），要求 signed positive。QPC 要求 signed nonnegative、相对 origin 单调不减；首次完整成功是 origin。只有转换、记录容量等全部成功才提交 last/origin/记录和 output。相同 tick 是有效 ns，但后来 benchmark 必须拒绝零 elapsed。频率是计时频率，不是 CPU 时钟或 UTC。

`h264-luma-dc-clock-convert.h` 精确 floor(delta*1e9/frequency)，frequency 为 1..INT64_MAX；whole/rem 分离，30 个 binary quotient/remainder 步骤计算 fractional ns。每步比较/减法代替可能溢出的余数加法；whole 乘法和最后加法均检查。没有浮点/uint128，失败不写输出。clock arithmetic 不属于 codec decoding。

`h264-luma-dc-windows-clock-record.h` 是新私有读取接口。`frd_dc_windows_clock_read(record*)` 复制成功状态，不读取 QPC、不修改 clock state、不提供 reset 或 bypass。version=1，capacity=256，frequency、origin_tick、last_tick、每次完整成功的 tick/delta/ns 与 count 可在后继 runner 计时区外读取/输出。记录用固定数组；第257次调用显式失败且不提交 output/last，不静默丢记录。读未初始化 clock、空输出失败。256 足够覆盖已冻结 31×2×2=124 次 batch 边界及有界额外读取；实际共享 main 的调用总数与日志接线需后继 composition 确认，不能据此宣称 benchmark 已可复现。

独立测试先于实现写入：math tests 有 22 固定 result/output 检查、null 检查，24 个频率矩阵行、12 个边界/非法频率行、10000 个固定 seed random 行；Python 独立构造完整预期 tuple 序列，逐行核对输入后用直接大整数乘除核验 10036 行及终止标记。其中 frequency=10 的相邻输入覆盖 whole 合法但最后加法溢出的路径，frequency=1e9/delta=UINT64_MAX 覆盖最大合法输出。C 原 finite 输入、顺序和 seed 没有改变；固定结果检查不输出 CLOCK 行。contract cases 覆盖 guard success/顺序/参数、尺寸/owner、各 syscall failure、commit mismatch、unexpected oldProtection、second allocation、retained owner retry、QPF/QPC/range/backward/overflow/记录容量，所有故障是 injected logic。

## 待执行命令（本阶段没有执行）

root 显式 release 且独立源码审查后，为输出选择全新外部目录；下列 `$out` 与 `$cc` 是 root 提供的独立受信绝对路径，不从 artifact 记录执行命令。

```sh
# 仅 host math/注入逻辑；不依赖 Windows headers，不计 native 证明。
cc -std=c11 -O2 -Wall -Wextra -Werror \
  third_party/ffmpeg/8.1.2/tests/h264-luma-dc-clock-tests.c -o "$out/clock-tests"
"$out/clock-tests" > "$out/clock-matrix.txt"
python3 third_party/ffmpeg/8.1.2/tests/h264-luma-dc-clock-oracle.py < "$out/clock-matrix.txt"
cc -std=c11 -O2 -Wall -Wextra -Werror \
  third_party/ffmpeg/8.1.2/tests/h264-luma-dc-platform-windows-tests.c -o "$out/contract-tests"
for case in guard qpf-fail qpf-zero qpf-negative qpc-fail qpc-negative \
  qpc-backward qpc-same qpc-overflow clock-record-limit; do
  "$out/contract-tests" "$case" > "$out/$case.stdout" 2> "$out/$case.stderr" || exit 1
done

# 真实 ARM64 Windows object，必须使用批准的固定 compiler/header closure。
"$cc" --target=aarch64-w64-mingw32 -O3 -std=c11 -Wall -Wextra -Werror \
  -D_WIN32_WINNT=0x0A00 -c third_party/ffmpeg/8.1.2/tests/h264-luma-dc-platform-windows.c \
  -o "$out/platform-windows.o"
```

fix1 oracle 拒绝负例的待执行命令如下。必须先取得同次 C 测试保存的完整正例；脚本先要求正例通过，再保存每种破坏输入及 oracle stdout/stderr。所有负例要求 oracle 明确非零退出，timeout/脚本错误不计通过。当前 SOURCE/STATIC 阶段没有执行下列脚本。

```sh
python3 - "$out" <<'PY'
from pathlib import Path
import subprocess
import sys

out = Path(sys.argv[1])
oracle = Path('third_party/ffmpeg/8.1.2/tests/h264-luma-dc-clock-oracle.py').resolve()
lines = (out / 'clock-matrix.txt').read_text().splitlines()
if len(lines) != 10037 or lines[-1] != 'CLOCK-END 10036':
    raise SystemExit('FAIL negative-suite positive input shape')

def invoke(name, data, expected_success):
    text = '\n'.join(data) + '\n'
    (out / (name + '.input.txt')).write_text(text)
    result = subprocess.run([sys.executable, str(oracle)], input=text,
                            text=True, capture_output=True, timeout=10)
    (out / (name + '.stdout')).write_text(result.stdout)
    (out / (name + '.stderr')).write_text(result.stderr)
    if expected_success:
        if result.returncode != 0:
            raise SystemExit('FAIL positive oracle control')
    elif result.returncode == 0 or not result.stderr.startswith('FAIL '):
        raise SystemExit('FAIL oracle accepted mutant or failed without oracle rejection: ' + name)

invoke('oracle-positive', lines, True)
mutants = {}
data = lines.copy(); data[1] = data[0]; mutants['duplicate-replacement'] = data
mutants['all-duplicate-replacement'] = [lines[0]] * 10036 + [lines[-1]]
mutants['missing-row'] = lines[:1] + lines[2:]
mutants['extra-row'] = lines[:-1] + [lines[0], lines[-1]]
data = lines.copy(); data[0], data[1] = data[1], data[0]; mutants['reordered'] = data
data = lines.copy(); fields = data[0].split(); fields[1] = '2'; data[0] = ' '.join(fields)
mutants['wrong-tuple'] = data
data = lines.copy(); fields = data[0].split(); fields[4] = '1'; data[0] = ' '.join(fields)
mutants['wrong-result'] = data
data = lines.copy(); fields = data[0].split(); fields[3] = '-1'; data[0] = ' '.join(fields)
mutants['wrong-status'] = data
data = lines.copy(); data[-1] = 'CLOCK-END 10038'; mutants['wrong-end'] = data
mutants['missing-end'] = lines[:-1]
mutants['extra-after-end'] = lines + [lines[0]]
for name, data in mutants.items():
    invoke('oracle-negative-' + name, data, False)
print('PASS prepared oracle rejection suite mutants=' + str(len(mutants)))
PY
```

eventual 固定 compiler 是 llvm-mingw20250709 msvcrt ubuntu-22.04 x86_64 archive，SHA256 `422f13915a41acd8b93566e590531f55d3de6e531575197d51756ff8e8d16525`，driver `/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-clang`。当前任务不发现/安装/猜测工具或配置。不存在批准 Windows compiler 时仅 host math/contract 可执行；缺工具/headers/symbols 保留真实失败。真实 object 成功也仅 compiled-only。

后继必须提供独立 ready+case 已 flush 的真实 guard children，要求 exact Windows access violation（不是 timeout/任意错误）、合法访问正控制；真实 QPC、Windows ARM64 ABI/SEH/unwind、host/process/avutil、同 shipping object/CRT/import/PE identity、pristine RED、parity、同 exe benchmark 原始边界、package 和 live 仍缺。不得因为本 mock/host math PASS 宣称 Windows qualified。

主文档（brief 已核验 2026-10-14）：[VirtualAlloc](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualalloc)、[VirtualFree](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualfree)、[QPC](https://learn.microsoft.com/en-us/windows/win32/sysinfo/acquiring-high-resolution-time-stamps)。本阶段无网络访问。
