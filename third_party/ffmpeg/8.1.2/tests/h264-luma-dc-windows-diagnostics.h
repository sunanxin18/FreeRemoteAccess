/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有失败诊断。只读取停止的 child；不改变 context/Continue/终止策略。
 */
#ifndef FRD_WINDOWS_DIAGNOSTICS_H
#define FRD_WINDOWS_DIAGNOSTICS_H
#include <bcrypt.h>
#include <stdarg.h>
#define FRDD_FLAGS (CONTEXT_CONTROL|CONTEXT_INTEGER|CONTEXT_FLOATING_POINT)
#define FRDD_EVENTS 8u
#define FRDD_MODULES 32u
#define FRDD_HASH_MAX (32u*1024u*1024u)
typedef struct {
    CONTEXT context;
    uint64_t magic;
    DWORD pid,tid,case_id,complete;
} frdd_reference;
/* 独立数据符号由固定 compile gate 认证 RVA/section/range。 */
frdd_reference frd_diag_reference = {.magic=UINT64_C(0x4652444449414731)};
_Static_assert(_Alignof(CONTEXT)>=16,"aligned ARM64 control/integer/floating context");
_Static_assert(sizeof(CONTEXT)==912,"ARM64 CONTEXT wire size");
_Static_assert(sizeof(frdd_reference)==944,"private reference size");
typedef struct { int attempted,result; DWORD error; SIZE_T got; } frdd_api;
typedef struct {
    uintptr_t base; DWORD load_seq,unload_seq,pid,tid,size; WORD machine;
    HANDLE file; frdd_api header,duplicate,event_close,file_close;
} frdd_module;
typedef struct {
    DWORD seq,pid,tid,code,first; uintptr_t pc;
    frdd_api open,owner,thread_id,context,close,reference;
    DWORD actual_pid,actual_tid;
    CONTEXT cpu; frdd_reference baseline;
    int reference_valid;
    char av_text[16384];
} frdd_event;
typedef struct {
    DWORD seq,pid; unsigned count,modules; int failed,overflow;
    ULONGLONG diagnostic_ticks;
    frdd_event events[FRDD_EVENTS]; frdd_module images[FRDD_MODULES];
} frdd_state;
/* AV 记录在停止点生成至私有有界缓存，进程退出后才输出失败块。 */
static char *frdd_sink;
static size_t frdd_sink_used,frdd_sink_size;
static int frdd_sink_failed;
static void frdd_printf(const char *format,...)
{
    va_list args; int n;
    va_start(args,format);
    if(frdd_sink) {
        n=vsnprintf(frdd_sink+frdd_sink_used,frdd_sink_size-frdd_sink_used,format,args);
        if(n<0 || (size_t)n>=frdd_sink_size-frdd_sink_used) frdd_sink_failed=1;
        else frdd_sink_used+=(size_t)n;
    } else vfprintf(stderr,format,args);
    va_end(args);
}
static void frdd_puts(const char *text) {frdd_printf("%s",text);}
static void frdd_putc(int c) {frdd_printf("%c",c);}
static void frdd_cache_av(frdd_state *d,HANDLE process,frdd_event *e);
static void frdd_bool(frdd_api *a,BOOL result)
{
    a->attempted=1; a->result=(int)result;
    a->error=result ? ERROR_SUCCESS:GetLastError();
}
static int frdd_read(HANDLE process,uintptr_t address,void *bytes,SIZE_T n,frdd_api *a)
{
    BOOL result=ReadProcessMemory(process,(const void *)address,bytes,n,&a->got);
    frdd_bool(a,result);
    return result!=FALSE && a->got==n;
}
static int frdd_header(const unsigned char *raw,DWORD *size,WORD *machine,
                       const IMAGE_NT_HEADERS64 **nt)
{
    const IMAGE_DOS_HEADER *dos=(const IMAGE_DOS_HEADER *)raw;
    if(dos->e_magic!=IMAGE_DOS_SIGNATURE || dos->e_lfanew<64 || dos->e_lfanew>2048 || (dos->e_lfanew&7)) return 0;
    *nt=(const IMAGE_NT_HEADERS64 *)(raw+dos->e_lfanew);
    if((*nt)->Signature!=IMAGE_NT_SIGNATURE || !(*nt)->FileHeader.Machine ||
       (*nt)->OptionalHeader.Magic!=IMAGE_NT_OPTIONAL_HDR64_MAGIC ||
       (*nt)->FileHeader.SizeOfOptionalHeader!=sizeof(IMAGE_OPTIONAL_HEADER64) ||
       !(*nt)->FileHeader.NumberOfSections || (*nt)->FileHeader.NumberOfSections>32 ||
       (size_t)dos->e_lfanew+sizeof(**nt)+(*nt)->FileHeader.NumberOfSections*sizeof(IMAGE_SECTION_HEADER)>4096 ||
       (*nt)->OptionalHeader.SizeOfImage<4096) return 0;
    *size=(*nt)->OptionalHeader.SizeOfImage; *machine=(*nt)->FileHeader.Machine;
    return 1;
}
static void frdd_module_load(frdd_state *d,HANDLE process,const DEBUG_EVENT *ev,
                            uintptr_t base,HANDLE event_file)
{
    frdd_module *m; _Alignas(16) unsigned char raw[4096]; const IMAGE_NT_HEADERS64 *nt;
    if(d->modules==FRDD_MODULES) {
        d->overflow=1; d->failed=1;
        if(event_file) CloseHandle(event_file);
        return;
    }
    m=&d->images[d->modules++]; m->base=base; m->load_seq=d->seq;
    m->pid=ev->dwProcessId; m->tid=ev->dwThreadId;
    if(event_file) {
        BOOL copied=DuplicateHandle(GetCurrentProcess(),event_file,GetCurrentProcess(),&m->file,
                                   0,FALSE,DUPLICATE_SAME_ACCESS);
        frdd_bool(&m->duplicate,copied);
        if(!copied) d->failed=1;
        /* 原始 debug event hFile 每条恰关闭一次，副本独立持有。 */
        frdd_bool(&m->event_close,CloseHandle(event_file));
        if(!m->event_close.result) d->failed=1;
    }
    if(ev->dwProcessId!=d->pid || !base || base>UINTPTR_MAX-UINT32_MAX) return;
    if(!frdd_read(process,base,raw,sizeof(raw),&m->header)) d->failed=1;
    else if(!frdd_header(raw,&m->size,&m->machine,&nt)) {m->size=0; d->failed=1;}
}
static void frdd_unload(frdd_state *d,uintptr_t base)
{
    unsigned i;
    for(i=0;i<d->modules;++i) if(d->images[i].base==base && !d->images[i].unload_seq) {
        frdd_module *m=&d->images[i]; m->unload_seq=d->seq;
        if(m->file) {frdd_bool(&m->file_close,CloseHandle(m->file)); if(!m->file_close.result) d->failed=1; m->file=NULL;}
    }
}
static void frdd_capture(frdd_state *d,HANDLE process,const DEBUG_EVENT *ev,
                         uintptr_t base,uintptr_t self_base,DWORD self_size,unsigned case_id)
{
    frdd_event *e; HANDLE thread; DWORD pid,tid; uintptr_t rva;
    if(d->count==FRDD_EVENTS) {d->failed=1; d->overflow=1; return;}
    e=&d->events[d->count++]; e->seq=d->seq; e->pid=ev->dwProcessId; e->tid=ev->dwThreadId;
    e->code=ev->u.Exception.ExceptionRecord.ExceptionCode; e->first=ev->u.Exception.dwFirstChance;
    e->pc=(uintptr_t)ev->u.Exception.ExceptionRecord.ExceptionAddress;
    if(e->pid!=d->pid || !e->tid) {d->failed=1; return;}
    thread=OpenThread(THREAD_GET_CONTEXT|THREAD_QUERY_LIMITED_INFORMATION,FALSE,e->tid);
    frdd_bool(&e->open,thread!=NULL);
    if(thread) {
        pid=GetProcessIdOfThread(thread); e->owner.attempted=1;
        e->owner.result=pid!=0; e->owner.error=pid ? 0:GetLastError();
        e->actual_pid=pid;
        tid=GetThreadId(thread); e->actual_tid=tid;
        e->thread_id.attempted=1; e->thread_id.result=tid!=0;
        e->thread_id.error=tid ? 0:GetLastError();
        if(pid==e->pid && tid==e->tid) {
            e->cpu.ContextFlags=FRDD_FLAGS;
            frdd_bool(&e->context,GetThreadContext(thread,&e->cpu));
        }
        frdd_bool(&e->close,CloseHandle(thread));
    }
    if(!e->context.result || (e->cpu.ContextFlags&FRDD_FLAGS)!=FRDD_FLAGS || !e->close.result) d->failed=1;
    rva=(uintptr_t)&frd_diag_reference-self_base;
    if(rva>self_size || sizeof(e->baseline)>self_size-rva || base>UINTPTR_MAX-rva) d->failed=1;
    else if(frdd_read(process,base+rva,&e->baseline,sizeof(e->baseline),&e->reference)) {
        e->reference_valid=e->baseline.magic==UINT64_C(0x4652444449414731) &&
            e->baseline.complete==1 && e->baseline.pid==e->pid && e->baseline.tid==e->tid &&
            e->baseline.case_id==case_id && (e->baseline.context.ContextFlags&FRDD_FLAGS)==FRDD_FLAGS;
        if(!e->reference_valid) d->failed=1;
    } else d->failed=1;
    if(e->code==EXCEPTION_ACCESS_VIOLATION) frdd_cache_av(d,process,e);
}
static void frdd_api_json(const frdd_api *a)
{
    frdd_printf("{\"attempted\":%d,\"result\":%d,\"error_valid\":%d,\"error\":%lu,\"got\":%zu}",
            a->attempted,a->result,a->attempted && !a->result,(unsigned long)a->error,(size_t)a->got);
}
static void frdd_hex(const unsigned char *raw,size_t n)
{
    size_t i; frdd_putc('"'); for(i=0;i<n;++i) frdd_printf("%02x",raw[i]); frdd_putc('"');
}
static void frdd_context_json(const CONTEXT *c,int valid)
{
    unsigned i;
    if(!valid) {frdd_puts("null"); return;}
    frdd_printf("{\"flags\":%lu,\"pc\":\"%016" PRIx64 "\",\"sp\":\"%016" PRIx64
            "\",\"fp\":\"%016" PRIx64 "\",\"lr\":\"%016" PRIx64 "\",\"pstate\":%lu,\"x\":[",
            (unsigned long)c->ContextFlags,(uint64_t)c->Pc,(uint64_t)c->Sp,(uint64_t)c->Fp,
            (uint64_t)c->Lr,(unsigned long)c->Cpsr);
    for(i=0;i<29;++i) frdd_printf("%s\"%016" PRIx64 "\"",i ? ",":"",(uint64_t)c->X[i]);
    frdd_puts("],\"d8_15\":[");
    for(i=8;i<16;++i) frdd_printf("%s\"%016" PRIx64 "\"",i!=8 ? ",":"",(uint64_t)c->V[i].Low);
    frdd_puts("]}");
}
/* 所有文件操作仅来自 debug event 的 owned hFile；禁止路径 reopen。 */
static int frdd_file_read(HANDLE file,ULONGLONG offset,void *raw,DWORD n,frdd_api *seek,frdd_api *read)
{
    LARGE_INTEGER p; DWORD got=0; p.QuadPart=(LONGLONG)offset;
    frdd_bool(seek,SetFilePointerEx(file,p,NULL,FILE_BEGIN));
    if(!seek->result) return 0;
    frdd_bool(read,ReadFile(file,raw,n,&got,NULL)); read->got=got;
    return read->result && got==n;
}
static void frdd_file_id(const BY_HANDLE_FILE_INFORMATION *i,int valid)
{
    if(!valid) {frdd_puts("null"); return;}
    frdd_printf("{\"volume\":%lu,\"index\":\"%08lx%08lx\",\"size\":%" PRIu64
            ",\"write\":\"%08lx%08lx\"}",(unsigned long)i->dwVolumeSerialNumber,
            (unsigned long)i->nFileIndexHigh,(unsigned long)i->nFileIndexLow,
            ((uint64_t)i->nFileSizeHigh<<32)|i->nFileSizeLow,
            (unsigned long)i->ftLastWriteTime.dwHighDateTime,(unsigned long)i->ftLastWriteTime.dwLowDateTime);
}
/* 仅失败 AV 使用一次有界 hash/code 读取。每块检查 1s 预算；OS 同步调用延迟仍是 native 验证边界。 */
static void frdd_av_json(frdd_state *d,HANDLE process,const frdd_event *e)
{
    frdd_module *m=NULL; unsigned i,matches=0;
    _Alignas(16) unsigned char remote[256]={0},disk[256]={0},header[4096],buffer[65536],digest[32];
    wchar_t path[1024]; DWORD path_n=0,size=0; WORD machine=0; int path_valid=0;
    BY_HANDLE_FILE_INFORMATION before={0},after={0};
    frdd_api name={0},pre={0},post={0},seek={0},read={0},rpm={0};
    frdd_api header_seek={0},header_read={0},code_seek={0},code_read={0};
    BCRYPT_ALG_HANDLE alg=NULL; BCRYPT_HASH_HANDLE hash=NULL;
    NTSTATUS status=0; int hash_attempted=0,hash_ok=0,mapped=0,stable=0,matched=0;
    const char *hash_failure="not-attempted";
    NTSTATUS stages[6]={0}; unsigned stage_mask=0;
    ULONGLONG length=0,offset=0,started=GetTickCount64(); uintptr_t start=0;
    DWORD code_n=0; const IMAGE_NT_HEADERS64 *nt=NULL;
    DWORD section_rva=0,section_raw=0,section_size=0,section_flags=0; ULONGLONG code_offset=0;
    for(i=0;i<d->modules;++i) {
        frdd_module *p=&d->images[i];
        if(p->pid==e->pid && p->size && p->load_seq<e->seq &&
           (!p->unload_seq || p->unload_seq>e->seq) && e->pc>=p->base && e->pc-p->base<p->size) {m=p; ++matches;}
    }
    frdd_printf("FRDD1 {\"type\":\"av\",\"seq\":%lu,\"pid\":%lu,\"tid\":%lu,\"module\":",
            (unsigned long)e->seq,(unsigned long)e->pid,(unsigned long)e->tid);
    if(matches!=1 || !m || !m->file || m->unload_seq) {frdd_puts("null,\"reason\":\"module-or-file-unknown\"}\n"); d->failed=1; return;}
    frdd_printf("%u,\"base\":\"%016" PRIxPTR "\",\"rva\":%" PRIuPTR ",",(unsigned)(m-d->images),m->base,e->pc-m->base);
    path_n=GetFinalPathNameByHandleW(m->file,path,COUNT(path),FILE_NAME_NORMALIZED|VOLUME_NAME_DOS);
    name.attempted=1; name.result=(int)path_n;
    name.error=path_n ? 0:GetLastError();
    path_valid=path_n!=0 && path_n<COUNT(path);
    frdd_bool(&pre,GetFileInformationByHandle(m->file,&before));
    if(pre.result) length=((ULONGLONG)before.nFileSizeHigh<<32)|before.nFileSizeLow;
    if(pre.result && path_valid && length>=4096 && length<=FRDD_HASH_MAX &&
       frdd_file_read(m->file,0,header,4096,&header_seek,&header_read) && frdd_header(header,&size,&machine,&nt) && size==m->size && machine==m->machine) {
        const IMAGE_SECTION_HEADER *sections=(const IMAGE_SECTION_HEADER *)((const unsigned char *)nt+sizeof(*nt));
        DWORD rva=(DWORD)(e->pc-m->base),code_rva=rva>=64 ? rva-64:rva;
        code_n=(DWORD)(m->size-code_rva); if(code_n>256) code_n=256;
        for(i=0;i<nt->FileHeader.NumberOfSections;++i) {
            const IMAGE_SECTION_HEADER *s=&sections[i];
            if((s->Characteristics&IMAGE_SCN_MEM_EXECUTE) && code_rva>=s->VirtualAddress &&
               code_rva-s->VirtualAddress<=s->SizeOfRawData && code_n<=s->SizeOfRawData-(code_rva-s->VirtualAddress)) {
                ULONGLONG at=(ULONGLONG)s->PointerToRawData+code_rva-s->VirtualAddress;
                if(at<=length && code_n<=length-at) {
                    offset=at; code_offset=at; section_rva=s->VirtualAddress;
                    section_raw=s->PointerToRawData; section_size=s->SizeOfRawData;
                    section_flags=s->Characteristics; ++mapped;
                }
            }
        }
        if(mapped==1) {
            start=m->base+code_rva;
            if(frdd_file_read(m->file,offset,disk,code_n,&code_seek,&code_read) &&
               frdd_read(process,start,remote,code_n,&rpm)) matched=!memcmp(disk,remote,code_n);
        }
        hash_attempted=1;
        hash_failure="none"; stage_mask|=1;
        stages[0]=status=BCryptOpenAlgorithmProvider(&alg,BCRYPT_SHA256_ALGORITHM,NULL,0);
        if(status>=0) {stage_mask|=2; stages[1]=status=BCryptCreateHash(alg,&hash,NULL,0,NULL,0,0);}
        if(status<0) hash_failure="bcrypt";
        offset=0;
        while(status>=0 && offset<length) {
            DWORD n=(DWORD)(length-offset); if(n>sizeof(buffer)) n=sizeof(buffer);
            if(GetTickCount64()-started+d->diagnostic_ticks>=1000) {hash_failure="budget"; break;}
            if(!frdd_file_read(m->file,offset,buffer,n,&seek,&read)) {hash_failure="file-read"; break;}
            stage_mask|=4; stages[2]=status=BCryptHashData(hash,buffer,n,0);
            if(status<0) {hash_failure="bcrypt"; break;}
            offset+=n;
        }
        if(status>=0 && offset==length) {stage_mask|=8; stages[3]=status=BCryptFinishHash(hash,digest,sizeof(digest),0); hash_ok=status>=0; if(status<0) hash_failure="bcrypt";}
    }
    if(hash) {NTSTATUS close_status=BCryptDestroyHash(hash); stage_mask|=16; stages[4]=close_status; if(close_status<0) {status=close_status; hash_ok=0;}}
    if(alg) {NTSTATUS close_status=BCryptCloseAlgorithmProvider(alg,0); stage_mask|=32; stages[5]=close_status; if(close_status<0) {status=close_status; hash_ok=0;}}
    frdd_bool(&post,GetFileInformationByHandle(m->file,&after));
    d->diagnostic_ticks+=GetTickCount64()-started;
    stable=pre.result && post.result && before.dwVolumeSerialNumber==after.dwVolumeSerialNumber &&
        before.nFileIndexHigh==after.nFileIndexHigh && before.nFileIndexLow==after.nFileIndexLow &&
        before.nFileSizeHigh==after.nFileSizeHigh && before.nFileSizeLow==after.nFileSizeLow &&
        before.ftLastWriteTime.dwHighDateTime==after.ftLastWriteTime.dwHighDateTime &&
        before.ftLastWriteTime.dwLowDateTime==after.ftLastWriteTime.dwLowDateTime;
    frdd_puts("\"path_api\":"); frdd_api_json(&name); frdd_puts(",\"path_utf16\":");
    frdd_hex((unsigned char *)path,path_valid ? path_n*sizeof(wchar_t):0);
    frdd_printf(",\"path_valid\":%d,\"path_length\":%lu,\"file_machine\":%u,\"file_image_size\":%lu,\"file_offset\":%" PRIu64
                ",\"section_rva\":%lu,\"section_raw\":%lu,\"section_size\":%lu,\"section_flags\":%lu",
                path_valid,(unsigned long)path_n,machine,(unsigned long)size,(uint64_t)code_offset,
                (unsigned long)section_rva,(unsigned long)section_raw,(unsigned long)section_size,(unsigned long)section_flags);
    frdd_puts(",\"before_api\":"); frdd_api_json(&pre); frdd_puts(",\"before\":"); frdd_file_id(&before,pre.result);
    frdd_puts(",\"after_api\":"); frdd_api_json(&post); frdd_puts(",\"after\":"); frdd_file_id(&after,post.result);
    frdd_printf(",\"stable\":%d,\"hash_attempted\":%d,\"hash_status\":%lu,\"sha256\":",stable,hash_attempted,(unsigned long)(ULONG)status);
    if(hash_ok && stable) frdd_hex(digest,32); else frdd_puts("null");
    frdd_printf(",\"hash_failure\":\"%s\",\"hash_bytes\":%" PRIu64 ",\"hash_stages_attempted\":%u,\"hash_stages\":[",hash_failure,(uint64_t)offset,stage_mask);
    for(i=0;i<6;++i) frdd_printf("%s%lu",i ? ",":"",(unsigned long)(ULONG)stages[i]);
    frdd_puts("],\"header_seek\":"); frdd_api_json(&header_seek);
    frdd_puts(",\"header_read\":"); frdd_api_json(&header_read);
    frdd_puts(",\"code_seek\":"); frdd_api_json(&code_seek);
    frdd_puts(",\"code_read\":"); frdd_api_json(&code_read);
    frdd_printf(",\"code_start\":\"%016" PRIxPTR "\",\"code_size\":%lu,\"mapped\":%d,\"matched\":%d,\"seek\":",start,(unsigned long)code_n,mapped==1,matched);
    frdd_api_json(&seek); frdd_puts(",\"read\":"); frdd_api_json(&read); frdd_puts(",\"rpm\":"); frdd_api_json(&rpm);
    frdd_puts(",\"remote\":"); frdd_hex(remote,rpm.result && rpm.got==code_n ? code_n:0);
    frdd_puts(",\"disk\":"); frdd_hex(disk,code_read.result && code_read.got==code_n ? code_n:0);
    frdd_puts("}\n");
    if(!stable || !hash_ok || !matched || !path_valid) d->failed=1;
}
static void frdd_cache_av(frdd_state *d,HANDLE process,frdd_event *e)
{
    frdd_sink=e->av_text; frdd_sink_used=0; frdd_sink_size=sizeof(e->av_text); frdd_sink_failed=0;
    frdd_av_json(d,process,e);
    if(frdd_sink_failed) {e->av_text[0]=0; d->failed=1; d->overflow=1;}
    frdd_sink=NULL;
}
static void frdd_emit(frdd_state *d,const char *case_name,uintptr_t reference_rva)
{
    unsigned i;
    frdd_printf("FRDD1 {\"type\":\"begin\",\"version\":1,\"case\":\"%s\",\"pid\":%lu,\"events\":%u,\"modules\":%u,\"overflow\":%d,\"reference_rva\":%" PRIuPTR "}\n",
            case_name,(unsigned long)d->pid,d->count,d->modules,d->overflow,reference_rva);
    for(i=0;i<d->modules;++i) {
        frdd_module *m=&d->images[i];
        frdd_printf("FRDD1 {\"type\":\"module\",\"index\":%u,\"pid\":%lu,\"tid\":%lu,\"load_seq\":%lu,\"unload_seq\":%lu,\"base\":\"%016" PRIxPTR "\",\"size\":%lu,\"machine\":%u,\"header\":",i,(unsigned long)m->pid,(unsigned long)m->tid,(unsigned long)m->load_seq,(unsigned long)m->unload_seq,m->base,(unsigned long)m->size,m->machine);
        frdd_api_json(&m->header); frdd_puts(",\"duplicate\":"); frdd_api_json(&m->duplicate);
        frdd_puts(",\"event_close\":"); frdd_api_json(&m->event_close);
        frdd_puts(",\"file_close\":"); frdd_api_json(&m->file_close); frdd_puts("}\n");
    }
    for(i=0;i<d->count;++i) {
        frdd_event *e=&d->events[i];
        frdd_printf("FRDD1 {\"type\":\"event\",\"seq\":%lu,\"pid\":%lu,\"tid\":%lu,\"code\":%lu,\"first\":%lu,\"pc\":\"%016" PRIxPTR "\",\"open\":",(unsigned long)e->seq,(unsigned long)e->pid,(unsigned long)e->tid,(unsigned long)e->code,(unsigned long)e->first,e->pc);
        frdd_api_json(&e->open); frdd_puts(",\"owner\":"); frdd_api_json(&e->owner);
        frdd_puts(",\"thread_id\":"); frdd_api_json(&e->thread_id);
        frdd_printf(",\"actual_pid\":%lu,\"actual_tid\":%lu",(unsigned long)e->actual_pid,(unsigned long)e->actual_tid);
        frdd_puts(",\"context_api\":"); frdd_api_json(&e->context); frdd_puts(",\"close\":"); frdd_api_json(&e->close);
        frdd_puts(",\"context\":"); frdd_context_json(&e->cpu,e->context.result!=0);
        frdd_puts(",\"reference_api\":"); frdd_api_json(&e->reference);
        frdd_printf(",\"reference_valid\":%d,\"baseline\":",e->reference_valid);
        frdd_context_json(&e->baseline.context,e->reference_valid);
        frdd_puts(",\"reference_identity\":");
        if(e->reference.result && e->reference.got==sizeof(e->baseline))
            frdd_printf("{\"magic\":\"%016" PRIx64 "\",\"pid\":%lu,\"tid\":%lu,\"case_id\":%lu,\"complete\":%lu}",
                        e->baseline.magic,(unsigned long)e->baseline.pid,(unsigned long)e->baseline.tid,
                        (unsigned long)e->baseline.case_id,(unsigned long)e->baseline.complete);
        else frdd_puts("null");
        frdd_puts(",\"frames\":\"unknown-callsite-SP-proof\"}\n");
        if(e->code==EXCEPTION_ACCESS_VIOLATION) frdd_puts(e->av_text);
    }
    frdd_printf("FRDD1 {\"type\":\"end\",\"pid\":%lu,\"failed\":1,\"diagnostic_failed\":%d,\"native\":\"unresolved\"}\n",(unsigned long)d->pid,d->failed);
}
static void frdd_release(frdd_state *d)
{
    unsigned i;
    for(i=0;i<d->modules;++i) if(d->images[i].file) {
        frdd_bool(&d->images[i].file_close,CloseHandle(d->images[i].file));
        if(!d->images[i].file_close.result) d->failed=1;
        d->images[i].file=NULL;
    }
}
#endif
