/* SPDX-License-Identifier: LGPL-2.1-or-later
 * Windows 原生 ARM64 私有 ABI/SEH 控制；没有数值核/性能/产品资格。
 * SOURCE ONLY：必须独立审查、固定 SDK 核对、root 放行后才编译/执行。
 */
#if !defined(_WIN32) || !defined(__aarch64__) || defined(__arm64ec__) || \
    defined(_M_ARM64EC) || defined(__ARM64EC__)
#error "私有 contract 只支持原生 Windows ARM64，拒绝 ARM64EC"
#endif
#ifndef FRD_DC_WINDOWS_ABI_CONTRACT
#error "只允许私有 contract executable"
#endif
#ifndef FRD_CONTRACT_VARIANT
#error "必须显式绑定唯一 helper 变体 0..4"
#endif
#if FRD_CONTRACT_VARIANT < 0 || FRD_CONTRACT_VARIANT > 4
#error "bad5 没有产生 object，不是 native candidate"
#endif
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdint.h>
#include <inttypes.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

_Static_assert(sizeof(void *) == 8, "64-bit pointers required");
_Static_assert(sizeof(int) == 4, "frozen signed w2 contract");
_Static_assert(sizeof(RUNTIME_FUNCTION) == 8, "ARM64 pdata record required");
_Static_assert(offsetof(CONTEXT, Sp) == 0x100, "fixed ARM64 context required");
_Static_assert(offsetof(CONTEXT, Pc) == 0x108, "fixed ARM64 context required");
_Static_assert(offsetof(CONTEXT, V) == 0x110, "fixed ARM64 SIMD context required");

typedef void (*frd_dc_kernel)(int16_t *, int16_t *, int);
typedef int (*frd_dc_checker)(int16_t *, int16_t *, int, frd_dc_kernel);
typedef void (*frd_dc_qmul_wrapper)(int16_t *, int16_t *, int, frd_dc_kernel);
extern int frd_h264_dc_check_abi(int16_t *, int16_t *, int, frd_dc_kernel);
extern uint64_t frd_h264_dc_outer_probe(int16_t *, int16_t *, int,
                                      frd_dc_kernel, frd_dc_checker);
#define LEAF(name) extern void name(int16_t *, int16_t *, int)
LEAF(frd_h264_dc_preserve);
LEAF(frd_h264_dc_simd_upper_only);
LEAF(frd_h264_dc_fault_x18_snapshot);
LEAF(frd_h264_dc_fault_fp_snapshot);
LEAF(frd_h264_dc_fault_sp_snapshot);
LEAF(frd_h264_dc_corrupt_fp);
LEAF(frd_h264_dc_corrupt_sp16);
LEAF(frd_h264_dc_corrupt_sp8);
LEAF(frd_h264_dc_wrong_return);
LEAF(frd_h264_dc_no_return);
#define DECL_REG(kind,n) \
    LEAF(frd_h264_dc_clobber_##kind##n##_lo); \
    LEAF(frd_h264_dc_clobber_##kind##n##_hi); \
    extern int frd_h264_dc_fake_checker_##kind##n \
        (int16_t *, int16_t *, int, frd_dc_kernel)
DECL_REG(x,19); DECL_REG(x,20); DECL_REG(x,21); DECL_REG(x,22); DECL_REG(x,23);
DECL_REG(x,24); DECL_REG(x,25); DECL_REG(x,26); DECL_REG(x,27); DECL_REG(x,28);
DECL_REG(d,8); DECL_REG(d,9); DECL_REG(d,10); DECL_REG(d,11);
DECL_REG(d,12); DECL_REG(d,13); DECL_REG(d,14); DECL_REG(d,15);
/* 数据符号只取地址，不以错误 C 函数签名调用 PC label。 */
extern const unsigned char frd_h264_dc_abi_pc_after_alloc[];
extern const unsigned char frd_h264_dc_abi_pc_body[];
extern const unsigned char frd_h264_dc_abi_pc_call[];
extern const unsigned char frd_h264_dc_abi_pc_return[];
extern const unsigned char frd_h264_dc_abi_pc_epilogue[];
extern const unsigned char frd_h264_dc_abi_pc_epilogue_after_d14[];
extern const unsigned char frd_h264_dc_abi_pc_ret[];
extern const unsigned char frd_h264_dc_outer_pc_return[];

#define BIT(n) (UINT64_C(1) << (n))
#define COUNT(a) (sizeof(a) / sizeof((a)[0]))
#define FRAME 192u
#define HELPER_LENGTH 0x440u
#define OUTER_LENGTH 0x430u
#define WATCHDOG_MS 10000u
#define NO_RETURN_MS 1500u
#define FORCED_EXIT 0xe0f00101u

typedef enum { REFUSED = 2, FAILED = 1, ABI_ONLY = 0 } result_code;
static uintptr_t image_base;
static DWORD image_size, pdata_rva, pdata_size;
static PRUNTIME_FUNCTION helper_entry, outer_entry;
static int16_t input[16], output[256];
static const uintptr_t pc_offsets[] = {0x000,0x004,0x030,0x1a0,0x1a4,0x410,0x414,0x43c};
static const char *const pc_ids[] = {"entry","after_alloc","body","call","return",
                                    "epilogue","after_d14","ret"};

#include "h264-luma-dc-windows-diagnostics.h"

static int fail(const char *id)
{
    fprintf(stderr, "FRD1 failed id=%s win32=%lu\n", id, (unsigned long)GetLastError());
    return FAILED;
}
static int range(DWORD rva, size_t n)
{
    return rva <= image_size && n <= (size_t)image_size - rva;
}
static uintptr_t helper_va(void) { return (uintptr_t)frd_h264_dc_check_abi; }
static uintptr_t outer_va(void) { return (uintptr_t)frd_h264_dc_outer_probe; }
static uint64_t slot(const unsigned char *p, size_t off)
{
    uint64_t v;
    memcpy(&v, p + off, sizeof(v));
    return v;
}
static void store(unsigned char *p, size_t off, uint64_t v)
{
    memcpy(p + off, &v, sizeof(v));
}

/* Admission 读取实际 PE/table，只用于归属和固定变体，绝不计算 expected context。 */
static int native_admission(void)
{
    typedef BOOL (WINAPI *wow_api)(HANDLE, USHORT *, USHORT *);
    typedef BOOL (WINAPI *machine_api)(HANDLE, PROCESS_INFORMATION_CLASS, LPVOID, DWORD);
    HMODULE kernel32=GetModuleHandleW(L"kernel32.dll");
    wow_api wow;
    machine_api get_machine;
    USHORT process = 0xffff, native = 0xffff;
    PROCESS_MACHINE_INFORMATION machine;
    SYSTEM_INFO si;
    CONTEXT c;
    const IMAGE_DOS_HEADER *dos;
    const IMAGE_NT_HEADERS64 *nt;
    if(!kernel32) return fail("kernel32_provider_unavailable");
    wow=(wow_api)GetProcAddress(kernel32,"IsWow64Process2");
    get_machine=(machine_api)GetProcAddress(kernel32,"GetProcessInformation");
    if(!wow || !get_machine) return fail("native_api_provider_unavailable");
    if (!wow(GetCurrentProcess(), &process, &native))
        return fail("native_api_unavailable");
    memset(&machine, 0, sizeof(machine));
    if (!get_machine(GetCurrentProcess(), ProcessMachineTypeInfo,&machine,sizeof(machine)))
        return fail("process_machine_api_unavailable");
    GetNativeSystemInfo(&si);
    if (process != IMAGE_FILE_MACHINE_UNKNOWN || native != IMAGE_FILE_MACHINE_ARM64 ||
        machine.ProcessMachine != IMAGE_FILE_MACHINE_ARM64 ||
        si.wProcessorArchitecture != PROCESSOR_ARCHITECTURE_ARM64)
        return fail("native_os_process_refused");
    image_base = (uintptr_t)GetModuleHandleW(NULL);
    if (!image_base) return fail("module");
    dos = (const IMAGE_DOS_HEADER *)image_base;
    if (dos->e_magic != IMAGE_DOS_SIGNATURE || dos->e_lfanew < 64 ||
        dos->e_lfanew > 0x10000) return fail("dos_header");
    nt = (const IMAGE_NT_HEADERS64 *)(image_base + (DWORD)dos->e_lfanew);
    if (nt->Signature != IMAGE_NT_SIGNATURE ||
        nt->FileHeader.Machine != IMAGE_FILE_MACHINE_ARM64 ||
        nt->OptionalHeader.Magic != IMAGE_NT_OPTIONAL_HDR64_MAGIC ||
        nt->FileHeader.SizeOfOptionalHeader != sizeof(nt->OptionalHeader) ||
        nt->OptionalHeader.NumberOfRvaAndSizes < IMAGE_NUMBEROF_DIRECTORY_ENTRIES)
        return fail("pe_arm64_required");
    image_size = nt->OptionalHeader.SizeOfImage;
    pdata_rva = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXCEPTION].VirtualAddress;
    pdata_size = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXCEPTION].Size;
    if (!range(pdata_rva, pdata_size) || !pdata_rva || !pdata_size || pdata_size % 8)
        return fail("pe_exception_directory");
    /* ARM64EC/ARM64X hybrid metadata 非零也拒绝，不能只信 compiler 宏。 */
    {
        IMAGE_DATA_DIRECTORY ld = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG];
        if (ld.Size) {
            const IMAGE_LOAD_CONFIG_DIRECTORY64 *cfg;
            size_t needed = offsetof(IMAGE_LOAD_CONFIG_DIRECTORY64, CHPEMetadataPointer) + 8;
            if (!range(ld.VirtualAddress, ld.Size) || ld.Size < sizeof(DWORD))
                return fail("load_config_range");
            cfg = (const IMAGE_LOAD_CONFIG_DIRECTORY64 *)(image_base + ld.VirtualAddress);
            if (cfg->Size > ld.Size) return fail("load_config_size");
            if (cfg->Size >= needed && cfg->CHPEMetadataPointer)
                return fail("hybrid_process_refused");
        }
    }
    memset(&c, 0, sizeof(c));
    RtlCaptureContext(&c);
    if ((c.Sp & 15) || ((uintptr_t)&c & 15)) return fail("native_stack_alignment");
    printf("FRD1 admission native=%04x process=%04x pe=aa64 pointer=64 sp_align=16 variant=%d\n",
           native, process, FRD_CONTRACT_VARIANT);
    return ABI_ONLY;
}

