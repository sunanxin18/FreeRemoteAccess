/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 独立 POSIX 契约 executable，不链接核、不改变数值 case/RNG。
 */
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

static int map_calls, protect_calls, unmap_calls;
static int fail_map, fail_protect_at, fail_unmap, fake_page, fake_clock;
static long page_value;
static struct timespec clock_value;
static void *last_base, *last_protect;
static size_t last_mapping_size, last_protect_size;
static void *live_base[4];
static size_t live_size[4];
static int last_prot;
static void ensure(int ok, const char *what) {
    if (!ok) { fprintf(stderr,"FAIL platform contract %s\n",what); exit(1); }
}
static long contract_sysconf(int name) {
    ensure(name == _SC_PAGESIZE,"sysconf selector");
    if (fake_page) { errno=EIO; return page_value; }
    return sysconf(name);
}
static void *contract_mmap(void *p,size_t n,int prot,int flags,int fd,off_t offset) {
    map_calls++;
    ensure(!p && prot==PROT_NONE && flags==(MAP_PRIVATE|MAP_ANON) && fd==-1 && !offset,
           "exact three-page mmap arguments");
    if (fail_map) { errno=ENOMEM; return MAP_FAILED; }
#ifdef FRD_DC_CONTRACT_NO_GUARDS
    prot=PROT_READ|PROT_WRITE; /* RED：两侧保护页失效，隔离访问必须抓到。 */
#endif
    last_base=mmap(p,n,prot,flags,fd,offset); last_mapping_size=n;
    if(last_base!=MAP_FAILED) {
        unsigned slot=0; while(slot<4 && live_base[slot]) slot++;
        ensure(slot<4 && last_base,"live mapping ledger capacity/address");
        live_base[slot]=last_base; live_size[slot]=n;
    }
    return last_base;
}
static int contract_mprotect(void *p,size_t n,int prot) {
    protect_calls++; last_protect=p; last_protect_size=n; last_prot=prot;
    if (protect_calls==fail_protect_at) { errno=EACCES; return -1; }
#ifdef FRD_DC_CONTRACT_NO_READ_ONLY
    if (prot==PROT_READ) prot=PROT_READ|PROT_WRITE; /* RED：输入只读失效。 */
#endif
    return mprotect(p,n,prot);
}
static int contract_munmap(void *p,size_t n) {
    unmap_calls++;
    unsigned slot=0; while(slot<4 && live_base[slot]!=p) slot++;
    ensure(slot<4 && n==live_size[slot],"release whole unique mapping");
    if (fail_unmap) { errno=EBUSY; return -1; }
    int result=munmap(p,n);
    if(!result) { live_base[slot]=NULL; live_size[slot]=0; }
    return result;
}
static int contract_clock_gettime(clockid_t clock,struct timespec *value) {
    ensure(clock==CLOCK_MONOTONIC,"absolute CLOCK_MONOTONIC source");
    if (fake_clock<0) { errno=EIO; return -1; }
    if (fake_clock) { *value=clock_value; return 0; }
    return clock_gettime(clock,value);
}
#define sysconf contract_sysconf
#define mmap contract_mmap
#define mprotect contract_mprotect
#define munmap contract_munmap
#define clock_gettime contract_clock_gettime
#include "h264-luma-dc-platform.h"
#undef sysconf
#undef mmap
#undef mprotect
#undef munmap
#undef clock_gettime

