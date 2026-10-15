#!/usr/bin/env python3
"""stdin 的有界失败字节 -> 观察；无路径/地址参数、执行、网络或 native 权限。"""
import json
import re
import sys

MAX_BYTES = 1024 * 1024
FLAGS = 0x400007


class DiagnosticError(ValueError):
    pass


def need(ok, reason):
    if not ok:
        raise DiagnosticError(reason)


def keys(value, expected):
    need(type(value) is dict and set(value) == set(expected.split()), 'record keys')


def integer(value, low=0, high=0xffffffff):
    need(type(value) is int and low <= value <= high, 'integer type/range')
    return value


def bit(value):
    return integer(value, 0, 1)


def hexb(value, maximum, exact=None):
    need(type(value) is str and len(value) % 2 == 0 and len(value) <= maximum * 2 and
         re.fullmatch('[0-9a-f]*', value) is not None, 'canonical hex bounds')
    raw = bytes.fromhex(value)
    need(exact is None or len(raw) == exact, 'hex exact length')
    return raw


def address(value):
    hexb(value, 8, 8)
    return int(value, 16)


def api(value, maximum=65536):
    keys(value, 'attempted result error_valid error got')
    a = bit(value['attempted']); result = integer(value['result'], -(1 << 31), (1 << 31)-1)
    valid = bit(value['error_valid']); error = integer(value['error']); integer(value['got'], 0, maximum)
    need(valid == int(bool(a) and result == 0), 'BOOL error validity')
    need(a or (result == 0 and error == 0 and value['got'] == 0), 'unattempted API fabricated data')
    need(valid or error == 0, 'success has no last-error authority')
    return bool(a and result != 0)


def context(value):
    keys(value, 'flags pc sp fp lr pstate x d8_15')
    flags = integer(value['flags']); need(flags & FLAGS == FLAGS, 'ARM64 selective flags')
    for key in ('pc', 'sp', 'fp', 'lr'): address(value[key])
    integer(value['pstate'])
    for name, count in (('x', 29), ('d8_15', 8)):
        need(type(value[name]) is list and len(value[name]) == count, 'context register count')
        for item in value[name]: address(item)


def identity(value):
    keys(value, 'volume index size write')
    integer(value['volume']); address(value['index']); address(value['write'])
    integer(value['size'], 0, (1 << 64)-1)