static PRUNTIME_FUNCTION owned_entry(uintptr_t pc, uintptr_t begin, unsigned length,
                                    int allow_absence)
{
    ULONG_PTR base = 0;
    PRUNTIME_FUNCTION f = RtlLookupFunctionEntry(pc, &base, NULL);
    uintptr_t lo = image_base + pdata_rva, hi = lo + pdata_size;
    if (!f) {
        if (allow_absence) printf("FRD1 lookup absent pc_rva=%" PRIxPTR "\n", pc-image_base);
        return NULL;
    }
    if (base != image_base || (uintptr_t)f < lo || (uintptr_t)f > hi-8 ||
        ((uintptr_t)f-lo) % 8 || base + f->BeginAddress != begin ||
        (f->UnwindData & 3) || !range(f->UnwindData, 4)) return NULL;
    /* 已观察 full xdata；不把内部 EpiloguePacked 当 pdata packed。 */
    if ((*(const DWORD *)(base + f->UnwindData) & 0x3ffffu) * 4u != length)
        return NULL;
    return f;
}

static int linked_admission(void)
{
    static const uint32_t prologue[] = {
        0xd10303ff,0xa9007bfd,0xa90153f3,0xa9025bf5,0xa90363f7,0xa9046bf9,
        0xa90573fb,0x6d0627e8,0x6d072fea,0x6d0837ec,0x6d093fee,0x910003fd
    };
    static const uint32_t epilogue[] = {
        0x6d493fee,0x6d4837ec,0x6d472fea,0x6d4627e8,0xa94573fb,0xa9446bf9,
        0xa94363f7,0xa9425bf5,0xa94153f3,0xa9407bfd,0x910303ff,0xd65f03c0
    };
    const unsigned char *const labels[] = {
        (const unsigned char *)frd_h264_dc_check_abi,
        frd_h264_dc_abi_pc_after_alloc, frd_h264_dc_abi_pc_body,
        frd_h264_dc_abi_pc_call, frd_h264_dc_abi_pc_return,
        frd_h264_dc_abi_pc_epilogue, frd_h264_dc_abi_pc_epilogue_after_d14,
        frd_h264_dc_abi_pc_ret
    };
    static const unsigned char good[] = {
        0x10,0x01,0x60,0x28,0xe1,0xd9,0x92,0xd9,0x10,0xd8,0x8e,0xd8,
        0x0c,0xe6,0xe6,0xe6,0xe6,0xc8,0x02,0x40,0x0c,0xe4,0xe3,0xe3
    };
    static const unsigned char bad3[] = {
        0x10,0x01,0x60,0x28,0xe1,0xd9,0x92,0xd9,0x10,0xd8,0x8e,0xd8,
        0x0c,0xe6,0xe6,0xe6,0xc8,0x84,0xc8,0x03,0x40,0x0c,0xe4,0xe3
    };
    static const unsigned char bad4[] = {
        0x10,0x01,0xa0,0x4c,0xe1,0xd9,0x92,0xd9,0x10,0xd8,0x8e,0xd8,
        0x0c,0xe6,0xe6,0xe6,0xe6,0xc8,0x02,0x40,0x0c,0xe4,0xe3,0xd9,
        0x10,0xd8,0x8e,0xd8,0x0c,0xe6,0xe6,0xe6,0xe6,0xc8,0x02,0x40,
        0x0c,0xe4,0xe3,0xe3
    };
    unsigned char expected[40];
    size_t n = sizeof(good), i;
    if (helper_va() < image_base || outer_va() < image_base ||
        helper_va()-image_base > UINT32_MAX || outer_va()-image_base > UINT32_MAX ||
        !range((DWORD)(helper_va()-image_base), HELPER_LENGTH) ||
        !range((DWORD)(outer_va()-image_base), OUTER_LENGTH)) return fail("label_range");
    if(memcmp((const void *)helper_va(),prologue,sizeof(prologue)) ||
       memcmp((const void *)outer_va(),prologue,sizeof(prologue)) ||
       memcmp((const void *)(helper_va()+0x410),epilogue,sizeof(epilogue)) ||
       memcmp((const void *)(outer_va()+0x400),epilogue,sizeof(epilogue)))
        return fail("actual_frame_instruction_binding");
    if((uintptr_t)frd_h264_dc_wrong_return < image_base ||
       (uintptr_t)frd_h264_dc_wrong_return-image_base > UINT32_MAX ||
       !range((DWORD)((uintptr_t)frd_h264_dc_wrong_return-image_base),16) ||
       *(const uint32_t *)(helper_va()+0x408)!=UINT32_C(0xd43e0000) ||
       *(const uint32_t *)((uintptr_t)frd_h264_dc_wrong_return+8)!=UINT32_C(0xd43e0000) ||
       *(const uint32_t *)(outer_va()+0x3f8)!=UINT32_C(0xd43e0000))
        return fail("actual_windows_breakpoint_binding");
    for (i=0; i<COUNT(labels); ++i) {
        ULONG_PTR base=0;
        PRUNTIME_FUNCTION f;
        if ((uintptr_t)labels[i] != helper_va()+pc_offsets[i]) return fail("linked_pc_offset");
        f = RtlLookupFunctionEntry((uintptr_t)labels[i], &base, NULL);
        if (FRD_CONTRACT_VARIANT == 1) {
            if (f) return fail("bad1_expected_missing_pdata");
        } else {
            PRUNTIME_FUNCTION owned = owned_entry((uintptr_t)labels[i], helper_va(), HELPER_LENGTH, 0);
            if (!owned || (helper_entry && owned != helper_entry)) return fail("helper_table_owner");
            helper_entry = owned;
        }
        printf("FRD1 binding id=%s rva=%" PRIxPTR " offset=%" PRIxPTR " table=%s\n",
               pc_ids[i], (uintptr_t)labels[i]-image_base, pc_offsets[i], f ? "present":"absent");
    }
    if ((uintptr_t)frd_h264_dc_outer_pc_return != outer_va()+0x188)
        return fail("outer_relative_return");
    outer_entry = owned_entry((uintptr_t)frd_h264_dc_outer_pc_return, outer_va(), OUTER_LENGTH, 0);
    if (!outer_entry || !range(outer_entry->UnwindData, sizeof(good))) return fail("outer_owner");
    /* outer header length 0x430 = 268 instructions，与 helper 的 272 独立。 */
    memcpy(expected, good, sizeof(good)); expected[0]=0x0c;
    if (memcmp((const void *)(image_base+outer_entry->UnwindData), expected, sizeof(good)))
        return fail("outer_metadata_binding");
    if (FRD_CONTRACT_VARIANT != 1) {
        memcpy(expected, good, sizeof(good));
        if (FRD_CONTRACT_VARIANT == 2) expected[20] = 0x0d;
        if (FRD_CONTRACT_VARIANT == 3) memcpy(expected, bad3, sizeof(bad3));
        if (FRD_CONTRACT_VARIANT == 4) { memcpy(expected, bad4, sizeof(bad4)); n=sizeof(bad4); }
        if (!range(helper_entry->UnwindData, n) ||
            memcmp((const void *)(image_base+helper_entry->UnwindData), expected, n))
            return fail("configured_variant_mismatch");
    }
    printf("FRD1 binding outer_rva=%" PRIxPTR " return_offset=188 epilogue_offset=400 length=430\n",
           outer_va()-image_base);
    return ABI_ONLY;
}