static void reset(void) {
    for(unsigned i=0;i<4;i++) ensure(!live_base[i],"previous mappings released");
    map_calls=protect_calls=unmap_calls=0;
    fail_map=fail_protect_at=fail_unmap=fake_page=fake_clock=0;
}
static int same_owner(frd_dc_guard a,frd_dc_guard b) {
    return a.base==b.base && a.page==b.page && a.page_size==b.page_size &&
           a.mapping_size==b.mapping_size;
}
static void ownership(void) {
    frd_dc_guard g={0}; reset();
    ensure(!frd_dc_guard_destroy(&g) && !unmap_calls,"zero owner no-op");
    ensure(frd_dc_guard_create(NULL)==-1 && !map_calls,"NULL owner rejected");
    g.page_size=1; frd_dc_guard saved=g;
    ensure(frd_dc_guard_create(&g)==-1 && same_owner(g,saved) && !map_calls,"dirty owner retained");
    ensure(frd_dc_guard_destroy(&g)==-1 && !unmap_calls,"invalid destroy rejected");
    g=(frd_dc_guard){0}; fake_page=1;
    const long invalid_pages[]={-1,0,481,LONG_MAX};
    for(unsigned i=0;i<sizeof invalid_pages/sizeof *invalid_pages;i++) {
        page_value=invalid_pages[i];
        ensure(frd_dc_guard_create(&g)==-1 && frd_dc_guard_zero(&g) && !map_calls,
               "negative/zero/small/overflow page rejected before mmap");
    }
    reset(); fail_map=1;
    ensure(frd_dc_guard_create(&g)==-1 && frd_dc_guard_zero(&g) && !unmap_calls,"mmap failure zero owner");
    reset(); fail_protect_at=1;
    ensure(frd_dc_guard_create(&g)==-1 && frd_dc_guard_zero(&g) && unmap_calls==1,
           "initial protection failure releases once");
    reset(); fail_protect_at=1; fail_unmap=1;
    ensure(frd_dc_guard_create(&g)==-1 && frd_dc_guard_valid(&g) && unmap_calls==1,
           "failed cleanup retains unique owner and fails create");
    saved=g; fail_unmap=0;
    ensure(!frd_dc_guard_destroy(&g) && unmap_calls==2 && frd_dc_guard_zero(&g),"retained owner cleanup");
    reset(); ensure(!frd_dc_guard_create(&g),"create success"); saved=g;
    ensure(g.base==last_base && g.mapping_size==last_mapping_size &&
           g.mapping_size==g.page_size*3 && g.page==g.base+g.page_size &&
           last_protect==g.page && last_protect_size==g.page_size &&
           last_prot==(PROT_READ|PROT_WRITE),"middle page only accessible");
    ensure(frd_dc_guard_access(&g,(enum frd_dc_access)99)==-1 && protect_calls==1,
           "invalid access mode no syscall");
    fail_protect_at=2;
    ensure(frd_dc_guard_access(&g,FRD_DC_READ_ONLY)==-1 && same_owner(g,saved),"read-only failure owner retained");
    fail_protect_at=0;
    ensure(!frd_dc_guard_access(&g,FRD_DC_READ_ONLY) && last_protect==g.page &&
           last_protect_size==g.page_size && last_prot==PROT_READ,"read-only middle page");
    fail_protect_at=protect_calls+1;
    ensure(frd_dc_guard_access(&g,FRD_DC_READ_WRITE)==-1 && same_owner(g,saved),"restore failure owner retained");
    fail_protect_at=0;
    ensure(!frd_dc_guard_access(&g,FRD_DC_READ_WRITE),"restore success");
    fail_unmap=1;
    ensure(frd_dc_guard_destroy(&g)==-1 && same_owner(g,saved),"release failure retains owner");
    fail_unmap=0; ensure(!frd_dc_guard_destroy(&g) && frd_dc_guard_zero(&g),"release success zeros owner");
    int released=unmap_calls;
    ensure(!frd_dc_guard_destroy(&g) && unmap_calls==released,"no double release");
    /* 第二个 buffer 创建失败仍归还第一个；与共享 C 的 cleanup 顺序一致。 */
    reset(); frd_dc_guard output={0};
    ensure(!frd_dc_guard_create(&g),"first buffer"); fail_map=1;
    ensure(frd_dc_guard_create(&output)==-1,"second buffer failure");
    ensure(!frd_dc_guard_destroy(&g) && !frd_dc_guard_destroy(&output) && unmap_calls==1,
           "second failure releases first once");
    reset(); ensure(!frd_dc_guard_create(&g) && !frd_dc_guard_create(&output),"two independent owners");
    uintptr_t a=(uintptr_t)g.base,b=(uintptr_t)output.base;
    ensure(a+g.mapping_size<=b || b+output.mapping_size<=a,"unique nonoverlapping mappings");
    ensure(!frd_dc_guard_destroy(&g) && !frd_dc_guard_destroy(&output) && unmap_calls==2,
           "two owners release each base once");
    puts("ownership: success, zero/dirty owner, page sizes, mmap/protect/restore/release failures");
}
static void access_child(volatile unsigned char *p,int write_access,int fault,const char *id) {
    int ready[2]; ensure(!pipe(ready),"child ready pipe");
    pid_t child=fork(); ensure(child>=0,"fork");
    if(!child) {
        close(ready[0]); struct rlimit limit={0,0};
        if(setrlimit(RLIMIT_CORE,&limit)) _exit(2);
        alarm(5); unsigned char marker=0x73;
        if(write(ready[1],&marker,1)!=1) _exit(2);
        close(ready[1]);
        if(write_access) *p=0x35; else { volatile unsigned char value=*p; (void)value; }
        _exit(0);
    }
    close(ready[1]); unsigned char marker=0; ssize_t count;
    do { count=read(ready[0],&marker,1); } while(count<0 && errno==EINTR);
    close(ready[0]); int status; pid_t waited;
    do { waited=waitpid(child,&status,0); } while(waited<0 && errno==EINTR);
    ensure(count==1 && marker==0x73 && waited==child,"child ready and reaped");
    int ok=fault ? WIFSIGNALED(status) && (WTERMSIG(status)==SIGSEGV || WTERMSIG(status)==SIGBUS)
                 : WIFEXITED(status) && WEXITSTATUS(status)==0;
    ensure(ok,id);
}
static void guarded_access(void) {
    frd_dc_guard g={0}; reset(); ensure(!frd_dc_guard_create(&g),"real guard create");
    volatile unsigned char *input=g.page;
    memset(g.page,0x72,g.page_size);
    ensure(!frd_dc_guard_access(&g,FRD_DC_READ_ONLY),"real read-only");
    access_child(input,0,0,"input start legal read");
    access_child(input+31,0,0,"input last legal read");
    access_child(input-1,0,1,"input start guard");
    input=g.page+g.page_size-32;
    access_child(input,0,0,"input end legal read");
    access_child(input+31,0,0,"input end last legal read");
    access_child(input+32,0,1,"input end guard");
    access_child(input,1,1,"read-only input write");
    ensure(!frd_dc_guard_access(&g,FRD_DC_READ_WRITE),"real restore");
    volatile unsigned char *output=g.page;
    access_child(output,1,0,"output start legal write");
    access_child(output+481,1,0,"output last legal write");
    access_child(output-1,1,1,"output start guard");
    output=g.page+g.page_size-482;
    access_child(output,1,0,"output end legal write");
    access_child(output+481,1,0,"output end last legal write");
    access_child(output+482,1,1,"output end guard");
    ensure(!frd_dc_guard_destroy(&g),"real guard cleanup");
    puts("guarded access: legal endpoints, input32/output482 both edges, read-only child faults");
}
static void failed_clock(const char *id) {
    uint64_t value=UINT64_C(0xabcdef0123456789);
    ensure(frd_dc_clock_ns(&value)==-1 && value==UINT64_C(0xabcdef0123456789),id);
}
static void clocks(void) {
    reset(); fake_clock=-1; failed_clock("clock syscall failure preserves output");
    fake_clock=1; clock_value=(struct timespec){-1,0}; failed_clock("negative seconds");
    clock_value=(struct timespec){0,-1}; failed_clock("negative nanoseconds");
    clock_value=(struct timespec){0,1000000000L}; failed_clock("out-of-range nanoseconds");
    clock_value=(struct timespec){123,456}; uint64_t value=0;
    ensure(!frd_dc_clock_ns(&value) && value==UINT64_C(123000000456),"exact absolute conversion");
    if(sizeof(time_t)>=8) {
        clock_value=(struct timespec){(time_t)UINT64_C(18446744074),0}; failed_clock("seconds multiplication overflow");
        clock_value=(struct timespec){(time_t)UINT64_C(18446744073),709551616}; failed_clock("final addition overflow");
        clock_value.tv_nsec=709551615;
        ensure(!frd_dc_clock_ns(&value) && value==UINT64_MAX,"exact uint64 upper boundary");
    }
    ensure(frd_dc_clock_ns(NULL)==-1,"NULL clock output");
    fake_clock=0; struct timespec before,after;
    ensure(!clock_gettime(CLOCK_MONOTONIC,&before) && !frd_dc_clock_ns(&value) &&
           !clock_gettime(CLOCK_MONOTONIC,&after),"real CLOCK_MONOTONIC bracket");
    uint64_t lo=(uint64_t)before.tv_sec*UINT64_C(1000000000)+(uint64_t)before.tv_nsec;
    uint64_t hi=(uint64_t)after.tv_sec*UINT64_C(1000000000)+(uint64_t)after.tv_nsec;
    ensure(lo<=value && value<=hi,"same absolute monotonic clock epoch");
    uint64_t next; ensure(!frd_dc_clock_ns(&next) && next>=value,"real monotonic order");
    puts("clock: syscall/range/overflow rejection, output ownership, absolute monotonic bracket");
}
int main(void) {
    ownership(); guarded_access(); clocks();
    puts("PASS POSIX private platform contract (no kernel arithmetic cases)");
    return 0;
}