def file_dependencies(row, module, pre, path_ok, failed):
    """只验证固定 producer 的文件依赖；hash read 可保留上一次循环结果。"""
    n = integer(row['code_size'], 0, 256)
    ok = {key: api(row[key], maximum) for key, maximum in (
        ('header_seek', 0), ('header_read', 4096), ('code_seek', 0),
        ('code_read', n), ('seek', 0), ('read', 65536))}
    api(row['rpm'], n)
    bit(row['mapped']); bit(row['matched'])
    length = row['before']['size'] if pre else 0
    eligible = bool(pre and path_ok and 4096 <= length <= 32*1024*1024)
    need(bool(row['header_seek']['attempted']) == eligible, 'header seek prerequisites')
    need(bool(row['header_read']['attempted']) == ok['header_seek'], 'header read requires seek')
    header_full = ok['header_read'] and row['header_read']['got'] == 4096
    header_ready = header_full and row['file_machine'] == module['machine'] and row['file_image_size'] == module['size']
    need(not row['hash_attempted'] or header_ready, 'hash requires complete matching header')
    need(not row['mapped'] or row['hash_attempted'], 'mapping requires header path')
    need(bool(row['code_seek']['attempted']) == bool(row['mapped']), 'code seek requires unique mapping')
    need(bool(row['code_read']['attempted']) == ok['code_seek'], 'code read requires seek')
    code_full = ok['code_read'] and row['code_read']['got'] == n and n > 0
    need(bool(row['rpm']['attempted']) == code_full, 'remote code requires full file code read')
    need(not row['matched'] or ok['code_seek'] and code_full, 'match requires file code prerequisites')
    done = row['hash_bytes']; mask = row['hash_stages_attempted']; stages = row['hash_stages']
    reason = row['hash_failure']
    if not row['hash_attempted']:
        need(done == 0 and mask == 0 and row['hash_status'] == 0 and reason == 'not-attempted' and
             row['sha256'] is None and not row['seek']['attempted'] and not row['read']['attempted'] and
             n == 0 and address(row['code_start']) == 0, 'unattempted file evidence absent')
        return
    need(n > 0 and done <= length and mask & 1 and reason != 'not-attempted', 'hash attempt coverage')
    opened = stages[0] < 0x80000000
    need(bool(mask & 2) == opened, 'hash create requires provider')
    created = opened and stages[1] < 0x80000000
    need(not (mask & 12) or created, 'hash data/finish requires create')
    need(not done or mask & 4, 'covered chunks require hash data')
    if not created:
        need(done == 0 and reason == 'bcrypt' and not row['seek']['attempted'] and
             not row['read']['attempted'], 'failed hash setup has no file reads')
    elif done == length:
        # 精确整块也必须读取 65536 字节，不能将模余数 0 当作末块长度。
        final_n = (length - 1) % 65536 + 1
        need(ok['seek'] and ok['read'] and row['read']['got'] == final_n and mask & 4 and
             stages[2] < 0x80000000 and mask & 8, 'complete hash requires exact final read')
        need(reason == ('none' if stages[3] < 0x80000000 else 'bcrypt'), 'finish hash reason')
    else:
        need(not mask & 8, 'incomplete hash cannot finish')
        requested = min(length - done, 65536)
        if reason == 'budget':
            need(bool(row['seek']['attempted']) == bool(done) and
                 bool(row['read']['attempted']) == bool(done), 'budget preserves previous iteration')
            if done:
                need(ok['seek'] and ok['read'] and row['read']['got'] == 65536, 'budget prior full chunk')
        elif reason == 'file-read':
            need(row['seek']['attempted'], 'file failure requires seek attempt')
            if not ok['seek']:
                # 下一轮 seek 失败不覆盖 read：前一轮成功结果仍须完整保留。
                need(bool(row['read']['attempted']) == bool(done), 'failed seek prior read presence')
                if done: need(ok['read'] and row['read']['got'] == 65536, 'failed seek prior full chunk')
            else:
                need(row['read']['attempted'] and row['read']['got'] <= requested and
                     (not ok['read'] or row['read']['got'] != requested), 'failed or partial current read')
        elif reason == 'bcrypt':
            need(mask & 4 and stages[2] >= 0x80000000 and ok['seek'] and ok['read'] and
                 row['read']['got'] == requested, 'hash data failure follows full current read')
        else:
            raise DiagnosticError('incomplete hash failure reason')
        if reason in ('budget', 'file-read'):
            need(bool(mask & 4) == bool(done) and stages[2] < 0x80000000, 'prior hashed chunk state')
    if row['sha256'] is None or not row['matched']:
        need(failed, 'file observation failure latch')