/* Expected 永远来自已知指令/saved slots；以下比较不读取 unwind metadata。 */
static uint64_t differences(const CONTEXT *actual, const CONTEXT *expected)
{
    uint64_t mask=0;
    unsigned i;
    for (i=19;i<=28;++i) if (actual->X[i]!=expected->X[i]) mask |= BIT(i-19);
    for (i=8;i<=15;++i) if (actual->V[i].Low!=expected->V[i].Low) mask |= BIT(i+2);
    if (actual->X[18]!=expected->X[18]) mask |= BIT(18);
    if (actual->Fp!=expected->Fp) mask |= BIT(19);
    if (actual->Sp!=expected->Sp) mask |= BIT(20);
    if (actual->Pc!=expected->Pc) mask |= BIT(21);
    if (actual->Lr!=expected->Lr) mask |= BIT(22);
    return mask;
}
static int unwind_one(CONTEXT *c, PRUNTIME_FUNCTION entry)
{
    PVOID handler_data=NULL;
    ULONG_PTR establisher=0;
    PEXCEPTION_ROUTINE handler;
    if (!entry) return fail("nonleaf_missing_table");
    /* SDK 原型必须由 real headers 提供；没有手写兼容 ABI 或替代 unwinder。 */
    handler=RtlVirtualUnwind(UNW_FLAG_NHANDLER, image_base, c->Pc, entry, c,
                            &handler_data, &establisher, NULL);
    printf("FRD1 rtl establisher=%" PRIxPTR " handler=%s\n",(uintptr_t)establisher,
           handler ? "present":"absent");
    if(handler) return fail("unexpected_exception_handler");
    return ABI_ONLY;
}
static uint64_t pattern(unsigned n, int outer)
{
    if (outer) return ((uint64_t)(0x4800+n)<<48) | ((uint64_t)(0x3700+n)<<32) |
                      ((uint64_t)(0x2600+n)<<16) | (0x1500+n);
    return ((uint64_t)(0x8400+n)<<48) | ((uint64_t)(0x7300+n)<<32) |
           ((uint64_t)(0x6200+n)<<16) | (0x5100+n);
}

static int synthetic_suite(void)
{
    /* 额外余量只防止受控 bad2/bad3 读取越过 C 对象；不是正确 frame 的扩容。 */
    _Alignas(16) unsigned char stack[512];
    CONTEXT expected, initial, actual, observed_control;
    const uintptr_t frame=(uintptr_t)(stack+128);
    const uint64_t fp=UINT64_C(0x1938576abcd00010), lr=(uintptr_t)frd_h264_dc_outer_pc_return;
    size_t k;
    unsigned i;
    memset(&expected,0,sizeof(expected)); expected.ContextFlags=CONTEXT_FULL;
    expected.Sp=frame+FRAME; expected.Fp=fp; expected.Lr=lr; expected.Pc=lr;
    expected.X[18]=UINT64_C(0x1827364554637281);
    for(i=19;i<=28;++i) expected.X[i]=UINT64_C(0x29384756abcd0100)+i;
    for(i=8;i<=15;++i) expected.V[i].Low=UINT64_C(0x81726354fedc0100)+i;
    for(k=0;k<COUNT(pc_offsets);++k) {
        uint64_t want=0, got;
        memset(stack,0xa5,sizeof(stack));
        /* entry/after_alloc 尚未保存；poison 能区分错误地提前读取 saved slots。 */
        if(k>=2) {
            store((unsigned char *)frame,0,fp); store((unsigned char *)frame,8,lr);
            for(i=19;i<=28;++i) store((unsigned char *)frame,16+8*(i-19),expected.X[i]);
            for(i=8;i<=15;++i) store((unsigned char *)frame,96+8*(i-8),expected.V[i].Low);
        }
        initial=expected;
        initial.Pc=helper_va()+pc_offsets[k];
        if (k!=0 && k!=7) initial.Sp=frame;
        if (k>=2 && k<=6) initial.Fp=frame;
        if (k>=3 && k<=6) {
            for(i=19;i<=28;++i) initial.X[i]=pattern(i,0);
            for(i=8;i<=15;++i) initial.V[i].Low=pattern(0x80+i,0);
        }
        if(k==6) {initial.V[14].Low=expected.V[14].Low; initial.V[15].Low=expected.V[15].Low;}
        /* 完整 prologue 后 BLR 前 LR仍 caller；返回后 LR为 helper continuation。 */
        if(k>=4 && k<=6) initial.Lr=(uintptr_t)frd_h264_dc_abi_pc_return;
        if(FRD_CONTRACT_VARIANT==1) {
            ULONG_PTR base=0;
            if(RtlLookupFunctionEntry(initial.Pc,&base,NULL)) return fail("bad1_unexpected_entry");
            printf("FRD1 unwind id=%s outcome=expected-missing-nonleaf-table\n",pc_ids[k]);
            continue; /* 明确拒绝，无 lookup-null leaf 成功替代。 */
        }
        actual=initial;
        if(unwind_one(&actual,helper_entry)) return FAILED;
        got=differences(&actual,&expected);
        if(FRD_CONTRACT_VARIANT==2 && k!=0 && k!=7) want=BIT(20);
        if(FRD_CONTRACT_VARIANT==3 && k>=2 && k<=6) want=BIT(0)|BIT(1);
        if(FRD_CONTRACT_VARIANT==4 && k==5) want=BIT(16)|BIT(17);
        /* 正确 caller oracle 不变；坏控制还必须呈现精确、独立 saved-slot 差值。 */
        observed_control=expected;
        if(want==BIT(20)) observed_control.Sp=expected.Sp+16;
        if(want==(BIT(0)|BIT(1))) {
            observed_control.X[19]=expected.X[20]; observed_control.X[20]=expected.X[21];
        }
        if(want==(BIT(16)|BIT(17))) {
            observed_control.V[14].Low=initial.V[14].Low;
            observed_control.V[15].Low=initial.V[15].Low;
        }
        printf("FRD1 unwind id=%s diff=%016" PRIx64 " expected=%016" PRIx64
               " caller_sp=%016" PRIx64 " caller_pc=%016" PRIx64 "\n",
               pc_ids[k],got,want,(uint64_t)actual.Sp,(uint64_t)actual.Pc);
        if(got!=want || differences(&actual,&observed_control)) return fail("unwind_exact_context");
    }
    return ABI_ONLY;
}

