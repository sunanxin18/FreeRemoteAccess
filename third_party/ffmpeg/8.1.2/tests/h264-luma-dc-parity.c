/* SPDX-License-Identifier: LGPL-2.1-or-later
 * FreeRemoteDesk 私有 FFmpeg 8.1.2 核验证；不得加入产品库或 HAP。
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include "h264-luma-dc-platform.h"
#include "libavcodec/h264dsp.h"
#include "libavutil/cpu.h"

typedef void (*kernel)(int16_t *, int16_t *, int);
void ff_h264_luma_dc_dequant_idct_8_c(int16_t *, int16_t *, int);
#ifdef FRD_PRISTINE_RED
/* RED 仅提供永不调用的私有标记，实际未修改 dispatcher 必须不能选择它。 */
static void ff_h264_luma_dc_dequant_idct_neon(int16_t *a, int16_t *b, int q) {
    (void)a; (void)b; (void)q; abort();
}
#else
void ff_h264_luma_dc_dequant_idct_neon(int16_t *, int16_t *, int);
#endif
void ff_h264dsp_init_aarch64_baseline(H264DSPContext *, int, int);
int frd_h264_dc_check_abi(int16_t *, int16_t *, int, kernel);
static kernel neon;
static uint64_t cases;
static uint32_t rng = 0x4c5a2026;
static const unsigned offsets[4] = {0,32,128,160};
static void require(int ok, const char *what) {
    if (!ok) { fprintf(stderr,"FAIL %s case=%llu seed_state=%08x\n",what,
                         (unsigned long long)cases,rng); exit(1); }
}
static uint32_t random32(void) {
    rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; return rng;
}
/* 转换仅使用目标类型可表示值；不依赖有符号右移/越界 cast。 */
static int16_t signed16(uint32_t x) {
    int32_t y = x & 65535; return (int16_t)(y >= 32768 ? y - 65536 : y);
}
static int signed32(uint32_t x) {
    return x <= INT32_MAX ? (int)x : (int)((int64_t)x - INT64_C(4294967296));
}
static int16_t dequant(int32_t h, int q) {
    uint32_t u = (uint32_t)h * (uint32_t)q + UINT32_C(128);
    int64_t s = u <= INT32_MAX ? (int64_t)u : (int64_t)u - INT64_C(4294967296);
    int64_t shift = s >= 0 ? s / 256 : -((-s + 255) / 256);
    return signed16((uint32_t)shift);
}
static void normalized(int16_t *out, int16_t *in, int q) {
    int32_t t[16];
    for (int r=0;r<4;r++) {
        int32_t a=(int32_t)in[4*r]+in[4*r+1], b=(int32_t)in[4*r]-in[4*r+1];
        int32_t c=(int32_t)in[4*r+2]-in[4*r+3], d=(int32_t)in[4*r+2]+in[4*r+3];
        t[4*r]=a+d; t[4*r+1]=a-d; t[4*r+2]=b-c; t[4*r+3]=b+c;
    }
    for (int i=0;i<4;i++) {
        int32_t a=t[i]+t[8+i], b=t[i]-t[8+i], c=t[4+i]-t[12+i], d=t[4+i]+t[12+i];
        out[offsets[i]]=dequant(a+d,q); out[offsets[i]+16]=dequant(b+c,q);
        out[offsets[i]+64]=dequant(b-c,q); out[offsets[i]+80]=dequant(a-d,q);
    }
}
static void fill(int16_t *out, size_t n) {
    for(size_t i=0;i<n;i++) out[i]=signed16(0xace1u+(uint32_t)i*109u);
}
static void check(int16_t *in, int q) {
    _Alignas(16) int16_t a[256], b[256], c[256], saved[16];
    fill(a,256); memcpy(b,a,sizeof a); memcpy(c,a,sizeof a); memcpy(saved,in,sizeof saved);
    normalized(a,in,q); ff_h264_luma_dc_dequant_idct_8_c(b,in,q);
    require(!memcmp(a,b,sizeof a),"original C vs normalized oracle");
    neon(c,in,q);
    require(!memcmp(a,c,sizeof a),"NEON vs original C including 240 AC cells");
    require(!memcmp(saved,in,sizeof saved),"nonalias input unchanged"); cases++;
}
static void dispatch(void) {
    int saved=av_get_cpu_flags();
    neon=ff_h264_luma_dc_dequant_idct_neon;
    const int depths[]={8,9,10,12,14};
    for(int enabled=0;enabled<2;enabled++) {
        av_force_cpu_flags(enabled ? AV_CPU_FLAG_NEON : 0);
        for(unsigned d=0;d<sizeof depths/sizeof *depths;d++) for(int chroma=1;chroma<=3;chroma++) {
            H264DSPContext a,b; memset(&a,0,sizeof a); memset(&b,0,sizeof b);
            ff_h264dsp_init(&a,depths[d],chroma);
            av_force_cpu_flags(0); ff_h264dsp_init(&b,depths[d],chroma);
            av_force_cpu_flags(enabled ? AV_CPU_FLAG_NEON : 0);
            ff_h264dsp_init_aarch64_baseline(&b,depths[d],chroma);
            if(enabled && depths[d]==8) {
                require(a.luma_dc_dequant_idct==neon,"h264_luma_dc_selects_neon_8bit");
                a.luma_dc_dequant_idct=b.luma_dc_dequant_idct;
            } else if(depths[d]==8) require(a.luma_dc_dequant_idct==ff_h264_luma_dc_dequant_idct_8_c,"CPU-off C dispatch");
            require(!memcmp(&a,&b,sizeof a),"all other slots and depths match pristine baseline");
        }
    }
    av_force_cpu_flags(saved);
    puts("dispatch: 30 contexts, CPU off/on x depths 8/9/10/12/14 x chroma 1/2/3");
}
_Alignas(16) static int16_t vectors[128][16];
static unsigned nvec;
static void add_vector(int16_t *v) { require(nvec<128,"vector capacity"); memcpy(vectors[nvec++],v,32); }
static void boundaries(void) {
    _Alignas(16) int16_t v[16]={0}; add_vector(v);
    const int amps[]={1,-1,32767,-32768,129,-129};
    for(unsigned a=0;a<sizeof amps/sizeof *amps;a++) for(int i=0;i<16;i++) {
        memset(v,0,sizeof v); v[i]=amps[a]; add_vector(v);
    }
    for(int p=0;p<6;p++) {
        for(int i=0;i<16;i++) {
            int flip=p==0 ? 0 : p==1 ? 1 : p==2 ? i%2 : p==3 ? i/4%2 : p==4 ? (i/4+i%4)%2 : i%3==0;
            v[i]=flip ? -32768 : 32767;
        } add_vector(v);
    }
    for(int axis=0;axis<2;axis++) for(int n=0;n<4;n++) for(int sign=0;sign<2;sign++) {
        memset(v,0,sizeof v); for(int i=0;i<4;i++) v[axis ? i*4+n : n*4+i]=sign ? -32768 : 32767; add_vector(v);
    }
    const int factors[]={10,11,13,14,16,18}; unsigned combinations=0; int maxq=0;
    for(int qp=0;qp<=51;qp++) for(int scale=1;scale<=255;scale++) {
        int q=(factors[qp%6]*scale) << (qp/6+2); if(q>maxq) maxq=q; combinations++;
        for(unsigned j=0;j<nvec;j++) check(vectors[j],q);
    }
    require(combinations==13260 && maxq==3655680,"legal QP/scaling coverage");
    const int robust[]={0,1,32767,32768,INT32_MIN,INT32_MAX,-1};
    for(unsigned q=0;q<sizeof robust/sizeof *robust;q++) for(unsigned j=0;j<nvec;j++) check(vectors[j],robust[q]);
    printf("boundaries: %u vectors x 13260 legal QP/scaling combinations, max qmul=%d; 7 robustness qmul values\n",nvec,maxq);
}
static void exhaustive_and_random(void) {
    _Alignas(16) int16_t v[16]; const int qs[]={1,40,32768,3655680};
    for(int i=0;i<16;i++) for(unsigned q=0;q<sizeof qs/sizeof *qs;q++) {
        memset(v,0,sizeof v);
        for(int a=-32768;a<=32767;a++) { v[i]=(int16_t)a; check(v,qs[q]); }
    }
    /* 显式覆盖 signed32 加128溢出，以及乘法/ASR/窄化边界。 */
    const int edgeq[]={-129,-128,127,128,INT32_MAX-127,INT32_MAX,INT32_MIN,65535,65536};
    memset(v,0,sizeof v); v[0]=1;
    for(unsigned q=0;q<sizeof edgeq/sizeof *edgeq;q++) check(v,edgeq[q]);
    for(int i=0;i<200000;i++) {
        for(int j=0;j<16;j++) v[j]=signed16(random32());
        check(v,signed32(random32()));
    }
    puts("exhaustive: 16 positions x 65536 amplitudes x 4 qmul; random: 200000 seed=0x4c5a2026; explicit rounding/wrap cases: 9");
}
static void memory_cases(void) {
    _Alignas(16) unsigned char src[64], saved[64], a[576], b[576];
    for(int align=0;align<16;align+=2) for(unsigned j=0;j<nvec;j++) {
        memset(src,0xa7,sizeof src); memcpy(src+16,vectors[j],32); memcpy(saved,src,sizeof src);
        fill((int16_t*)a,sizeof a/2); memcpy(b,a,sizeof a);
        ff_h264_luma_dc_dequant_idct_8_c((int16_t*)(a+32+align),(int16_t*)(src+16),3655680);
        neon((int16_t*)(b+32+align),(int16_t*)(src+16),3655680);
        require(!memcmp(a,b,sizeof a),"output align/redzones/scatter");
        require(!memcmp(src,saved,sizeof src),"input redzones"); cases++;
    }
    const int alias[]={0,16,128,240};
    for(unsigned k=0;k<4;k++) for(unsigned j=0;j<nvec;j++) {
        fill((int16_t*)a,sizeof a/2); memcpy(a+32+2*alias[k],vectors[j],32); memcpy(b,a,sizeof a);
        ff_h264_luma_dc_dequant_idct_8_c((int16_t*)(a+32),(int16_t*)(a+32+2*alias[k]),3655680);
        neon((int16_t*)(b+32),(int16_t*)(b+32+2*alias[k]),3655680);
        require(!memcmp(a,b,sizeof a),"alias complete memory image"); cases++;
    }
    frd_dc_guard input={0}, output={0}; const char *failure=NULL;
    if(frd_dc_guard_create(&input) || frd_dc_guard_create(&output)) { failure="guard create"; goto guard_cleanup; }
    for(int edge=0;edge<2;edge++) for(int outedge=0;outedge<2;outedge++) {
        int16_t *in=(int16_t*)(input.page+(edge ? input.page_size-32 : 0));
        int16_t *out=(int16_t*)(output.page+(outedge ? output.page_size-482 : 0));
        memcpy(in,vectors[5],32); fill(out,241);
        _Alignas(16) int16_t expected[256]; fill(expected,256); normalized(expected,in,3655680);
        if(frd_dc_guard_access(&input,FRD_DC_READ_ONLY)) { failure="input read-only"; goto guard_cleanup; }
        neon(out,in,3655680);
        if(memcmp(out,expected,482)) { failure="guard page parity"; goto guard_cleanup; }
        if(frd_dc_guard_access(&input,FRD_DC_READ_WRITE)) { failure="restore input access"; goto guard_cleanup; } cases++;
    }
guard_cleanup:
    { int input_error=frd_dc_guard_destroy(&input), output_error=frd_dc_guard_destroy(&output);
      require(!failure && !input_error && !output_error,failure ? failure : "guard cleanup"); }
    _Alignas(16) int16_t out[256];
    require(!frd_h264_dc_check_abi(out,vectors[2],3655680,neon),"callee saved GPR/SIMD, x18 and SP");
    puts("memory: 8 output alignments; alias indices 0/16/128/240; read-only input and input/output start/end guard pages; AArch64 ABI sentinels");
}
static uint64_t ns(void) { uint64_t value; require(!frd_dc_clock_ns(&value),"clock"); return value; }
static int cmp_double(const void *a,const void *b) { double x=*(const double*)a,y=*(const double*)b; return (x>y)-(x<y); }
static void benchmark(void) {
    enum { POOL=256, BATCHES=31, CALLS=262144, WARMUP=65536 };
    _Alignas(16) int16_t inputs[POOL][16],outputs[POOL][256]; int qs[POOL];
    double results[2][BATCHES]; uint64_t sums[2]={0,0};
    kernel f[2]={ff_h264_luma_dc_dequant_idct_8_c,neon};
    for(int i=0;i<POOL;i++) { qs[i]=(10+(i%9))*((i%255)+1)*1024; for(int j=0;j<16;j++) inputs[i][j]=signed16(random32()); }
    for(int k=0;k<2;k++) for(int i=0;i<WARMUP;i++) f[k](outputs[i%POOL],inputs[i%POOL],qs[i%POOL]);
    /* 交替先后顺序降低温度/频率漂移偏差；校验不在计时区。 */
    for(int batch=0;batch<BATCHES;batch++) for(int order=0;order<2;order++) {
        int k=order^(batch&1); memset(outputs,0,sizeof outputs); uint64_t start=ns();
        for(int i=0;i<CALLS;i++) f[k](outputs[i%POOL],inputs[i%POOL],qs[i%POOL]);
        results[k][batch]=(double)(ns()-start)/CALLS;
        uint64_t sum=0; for(int i=0;i<POOL;i++) for(int j=0;j<256;j++) sum=sum*UINT64_C(1099511628211)+(uint16_t)outputs[i][j];
        if(!batch) sums[k]=sum; else require(sums[k]==sum,"benchmark stable checksum");
    }
    require(sums[0]==sums[1],"benchmark C/NEON checksum");
    for(int k=0;k<2;k++) { qsort(results[k],BATCHES,sizeof(double),cmp_double);
        printf("benchmark %s: warmup=%d measured_calls=%d batches=%d ns/call median=%.3f p95=%.3f max=%.3f checksum=%016llx\n",k ? "NEON":"C",WARMUP,CALLS*BATCHES,BATCHES,results[k][15],results[k][29],results[k][30],(unsigned long long)sums[k]); }
}
int main(int argc,char **argv) {
    require(sizeof(int)==4 && sizeof(int16_t)==2,"32-bit int ABI"); dispatch();
    if(argc==2 && !strcmp(argv[1],"--benchmark")) benchmark();
    else { require(argc==1,"usage: [--benchmark]"); boundaries(); exhaustive_and_random(); memory_cases(); printf("PASS parity cases=%llu\n",(unsigned long long)cases); }
    return 0;
}