def validate_block(rows):
    start, end = rows[0], rows[-1]
    keys(start, 'type version case pid events modules overflow reference_rva')
    need(start['version'] == 1 and type(start['version']) is int, 'version')
    need(start['case'] in ('preserve', 'corrupt_fp', 'corrupt_sp16', 'corrupt_sp8', 'wrong_return', 'no_return'), 'case')
    pid = integer(start['pid'], 1); ne = integer(start['events'], 0, 8); nm = integer(start['modules'], 0, 32)
    bit(start['overflow']); integer(start['reference_rva'], 1, 0xffffffff-944)
    keys(end, 'type pid failed diagnostic_failed native')
    need(integer(end['pid'], 1) == pid and bit(end['failed']) == 1 and end['native'] == 'unresolved', 'failure-only authority')
    bit(end['diagnostic_failed'])
    modules, events, avs = [], [], []
    for row in rows[1:-1]:
        kind = row.get('type')
        if kind == 'module':
            need(not events, 'module ordering')
            keys(row, 'type index pid tid load_seq unload_seq base size machine header duplicate event_close file_close')
            need(integer(row['index'], 0, 31) == len(modules) and integer(row['pid'], 1) == pid, 'module ownership/index')
            integer(row['tid'], 1); integer(row['load_seq'], 1); integer(row['unload_seq'])
            need(not row['unload_seq'] or row['unload_seq'] > row['load_seq'], 'unload sequence')
            base = address(row['base']); size = integer(row['size']); machine = integer(row['machine'], 0, 65535)
            need(base > 0 and base + size < 1 << 64, 'module overflow')
            good = api(row['header'], 4096)
            if size: need(good and row['header']['got'] == 4096 and size >= 4096 and machine > 0, 'remote image range')
            copied = api(row['duplicate']); closed_event = api(row['event_close']); closed_file = api(row['file_close'])
            need(not row['duplicate']['attempted'] or row['event_close']['attempted'], 'event file ownership')
            need(not copied or row['file_close']['attempted'], 'owned duplicate cleanup')
            if not closed_event or copied and not closed_file: need(end['diagnostic_failed'] or not row['duplicate']['attempted'], 'close failure latch')
            if row['header']['attempted'] and (not good or not size):
                need(end['diagnostic_failed'], 'module read failure latch')
            modules.append(row)
        elif kind == 'event':
            keys(row, 'type seq pid tid code first pc open owner thread_id actual_pid actual_tid context_api close context reference_api reference_valid baseline reference_identity frames')
            need(integer(row['pid'], 1) == pid, 'event PID')
            integer(row['tid'], 1); integer(row['seq'], 1); integer(row['code']); bit(row['first']); address(row['pc'])
            need(not events or row['seq'] > events[-1]['seq'], 'event sequence duplicate/order')
            opened = api(row['open']); owner = api(row['owner']); thread_id = api(row['thread_id']); closed = api(row['close'])
            integer(row['actual_pid']); integer(row['actual_tid'])
            need(owner == bool(row['actual_pid']) and thread_id == bool(row['actual_tid']), 'query API actual ID')
            for field in ('open', 'owner', 'thread_id', 'context_api', 'close'):
                need(row[field]['got'] == 0, 'non-read API cannot claim byte count')
            need(opened or not any(row[k]['attempted'] for k in ('owner', 'thread_id', 'close')), 'unowned thread API attempt')
            if row['context_api']['attempted']:
                need(opened and owner and thread_id and row['actual_pid'] == pid and row['actual_tid'] == row['tid'], 'context attempt requires actual thread ownership')
            cpu = api(row['context_api'])
            if cpu:
                need(opened and owner and thread_id and row['actual_pid'] == pid and row['actual_tid'] == row['tid'], 'actual thread ownership')
                context(row['context'])
            else: need(row['context'] is None and end['diagnostic_failed'], 'failed context cannot be actual zeros')
            need(not opened or row['close']['attempted'], 'owned thread cleanup')
            if opened and not closed: need(end['diagnostic_failed'], 'close failure latch')
            reference = api(row['reference_api'], 944); bit(row['reference_valid'])
            if reference and row['reference_api']['got'] == 944:
                ref = row['reference_identity']; keys(ref, 'magic pid tid case_id complete')
                address(ref['magic'])
                for field in ('pid', 'tid', 'case_id', 'complete'): integer(ref[field])
            else: need(row['reference_identity'] is None, 'partial reference identity absent')
            if row['reference_valid']:
                need(reference and row['reference_api']['got'] == 944, 'full independent reference read')
                context(row['baseline'])
                ref = row['reference_identity']; keys(ref, 'magic pid tid case_id complete')
                need(ref['magic'] == '4652444449414731' and integer(ref['pid'], 1) == pid and
                     integer(ref['tid'], 1) == row['tid'] and bit(ref['complete']) == 1 and
                     integer(ref['case_id'], 0, 5) == ('preserve', 'corrupt_fp', 'corrupt_sp16', 'corrupt_sp8', 'wrong_return', 'no_return').index(start['case']), 'reference identity')
            else: need(row['baseline'] is None and end['diagnostic_failed'], 'unknown reference must be absent')
            need(row['frames'] == 'unknown-callsite-SP-proof', 'no inferred H/O authority')
            events.append(row)
        elif kind == 'av':
            need(events and events[-1]['code'] == 0xc0000005 and not any(x['seq'] == row.get('seq') for x in avs), 'AV order/duplicate')
            e = events[-1]
            need(all(type(row.get(k)) is int and row[k] == e[k] for k in ('seq', 'pid', 'tid')), 'AV event tie')
            if row.get('module') is None:
                keys(row, 'type seq pid tid module reason')
                need(row['reason'] == 'module-or-file-unknown' and end['diagnostic_failed'], 'explicit unknown module')
            else:
                keys(row, 'type seq pid tid module base rva path_api path_utf16 path_valid path_length file_machine file_image_size file_offset section_rva section_raw section_size section_flags before_api before after_api after stable hash_attempted hash_status sha256 hash_failure hash_bytes hash_stages_attempted hash_stages header_seek header_read code_seek code_read code_start code_size mapped matched seek read rpm remote disk')
                ix = integer(row['module'], 0, len(modules)-1); m = modules[ix]
                need(m['load_seq'] < e['seq'] and (not m['unload_seq'] or m['unload_seq'] > e['seq']), 'stale module')
                base = address(row['base']); rva = integer(row['rva'])
                need(row['base'] == m['base'] and rva < m['size'] and base+rva == address(e['pc']), 'module PC/range tie')
                need(api(m['duplicate']), 'module file ownership')
                path_api_ok = api(row['path_api']); path_ok = bit(row['path_valid']); path = hexb(row['path_utf16'], 2046)
                need(row['path_api']['result'] == row['path_length'] and path_ok == int(path_api_ok and 0 < row['path_length'] < 1024), 'actual path API result/bound')
                need((path_ok and len(path)>0 and len(path)%2==0) or (not path_ok and not path), 'path length/status')
                try: decoded = path.decode('utf-16-le')
                except UnicodeError as exc: raise DiagnosticError('path UTF16') from exc
                need('\0' not in decoded, 'path NUL')
                if path_ok:
                    need(re.match(r'^\\\\\?\\(?:[A-Za-z]:\\|UNC\\[^\\]+\\[^\\]+\\)', decoded) is not None and
                         '/' not in decoded and not any(part in ('.', '..') for part in decoded.split('\\')), 'canonical file path')
                integer(row['path_length'])
                need(not path_ok or row['path_length'] == len(path)//2, 'path return length')
                for field in ('file_machine', 'file_image_size', 'section_rva', 'section_raw', 'section_size', 'section_flags'): integer(row[field])
                integer(row['file_offset'], 0, 32*1024*1024)
                pre, post = api(row['before_api']), api(row['after_api'])
                for key, valid in (('before', pre), ('after', post)):
                    if valid: identity(row[key])
                    else: need(row[key] is None, 'failed identity absent')
                stable = bit(row['stable']); need(stable == int(pre and post and row['before'] == row['after']), 'file identity stability')
                bit(row['hash_attempted']); integer(row['hash_status']); mask = integer(row['hash_stages_attempted'], 0, 63)
                need(type(row['hash_stages']) is list and len(row['hash_stages']) == 6, 'BCrypt stage count')
                for i, status in enumerate(row['hash_stages']):
                    integer(status); need(mask & (1 << i) or status == 0, 'unattempted BCrypt status')
                need(row['hash_failure'] in ('none', 'not-attempted', 'budget', 'file-read', 'bcrypt'), 'hash failure reason')
                integer(row['hash_bytes'], 0, 32*1024*1024)
                need(row['hash_bytes'] % 65536 == 0 or pre and row['hash_bytes'] == row['before']['size'], 'complete hash chunk coverage')
                if row['sha256'] is not None:
                    hexb(row['sha256'], 32, 32)
                    need(path_ok and stable and row['hash_attempted'] and mask == 63 and all(s < 0x80000000 for s in row['hash_stages']) and
                         row['hash_status'] < 0x80000000 and row['hash_failure'] == 'none' and
                         4096 <= row['before']['size'] <= 32*1024*1024 and row['hash_bytes'] == row['before']['size'], 'hash exact file coverage')
                else: need(end['diagnostic_failed'], 'hash failure latch')
                file_dependencies(row, m, pre, path_ok, end['diagnostic_failed'])
                n = integer(row['code_size'], 0, 256); code_start = address(row['code_start']); bit(row['mapped']); bit(row['matched'])
                rpm = api(row['rpm'], 256); remote = hexb(row['remote'], 256); disk = hexb(row['disk'], 256)
                need(len(remote) == (n if rpm and row['rpm']['got'] == n else 0), 'RPM exact raw length')
                need(len(disk) == (n if api(row['code_read']) and row['code_read']['got'] == n else 0), 'file exact raw length')
                if row['mapped']:
                    code_rva=code_start-base
                    need(row['file_machine'] == m['machine'] and row['file_image_size'] == m['size'] and
                         row['section_flags'] & 0x20000000 and code_rva >= row['section_rva'] and
                         code_rva-row['section_rva']+n <= row['section_size'] and
                         row['file_offset'] == row['section_raw']+code_rva-row['section_rva'] and
                         pre and row['file_offset']+n <= row['before']['size'], 'file PE mapping arithmetic')
                    need(n > 0 and base <= code_start <= base+rva < code_start+n <= base+m['size'] and
                         api(row['header_read']) and row['header_read']['got'] == 4096, 'bounded code mapping')
                if row['matched']:
                    need(row['mapped'] and rpm and len(remote) == n and len(disk) == n and remote == disk, 'disk/remote code match')
                else: need(end['diagnostic_failed'], 'code failure latch')
                candidates = [other for other in modules if other['size'] and other['load_seq'] < e['seq'] and
                              (not other['unload_seq'] or other['unload_seq'] > e['seq']) and
                              address(other['base']) <= address(e['pc']) < address(other['base'])+other['size']]
                need(len(candidates) == 1, 'unique actual module ownership')
            avs.append(row)
        else: raise DiagnosticError('record type/order')
    need(len(events) == ne and len(modules) == nm, 'declared counts')
    references = [(e['baseline'], e['reference_identity']) for e in events if e['reference_valid']]
    need(not references or all(item == references[0] for item in references), 'independent reference changed between stops')
    need(len(avs) == sum(e['code'] == 0xc0000005 for e in events), 'missing AV record')
    return dict(case=start['case'], pid=pid, contexts=sum(e['context'] is not None for e in events),
                context_pc_mismatches=sum(e['context'] is not None and e['context']['pc'] != e['pc'] for e in events),
                access_violations=len(avs), unknown_modules=sum(a['module'] is None for a in avs),
                frames='unknown-callsite-SP-proof', native='unresolved', records=rows)


def unique(pairs):
    result = {}
    for key, value in pairs:
        need(key not in result, 'duplicate JSON key'); result[key] = value
    return result


def parse(raw):
    need(type(raw) is bytes and 0 < len(raw) <= MAX_BYTES and raw.endswith(b'\n'), 'failure stream bound/framing')
    blocks, pending = [], []
    for line in raw.splitlines():
        need(len(line) <= 32768, 'line bound')
        if not line.startswith(b'FRDD1 '):
            need(not pending and line.startswith(b'FRD1 failed '), 'non-diagnostic line location')
            continue
        try:
            row = json.loads(line[6:], object_pairs_hook=unique, parse_constant=lambda _: (_ for _ in ()).throw(DiagnosticError('nonfinite JSON')))
        except (ValueError, UnicodeError, RecursionError) as exc: raise DiagnosticError('JSON: '+str(exc)) from exc
        need(type(row) is dict, 'record object')
        if not pending: need(row.get('type') == 'begin', 'missing begin')
        else: need(row.get('type') != 'begin', 'nested begin')
        pending.append(row); need(len(pending) <= 51, 'record count')
        if row.get('type') == 'end':
            blocks.append(validate_block(pending)); pending=[]
            need(len(blocks) <= 6 and len({b['case'] for b in blocks}) == len(blocks), 'block count/duplicate case')
    need(blocks and not pending, 'incomplete block')
    return dict(schema='freeremotedesk.windows.failure-diagnostics.observations.v1', native='unresolved', blocks=blocks)


def main():
    if len(sys.argv) != 1:
        print('仅从 stdin 读取失败 stderr；不接受路径或地址参数', file=sys.stderr); return 2
    try: result = parse(sys.stdin.buffer.read(MAX_BYTES+1))
    except DiagnosticError as exc:
        print('diagnostics refused: '+str(exc), file=sys.stderr); return 1
    json.dump(result, sys.stdout, sort_keys=True); sys.stdout.write('\n'); return 0


if __name__ == '__main__':
    raise SystemExit(main())