/* 实际 callback 栈在其仍存活时回溯；C compiler frame 由真实 Rtl 解码，
 * helper/outer expected 从有效 FP 链和固定保存指令独立冻结，不从 Rtl 输出回推。 */
static int walk_result=FAILED;
static int stack_contains(uintptr_t address,size_t n)
{
    MEMORY_BASIC_INFORMATION m;
    if(!VirtualQuery((const void *)address,&m,sizeof(m)) || m.State!=MEM_COMMIT ||
       (m.Protect & (PAGE_NOACCESS|PAGE_GUARD))) return 0;
    return address >= (uintptr_t)m.BaseAddress && n <= m.RegionSize &&
           address-(uintptr_t)m.BaseAddress <= m.RegionSize-n;
}
static void expected_slots(CONTEXT *e,const unsigned char *frame,uintptr_t teb)
{
    unsigned i;
    memset(e,0,sizeof(*e)); e->ContextFlags=CONTEXT_FULL;
    e->Sp=(uintptr_t)frame+FRAME; e->Fp=slot(frame,0); e->Lr=slot(frame,8); e->Pc=e->Lr;
    e->X[18]=teb;
    for(i=19;i<=28;++i) e->X[i]=slot(frame,16+8*(i-19));
    for(i=8;i<=15;++i) e->V[i].Low=slot(frame,96+8*(i-8));
}
static __attribute__((noinline)) void capture_callback(int16_t *in,int16_t *out,int q)
{
    CONTEXT c, helper_expected, outer_expected;
    uintptr_t cursor, helper_frame=0, outer_frame=0;
    unsigned depth,i;
    (void)in; (void)out; (void)q;
    memset(&c,0,sizeof(c)); RtlCaptureContext(&c);
    /* 已知 x29 链独立定位 callback 的 caller，而非从 helper unwind 得到 oracle。 */
    cursor=c.Fp;
    for(depth=0;depth<16;++depth) {
        uint64_t next, ret;
        if((cursor&15) || !stack_contains(cursor,16)) {fail("callback_fp_chain"); return;}
        next=slot((const unsigned char *)cursor,0); ret=slot((const unsigned char *)cursor,8);
        if(ret==(uintptr_t)frd_h264_dc_abi_pc_return) {helper_frame=(uintptr_t)next; break;}
        if(next<=cursor) {fail("callback_chain_order"); return;}
        cursor=(uintptr_t)next;
    }
    if(!helper_frame || !stack_contains(helper_frame,FRAME*2) || (helper_frame&15)) {
        fail("live_helper_frame"); return;
    }
    outer_frame=helper_frame+FRAME;
    if(slot((const unsigned char *)helper_frame,0)!=outer_frame ||
       slot((const unsigned char *)helper_frame,8)!=(uintptr_t)frd_h264_dc_outer_pc_return) {
        fail("live_known_frame_pair"); return;
    }
    expected_slots(&helper_expected,(const unsigned char *)helper_frame,c.X[18]);
    expected_slots(&outer_expected,(const unsigned char *)outer_frame,c.X[18]);
    for(i=19;i<=28;++i) if(helper_expected.X[i]!=pattern(i,1)) {fail("outer_saved_pattern"); return;}
    for(i=8;i<=15;++i) if(helper_expected.V[i].Low!=pattern(0x90+i,1)) {fail("outer_saved_simd"); return;}
    /* 只对当前 callback 的真实 table 做一步；拒绝无表非leaf，拒绝未知 PC。 */
    {
        ULONG_PTR base=0;
        PRUNTIME_FUNCTION f=RtlLookupFunctionEntry(c.Pc,&base,NULL);
        uintptr_t lo=image_base+pdata_rva,hi=lo+pdata_size;
        if(!f || base!=image_base || (uintptr_t)f<lo || (uintptr_t)f>hi-8 ||
           ((uintptr_t)f-lo)%8 || base+f->BeginAddress!=(uintptr_t)capture_callback) {
            fail("callback_table"); return;
        }
        if(unwind_one(&c,f)) return;
    }
    if(c.Pc!=(uintptr_t)frd_h264_dc_abi_pc_return || c.Sp!=helper_frame || c.Fp!=helper_frame) {
        fail("callback_to_helper"); return;
    }
    for(i=19;i<=28;++i) if(c.X[i]!=pattern(i,0)) {fail("callback_helper_gpr"); return;}
    for(i=8;i<=15;++i) if(c.V[i].Low!=pattern(0x80+i,0)) {fail("callback_helper_simd"); return;}
    if(owned_entry(c.Pc,helper_va(),HELPER_LENGTH,0)!=helper_entry ||
       unwind_one(&c,helper_entry) || differences(&c,&helper_expected)) {fail("live_helper_to_outer"); return;}
    if(owned_entry(c.Pc,outer_va(),OUTER_LENGTH,0)!=outer_entry ||
       unwind_one(&c,outer_entry) || differences(&c,&outer_expected)) {fail("live_outer_to_caller"); return;}
    printf("FRD1 walk callback-helper-outer-caller outcome=matched\n");
    walk_result=ABI_ONLY;
}
static int live_walk_suite(void)
{
    uint64_t m;
    /* bad1/2/3 的已知缺陷由 synthetic 判别；不在坏 Rtl 输出上继续猜 caller。 */
    if(FRD_CONTRACT_VARIANT!=0 && FRD_CONTRACT_VARIANT!=4) {
        printf("FRD1 walk outcome=not-applicable-bad-body-metadata\n"); return ABI_ONLY;
    }
    walk_result=FAILED;
    m=frd_h264_dc_outer_probe(input,output,1,capture_callback,frd_h264_dc_check_abi);
    if(m || walk_result) return fail("live_capture_walk");
    return ABI_ONLY;
}

typedef struct {const char *id; frd_dc_kernel lo,hi; frd_dc_checker fake; unsigned bit;} reg_case;
#define REG(kind,n,bit) {#kind #n,frd_h264_dc_clobber_##kind##n##_lo, \
    frd_h264_dc_clobber_##kind##n##_hi,frd_h264_dc_fake_checker_##kind##n,bit}
static const reg_case registers[] = {
    REG(x,19,0),REG(x,20,1),REG(x,21,2),REG(x,22,3),REG(x,23,4),
    REG(x,24,5),REG(x,25,6),REG(x,26,7),REG(x,27,8),REG(x,28,9),
    REG(d,8,10),REG(d,9,11),REG(d,10,12),REG(d,11,13),
    REG(d,12,14),REG(d,13,15),REG(d,14,16),REG(d,15,17)
};
static int masks(const char *id,frd_dc_kernel callback,frd_dc_checker checker,uint64_t expected)
{
    uint64_t m=frd_h264_dc_outer_probe(input,output,1,callback,checker);
    printf("FRD1 mask id=%s actual=%016" PRIx64 " expected=%016" PRIx64 "\n",id,m,expected);
    return m==expected ? ABI_ONLY:fail("exact_mask");
}
static int register_suite(void)
{
    size_t i;
    if(masks("preserve",frd_h264_dc_preserve,frd_h264_dc_check_abi,0) ||
       masks("simd_upper_only",frd_h264_dc_simd_upper_only,frd_h264_dc_check_abi,0)) return FAILED;
    for(i=0;i<COUNT(registers);++i) {
        const reg_case *r=&registers[i];
        char low_id[32],high_id[32],fake_id[32];
        if(snprintf(low_id,sizeof(low_id),"%s_lo",r->id)<0 ||
           snprintf(high_id,sizeof(high_id),"%s_bit40",r->id)<0 ||
           snprintf(fake_id,sizeof(fake_id),"%s_fake_checker",r->id)<0) return fail("register_case_id");
        if(masks(low_id,r->lo,frd_h264_dc_check_abi,BIT(r->bit)) ||
           masks(high_id,r->hi,frd_h264_dc_check_abi,BIT(r->bit)) ||
           masks(fake_id,frd_h264_dc_preserve,r->fake,BIT(r->bit+32))) return FAILED;
    }
    return masks("x18_snapshot",frd_h264_dc_fault_x18_snapshot,frd_h264_dc_check_abi,BIT(18)) ||
           masks("fp_snapshot",frd_h264_dc_fault_fp_snapshot,frd_h264_dc_check_abi,BIT(19)) ||
           masks("sp_snapshot",frd_h264_dc_fault_sp_snapshot,frd_h264_dc_check_abi,BIT(20));
}

typedef enum { CHILD_PRESERVE, CHILD_FP, CHILD_SP16, CHILD_SP8, CHILD_LR,
               CHILD_NO_RETURN, CHILD_COUNT } child_id;
typedef struct {const char *name; frd_dc_kernel callback;} child_case;
static const child_case children[CHILD_COUNT] = {
    {"preserve",frd_h264_dc_preserve},{"corrupt_fp",frd_h264_dc_corrupt_fp},
    {"corrupt_sp16",frd_h264_dc_corrupt_sp16},{"corrupt_sp8",frd_h264_dc_corrupt_sp8},
    {"wrong_return",frd_h264_dc_wrong_return},{"no_return",frd_h264_dc_no_return}
};
typedef struct {uint32_t magic,case_id;} go_message;
#define GO_MAGIC UINT32_C(0x46524431)
typedef struct {unsigned char bytes[32768]; size_t used; int closed;} stream_capture;
typedef struct {
    int ready,go,initial_breakpoint,first,second,unexpected,forced,timeout,terminal;
    int designated_timeout;
    int exit_event_seen,exit_continue_attempted,exit_continue_ok,process_wait_attempted;
    BOOL exit_continue_result;
    DWORD exit_code,exception_code;
    DWORD exit_continue_error,process_wait_result,process_wait_error;
    uintptr_t exception_pc;
    uint32_t instruction;
    ULONGLONG ready_ticks,terminate_ticks,exit_event_ticks,terminal_ticks;
} child_observation;

static int child_main(child_id id)
{
    go_message go;
    DWORD bytes=0;
    uint64_t m;
    char ready[96];
    int n;
    if(!IsDebuggerPresent()) return fail("child_requires_native_debugger_parent");
    n=snprintf(ready,sizeof(ready),"FRD1 ready case=%s variant=%d\n",children[id].name,FRD_CONTRACT_VARIANT);
    if(n<0 || (size_t)n>=sizeof(ready)) return fail("child_ready_format");
    if(fflush(stdout) || fflush(stderr)) return fail("ready_flush");
    /* WriteFile 避免 CRT text-mode 把协议 LF 改为 CRLF。 */
    if(!WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),ready,(DWORD)n,&bytes,NULL) || bytes!=(DWORD)n)
        return fail("child_ready_write");
    if(!ReadFile(GetStdHandle(STD_INPUT_HANDLE),&go,sizeof(go),&bytes,NULL) ||
       bytes!=sizeof(go) || go.magic!=GO_MAGIC || go.case_id!=(uint32_t)id)
        return fail("child_go_protocol");
    frd_diag_reference.pid=GetCurrentProcessId();
    frd_diag_reference.tid=GetCurrentThreadId();
    frd_diag_reference.case_id=(DWORD)id;
    RtlCaptureContext(&frd_diag_reference.context);
    frd_diag_reference.complete=1;
    m=frd_h264_dc_outer_probe(input,output,1,children[id].callback,frd_h264_dc_check_abi);
    printf("FRD1 returned case=%s mask=%016" PRIx64 "\n",children[id].name,m);
    if(id!=CHILD_PRESERVE || m) return FAILED;
    {
        static const char result[]="FRD1 result case=preserve outcome=normal-control\n";
        if(fflush(stdout) || !WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),result,sizeof(result)-1,&bytes,NULL) ||
           bytes!=sizeof(result)-1) return fail("child_result_write");
    }
    return ABI_ONLY;
}

static int drain(HANDLE pipe,stream_capture *s)
{
    DWORD available=0,got=0;
    if(s->closed) return ABI_ONLY;
    if(!PeekNamedPipe(pipe,NULL,0,NULL,&available,NULL)) {
        if(GetLastError()==ERROR_BROKEN_PIPE) {s->closed=1; return ABI_ONLY;}
        return fail("pipe_peek");
    }
    while(available) {
        size_t room=sizeof(s->bytes)-1-s->used;
        DWORD n=available < room ? available:(DWORD)room;
        if(!room) return fail("child_output_bound");
        if(!ReadFile(pipe,s->bytes+s->used,n,&got,NULL) || !got) return fail("pipe_read");
        s->used+=got; s->bytes[s->used]=0;
        if(!PeekNamedPipe(pipe,NULL,0,NULL,&available,NULL)) {
            if(GetLastError()==ERROR_BROKEN_PIPE) {s->closed=1; break;}
            return fail("pipe_peek_after_read");
        }
    }
    return ABI_ONLY;
}
static int raw_stream(const char *id,const char *kind,const stream_capture *s)
{
    DWORD written=0;
    printf("FRD1 raw case=%s stream=%s bytes=%zu begin\n",id,kind,s->used);
    /* 原始 child bytes 不经 parent CRT text-mode 二次变换。 */
    if(fflush(stdout) || (s->used &&
       (!WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),s->bytes,(DWORD)s->used,&written,NULL) ||
        written!=s->used))) return fail("raw_capture_write");
    printf("\nFRD1 raw case=%s stream=%s end\n",id,kind);
    return ABI_ONLY;
}
static int ready_message(child_id id,stream_capture *out,child_observation *o,HANDLE go_pipe)
{
    char line[96];
    const char *match;
    int n=snprintf(line,sizeof(line),"FRD1 ready case=%s variant=%d\n",children[id].name,FRD_CONTRACT_VARIANT);
    if(n<0 || (size_t)n>=sizeof(line)) return fail("ready_format");
    match=strstr((const char *)out->bytes,line);
    if(!match) return ABI_ONLY;
    if(match!=(const char *)out->bytes && match[-1]!='\n') return fail("ready_line_boundary");
    if(strstr(match+n,line)) return fail("duplicate_ready");
    if(!o->ready) {
        go_message go={GO_MAGIC,(uint32_t)id};
        DWORD written=0;
        o->ready=1; o->ready_ticks=GetTickCount64();
        if(!WriteFile(go_pipe,&go,sizeof(go),&written,NULL) || written!=sizeof(go))
            return fail("parent_go_write");
        o->go=1;
    }
    return ABI_ONLY;
}
static int exact_trap(HANDLE process,uintptr_t child_base,child_id id,
                      const EXCEPTION_DEBUG_INFO *e,child_observation *o)
{
    const EXCEPTION_RECORD *r=&e->ExceptionRecord;
    uintptr_t wanted;
    uint32_t opcode=0;
    SIZE_T got=0;
    if(id==CHILD_LR) wanted=child_base+((uintptr_t)frd_h264_dc_wrong_return-image_base)+8;
    else wanted=child_base+(helper_va()-image_base)+0x408;
    if(!o->ready || !o->go || (id==CHILD_PRESERVE || id==CHILD_NO_RETURN) ||
       r->ExceptionCode!=EXCEPTION_BREAKPOINT || (uintptr_t)r->ExceptionAddress!=wanted ||
       !ReadProcessMemory(process,(const void *)wanted,&opcode,sizeof(opcode),&got) ||
       got!=sizeof(opcode) || opcode!=UINT32_C(0xd43e0000))
        return 0;
    o->exception_code=r->ExceptionCode; o->exception_pc=wanted; o->instruction=opcode;
    if(e->dwFirstChance) {
        if(o->first || o->second) return 0;
        o->first=1;
    } else {
        if(!o->first || o->second) return 0;
        o->second=1;
    }
    return 1;
}

static int isolated_child(child_id id)
{
    SECURITY_ATTRIBUTES sa={sizeof(sa),NULL,TRUE};
    HANDLE stdout_read=NULL,stdout_write=NULL,stderr_read=NULL,stderr_write=NULL;
    HANDLE go_read=NULL,go_write=NULL,job=NULL,wait_process=NULL;
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits;
    wchar_t path[32768],cmd[33024];
    stream_capture out={0},err={0};
    child_observation o={0};
    frdd_state diagnostics={0};
    uintptr_t child_base=0;
    ULONGLONG started=GetTickCount64(),cleanup_deadline=0;
    int infrastructure=0,created=0,job_assigned=0;
    DWORD path_n;
    memset(&pi,0,sizeof(pi)); memset(&si,0,sizeof(si)); si.cb=sizeof(si);
    SetErrorMode(SEM_FAILCRITICALERRORS|SEM_NOGPFAULTERRORBOX|SEM_NOOPENFILEERRORBOX);
    path_n=GetModuleFileNameW(NULL,path,(DWORD)COUNT(path));
    if(!path_n || path_n>=COUNT(path)) {fail("self_path"); goto cleanup;}
    if(swprintf(cmd,COUNT(cmd),L"\"%ls\" --child %u %u",path,(unsigned)id,
                (unsigned)FRD_CONTRACT_VARIANT)<0) {fail("child_command"); goto cleanup;}
    if(!CreatePipe(&stdout_read,&stdout_write,&sa,0) ||
       !CreatePipe(&stderr_read,&stderr_write,&sa,0) ||
       !CreatePipe(&go_read,&go_write,&sa,0) ||
       !SetHandleInformation(stdout_read,HANDLE_FLAG_INHERIT,0) ||
       !SetHandleInformation(stderr_read,HANDLE_FLAG_INHERIT,0) ||
       !SetHandleInformation(go_write,HANDLE_FLAG_INHERIT,0)) {fail("child_pipes"); goto cleanup;}
    job=CreateJobObjectW(NULL,NULL);
    memset(&limits,0,sizeof(limits)); limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if(!job || !SetInformationJobObject(job,JobObjectExtendedLimitInformation,&limits,sizeof(limits))) {
        fail("child_ownership"); goto cleanup;
    }
    si.dwFlags=STARTF_USESTDHANDLES; si.hStdInput=go_read; si.hStdOutput=stdout_write; si.hStdError=stderr_write;
    if(!CreateProcessW(path,cmd,NULL,NULL,TRUE,
                       DEBUG_ONLY_THIS_PROCESS|CREATE_SUSPENDED|CREATE_NO_WINDOW,
                       NULL,NULL,&si,&pi)) {fail("create_debug_child"); goto cleanup;}
    created=1;
    diagnostics.pid=pi.dwProcessId;
    /* 不依赖 ContinueDebugEvent 自动关闭的 debugger event handle 的生命期。 */
    if(!DuplicateHandle(GetCurrentProcess(),pi.hProcess,GetCurrentProcess(),&wait_process,
                        0,FALSE,DUPLICATE_SAME_ACCESS))
        infrastructure=fail("duplicate_process_wait_handle");
    if(!DebugSetProcessKillOnExit(TRUE)) infrastructure=fail("debugger_kill_on_exit");
    if(!AssignProcessToJobObject(job,pi.hProcess)) infrastructure=fail("assign_child_job");
    else job_assigned=1;
    CloseHandle(stdout_write); stdout_write=NULL; CloseHandle(stderr_write); stderr_write=NULL;
    CloseHandle(go_read); go_read=NULL;
    if(!infrastructure && ResumeThread(pi.hThread)==(DWORD)-1) infrastructure=fail("resume_child");
    printf("FRD1 child case=%s pid=%lu watchdog_ms=%u no_return_ms=%u\n",children[id].name,
           (unsigned long)pi.dwProcessId,WATCHDOG_MS,id==CHILD_NO_RETURN ? NO_RETURN_MS:0);
    while(!o.exit_event_seen) {
        DEBUG_EVENT ev;
        DWORD continuation=DBG_CONTINUE;
        ULONGLONG now;
        if(drain(stdout_read,&out) || drain(stderr_read,&err) ||
           ready_message(id,&out,&o,go_write)) infrastructure=FAILED;
        now=GetTickCount64();
        if(!o.forced && (infrastructure || now-started>=WATCHDOG_MS ||
           (id==CHILD_NO_RETURN && o.ready && now-o.ready_ticks>=NO_RETURN_MS))) {
            o.timeout=!infrastructure;
            o.designated_timeout=!infrastructure && id==CHILD_NO_RETURN && o.ready && o.go &&
                                  now-o.ready_ticks>=NO_RETURN_MS;
            o.forced=1;
            o.terminate_ticks=now;
            cleanup_deadline=now+WATCHDOG_MS;
            if(!TerminateProcess(pi.hProcess,FORCED_EXIT)) infrastructure=fail("terminate_child");
        }
        if(o.forced && now>=cleanup_deadline) {
            infrastructure=fail("terminal_wait_expired"); break;
        }
        memset(&ev,0,sizeof(ev));
        if(!WaitForDebugEvent(&ev,50)) {
            if(GetLastError()!=ERROR_SEM_TIMEOUT) infrastructure=fail("wait_debug_event");
            continue;
        }
        ++diagnostics.seq;
        if(ev.dwProcessId!=pi.dwProcessId) infrastructure=fail("foreign_debug_event");
        switch(ev.dwDebugEventCode) {
        case CREATE_PROCESS_DEBUG_EVENT:
            child_base=(uintptr_t)ev.u.CreateProcessInfo.lpBaseOfImage;
            printf("FRD1 child_image case=%s pid=%lu base=%" PRIxPTR "\n",
                   children[id].name,(unsigned long)ev.dwProcessId,child_base);
            frdd_module_load(&diagnostics,pi.hProcess,&ev,child_base,ev.u.CreateProcessInfo.hFile);
            break;
        case LOAD_DLL_DEBUG_EVENT:
            frdd_module_load(&diagnostics,pi.hProcess,&ev,(uintptr_t)ev.u.LoadDll.lpBaseOfDll,ev.u.LoadDll.hFile);
            break;
        case UNLOAD_DLL_DEBUG_EVENT:
            frdd_unload(&diagnostics,(uintptr_t)ev.u.UnloadDll.lpBaseOfDll);
            printf("FRD1 debug_event case=%s pid=%lu tid=%lu event=%lu classification=ordinary\n",
                   children[id].name,(unsigned long)ev.dwProcessId,(unsigned long)ev.dwThreadId,
                   (unsigned long)ev.dwDebugEventCode);
            break;
        case CREATE_THREAD_DEBUG_EVENT:
        case EXIT_THREAD_DEBUG_EVENT:
        case OUTPUT_DEBUG_STRING_EVENT:
            /* 显式允许的普通事件；thread event handle 由 Windows debugger 关闭。 */
            printf("FRD1 debug_event case=%s pid=%lu tid=%lu event=%lu classification=ordinary\n",
                   children[id].name,(unsigned long)ev.dwProcessId,(unsigned long)ev.dwThreadId,
                   (unsigned long)ev.dwDebugEventCode);
            break;
        case EXCEPTION_DEBUG_EVENT: {
            const EXCEPTION_DEBUG_INFO *e=&ev.u.Exception;
            printf("FRD1 exception case=%s pid=%lu tid=%lu first=%lu code=%08lx flags=%08lx pc=%" PRIxPTR " ready=%d\n",
                   children[id].name,(unsigned long)ev.dwProcessId,(unsigned long)ev.dwThreadId,
                   (unsigned long)e->dwFirstChance,(unsigned long)e->ExceptionRecord.ExceptionCode,
                   (unsigned long)e->ExceptionRecord.ExceptionFlags,(uintptr_t)e->ExceptionRecord.ExceptionAddress,o.ready);
            if(e->ExceptionRecord.NumberParameters>EXCEPTION_MAXIMUM_PARAMETERS) o.unexpected=1;
            else {
                DWORD parameter;
                printf("FRD1 exception_parameters case=%s count=%lu",children[id].name,
                       (unsigned long)e->ExceptionRecord.NumberParameters);
                for(parameter=0;parameter<e->ExceptionRecord.NumberParameters;++parameter)
                    printf(" p%lu=%" PRIxPTR,(unsigned long)parameter,
                           (uintptr_t)e->ExceptionRecord.ExceptionInformation[parameter]);
                printf("\n");
            }
            /* 只处理 ready 之前、模块以外的唯一 loader debugger breakpoint。 */
            if(!o.ready && !o.initial_breakpoint && e->dwFirstChance &&
               e->ExceptionRecord.ExceptionCode==EXCEPTION_BREAKPOINT &&
               ((uintptr_t)e->ExceptionRecord.ExceptionAddress < child_base ||
                (uintptr_t)e->ExceptionRecord.ExceptionAddress >= child_base+image_size))
                o.initial_breakpoint=1;
            else {
                frdd_capture(&diagnostics,pi.hProcess,&ev,child_base,image_base,image_size,(unsigned)id);
                continuation=DBG_EXCEPTION_NOT_HANDLED;
                if(!exact_trap(pi.hProcess,child_base,id,e,&o)) o.unexpected=1;
                /* 不吞异常、不跳过 brk、不修改 child PC/SP。第二次交还 OS 终止。 */
            }
            break;
        }
        case EXIT_PROCESS_DEBUG_EVENT:
            o.exit_code=ev.u.ExitProcess.dwExitCode;
            o.exit_event_seen=1; o.exit_event_ticks=GetTickCount64();
            printf("FRD1 exit_event case=%s pid=%lu tid=%lu event=%lu code=%08lx tick=%" PRIu64
                   " terminal=0\n",children[id].name,(unsigned long)ev.dwProcessId,
                   (unsigned long)ev.dwThreadId,(unsigned long)ev.dwDebugEventCode,
                   (unsigned long)o.exit_code,(uint64_t)o.exit_event_ticks);
            break;
        case RIP_EVENT:
            printf("FRD1 debug_rip case=%s pid=%lu tid=%lu event=%lu error=%08lx type=%lu classification=failed\n",
                   children[id].name,(unsigned long)ev.dwProcessId,(unsigned long)ev.dwThreadId,
                   (unsigned long)ev.dwDebugEventCode,(unsigned long)ev.u.RipInfo.dwError,
                   (unsigned long)ev.u.RipInfo.dwType);
            infrastructure=fail("debug_rip_event"); /* warning 也永久失败，不当 probe exception。 */
            break;
        default:
            printf("FRD1 debug_unknown case=%s pid=%lu tid=%lu event=%lu classification=failed\n",
                   children[id].name,(unsigned long)ev.dwProcessId,(unsigned long)ev.dwThreadId,
                   (unsigned long)ev.dwDebugEventCode);
            infrastructure=fail("unknown_debug_event");
            break;
        }
        {
            BOOL continued=ContinueDebugEvent(ev.dwProcessId,ev.dwThreadId,continuation);
            DWORD continue_error=continued ? ERROR_SUCCESS:GetLastError();
            if(ev.dwDebugEventCode==EXIT_PROCESS_DEBUG_EVENT) {
                o.exit_continue_attempted=1; o.exit_continue_ok=continued!=FALSE;
                o.exit_continue_result=continued;
                o.exit_continue_error=continue_error;
            }
            printf("FRD1 debug_continue case=%s pid=%lu tid=%lu event=%lu status=%08lx result=%d ok=%d error_valid=%d error=%08lx\n",
                   children[id].name,(unsigned long)ev.dwProcessId,(unsigned long)ev.dwThreadId,
                   (unsigned long)ev.dwDebugEventCode,(unsigned long)continuation,(int)continued,continued!=FALSE,
                   continued==FALSE,(unsigned long)continue_error);
            if(!continued) {
                infrastructure=FAILED; break; /* 保留 terminal0，先保存 raw facts 再失败退出。 */
            }
        }
    }
    if(o.exit_event_seen && o.exit_continue_ok && wait_process) {
        o.process_wait_attempted=1;
        o.process_wait_result=WaitForSingleObject(wait_process,1000);
        o.process_wait_error=o.process_wait_result==WAIT_FAILED ? GetLastError():ERROR_SUCCESS;
        if(o.process_wait_result==WAIT_OBJECT_0) {
            o.terminal=1; o.terminal_ticks=GetTickCount64();
        } else infrastructure=FAILED;
    }
    if(drain(stdout_read,&out) || drain(stderr_read,&err)) infrastructure=FAILED;
    if(raw_stream(children[id].name,"stdout",&out) || raw_stream(children[id].name,"stderr",&err))
        infrastructure=FAILED;
    printf("FRD1 child_result case=%s ready=%d go=%d terminal=%d first=%d second=%d code=%08lx pc=%" PRIxPTR
           " brk=%08" PRIx32 " exit=%08lx timeout=%d forced=%d signal=not-applicable-win32\n",
           children[id].name,o.ready,o.go,o.terminal,o.first,o.second,(unsigned long)o.exception_code,
           o.exception_pc,o.instruction,(unsigned long)o.exit_code,o.timeout,o.forced);
    printf("FRD1 child_terminal case=%s exit_event_seen=%d exit_event_code=%08lx exit_continue_attempted=%d"
           " exit_continue_result=%d exit_continue_ok=%d exit_continue_error_valid=%d exit_continue_error=%08lx wait_attempted=%d"
           " wait_result_valid=%d wait_result=%08lx wait_error_valid=%d wait_error=%08lx terminal=%d\n",
           children[id].name,o.exit_event_seen,(unsigned long)o.exit_code,o.exit_continue_attempted,
           (int)o.exit_continue_result,o.exit_continue_ok,o.exit_continue_attempted && !o.exit_continue_ok,
           (unsigned long)o.exit_continue_error,o.process_wait_attempted,o.process_wait_attempted,(unsigned long)o.process_wait_result,
           o.process_wait_attempted && o.process_wait_result==WAIT_FAILED,(unsigned long)o.process_wait_error,o.terminal);
    printf("FRD1 child_time case=%s start_tick=%" PRIu64 " ready_tick=%" PRIu64
           " terminate_tick=%" PRIu64 " exit_event_tick=%" PRIu64 " terminal_tick=%" PRIu64 " designated_timeout=%d\n",
           children[id].name,(uint64_t)started,(uint64_t)o.ready_ticks,(uint64_t)o.terminate_ticks,
           (uint64_t)o.exit_event_ticks,(uint64_t)o.terminal_ticks,o.designated_timeout);
cleanup:
    frdd_release(&diagnostics);
    if(diagnostics.failed || diagnostics.overflow || o.unexpected)
        frdd_emit(&diagnostics,children[id].name,(uintptr_t)&frd_diag_reference-image_base);
    /* 错误也拥有 child；job close 保证不会遗留后台进程。 */
    if(created && !o.terminal) {
        if(!o.forced) (void)TerminateProcess(pi.hProcess,FORCED_EXIT);
        if(job_assigned && job) {CloseHandle(job); job=NULL;}
        /* 此路径只在 debug API 无法继续/等待终态时走；不可返回给下一 child。
         * 结束 parent 触发 debugger kill-on-exit，避免留下等待 debugger 的进程。 */
        fprintf(stderr,"FRD1 failed id=child_terminal_unconfirmed pid=%lu forced=1 terminal=0\n",
                (unsigned long)pi.dwProcessId);
        fflush(stdout); fflush(stderr);
        ExitProcess(FAILED);
    }
    if(job) CloseHandle(job);
    if(wait_process) CloseHandle(wait_process);
    if(pi.hThread) CloseHandle(pi.hThread);
    if(pi.hProcess) CloseHandle(pi.hProcess);
    if(stdout_read) CloseHandle(stdout_read); if(stdout_write) CloseHandle(stdout_write);
    if(stderr_read) CloseHandle(stderr_read); if(stderr_write) CloseHandle(stderr_write);
    if(go_read) CloseHandle(go_read); if(go_write) CloseHandle(go_write);
    if(!created || infrastructure || !o.ready || !o.go || !o.terminal || o.unexpected || diagnostics.failed || diagnostics.overflow) return FAILED;
    if(id==CHILD_PRESERVE) {
        if(o.exit_code || o.forced || o.first || o.second ||
           !strstr((const char *)out.bytes,"FRD1 result case=preserve outcome=normal-control\n")) return FAILED;
        return ABI_ONLY;
    }
    if(strstr((const char *)out.bytes,"FRD1 returned") || strstr((const char *)out.bytes,"FRD1 result")) return FAILED;
    if(id==CHILD_NO_RETURN) {
        if(!o.timeout || !o.designated_timeout || !o.forced || o.first || o.second ||
           o.exit_code!=FORCED_EXIT) return FAILED;
        printf("FRD1 isolated case=no_return outcome=expected-bounded-timeout termination=parent-forced\n");
        return ABI_ONLY;
    }
    return !o.forced && !o.timeout && o.first && o.second && o.exit_code==EXCEPTION_BREAKPOINT ? ABI_ONLY:FAILED;
}
static int isolated_suite(void)
{
    unsigned i;
    for(i=0;i<CHILD_COUNT;++i) if(isolated_child((child_id)i)) return fail("isolated_contract");
    return ABI_ONLY;
}
static int decimal(const char *s,unsigned *out)
{
    unsigned v=0;
    if(!s || !*s) return 0;
    for(;*s;++s) {if(*s<'0' || *s>'9' || v>100) return 0; v=v*10+(unsigned)(*s-'0');}
    *out=v; return 1;
}
int main(int argc,char **argv)
{
    const char *suite;
    unsigned expected_variant;
    int result;
    SetErrorMode(SEM_FAILCRITICALERRORS|SEM_NOGPFAULTERRORBOX|SEM_NOOPENFILEERRORBOX);
    if(argc==4 && !strcmp(argv[1],"--child")) {
        unsigned id;
        if(!decimal(argv[2],&id) || id>=CHILD_COUNT || !decimal(argv[3],&expected_variant) ||
           expected_variant!=FRD_CONTRACT_VARIANT) return REFUSED;
        if(native_admission() || linked_admission()) return REFUSED;
        return child_main((child_id)id);
    }
    if(argc!=5 || strcmp(argv[1],"--suite") || strcmp(argv[3],"--expect-variant") ||
       !decimal(argv[4],&expected_variant) || expected_variant!=FRD_CONTRACT_VARIANT) {
        fprintf(stderr,"用法: abi-contract.exe --suite register|unwind|isolated|all --expect-variant 0..4\n");
        return REFUSED;
    }
    suite=argv[2];
    if(strcmp(suite,"register") && strcmp(suite,"unwind") && strcmp(suite,"isolated") && strcmp(suite,"all"))
        return REFUSED;
    if(native_admission() || linked_admission()) return REFUSED;
    /* 所有 suite 先正常 call/return 正控制；任何坏 metadata 都不能冒充此检查。 */
    result=masks("baseline_return",frd_h264_dc_preserve,frd_h264_dc_check_abi,0);
    if(!result && (!strcmp(suite,"register") || !strcmp(suite,"all"))) result=register_suite();
    if(!result && (!strcmp(suite,"unwind") || !strcmp(suite,"all"))) {
        result=synthetic_suite(); if(!result) result=live_walk_suite();
    }
    if(!result && (!strcmp(suite,"isolated") || !strcmp(suite,"all"))) result=isolated_suite();
    printf("FRD1 final suite=%s variant=%d status=%s shipping_kernel=unimplemented qmul=unimplemented qualification=OPEN\n",
           suite,FRD_CONTRACT_VARIANT,result ? "failed":"ABI-contract-only/nativehelper-control");
    return result;
}
