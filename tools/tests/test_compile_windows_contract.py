#!/usr/bin/env python3
"""窄 parser/provenance/orchestration 控制；手写容器和 mock 从不证明 native Windows。"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "compile-windows-contract.py"
SPEC = importlib.util.spec_from_file_location("contract_compile", SCRIPT)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

# 一条 ret 的 container fixture；只验证 parser，不是可运行 ABI/SEH fixture。
COFF = bytes.fromhex(
    "64aa0100 00000000 00000000 00000000 0000 0000 "
    "2e74657874000000 00000000 00000000 04000000 3c000000 "
    "00000000 00000000 0000 0000 20000060 c0035fd6"
)


CONTRACT_APIS = ("RtlCaptureContext", "RtlLookupFunctionEntry", "RtlVirtualUnwind", "OpenThread", "GetThreadContext")
BCRYPT_APIS = ("BCryptOpenAlgorithmProvider", "BCryptCreateHash", "BCryptHashData",
               "BCryptFinishHash", "BCryptDestroyHash", "BCryptCloseAlgorithmProvider")
DEFAULT_IMPORTS = (("KERNEL32.dll", CONTRACT_APIS), ("msvcrt.dll", ("__C_specific_handler",)), ("bcrypt.dll", BCRYPT_APIS))


def container_pe(imports=DEFAULT_IMPORTS):
    """独立手写 PE32+ 数据表，没有程序/native 成功语义。"""
    data = bytearray(0xC00)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, 0xAA64, 1, 0, 0, 0, 240, 2)
    opt = 0x98
    struct.pack_into("<H", data, opt, 0x20B)
    struct.pack_into("<Q", data, opt + 24, 0x140000000)
    struct.pack_into("<II", data, opt + 56, 0x2000, 0x200)
    struct.pack_into("<H", data, opt + 68, 3)
    struct.pack_into("<I", data, opt + 108, 16)
    struct.pack_into("<II", data, opt + 112 + 8, 0x1200, (len(imports) + 1) * 20)
    struct.pack_into("<II", data, opt + 112 + 24, 0x1000, 8)
    struct.pack_into("<8sIIIIIIHHI", data, 0x188, b".text\0\0\0", 0xA00, 0x1000, 0xA00, 0x200, 0, 0, 0, 0, 0x60000020)
    struct.pack_into("<II", data, 0x200, 0x1100, 0x1120)
    struct.pack_into("<II", data, 0x320, 4 | (1 << 27), 0xE4)
    hint_name = 0x1500
    for index, (dll, symbols) in enumerate(imports):
        dll = dll.encode("ascii") + b"\0"
        name = 0x1280 + index * 32
        thunk = 0x1300 + index * 64
        struct.pack_into("<IIIII", data, 0x400 + index * 20, thunk, 0, 0, name, thunk)
        data[name - 0xE00:name - 0xE00 + len(dll)] = dll
        for position, symbol in enumerate(symbols):
            if isinstance(symbol, int):
                entry = (1 << 63) | symbol
            else:
                encoded = symbol.encode("ascii") + b"\0"
                entry = hint_name
                struct.pack_into("<H", data, hint_name - 0xE00, 0)
                data[hint_name - 0xE00 + 2:hint_name - 0xE00 + 2 + len(encoded)] = encoded
                hint_name += 2 + len(encoded)
            struct.pack_into("<Q", data, thunk - 0xE00 + position * 8, entry)
        struct.pack_into("<Q", data, thunk - 0xE00 + len(symbols) * 8, 0)
    return data



def controlled_coff(functions, tables, selector=False, labels=()):
    """独立手写实际 COFF bytes；只为 parser/机器字负控，不是 native 程序。"""
    text = bytearray(); function_offsets = {}
    for name, raw in functions:
        function_offsets[name] = len(text); text.extend(raw)
    xdata = bytearray(); pdata = bytearray(); pdata_relocs = []
    for name, metadata in tables:
        direct = name in ('frd_h264_dc_abi_sp_failure_fragment', 'frd_h264_dc_wrong_return')
        pdata_relocs += [(len(pdata), name if direct else '.text', 2), (len(pdata)+4, '.xdata', 2)]
        pdata.extend(struct.pack('<II', 0 if direct else function_offsets[name], len(xdata)))
        xdata.extend(metadata)
    names = [(name, offset, 1, 0x20, 2) for name, offset in function_offsets.items()]
    names += [(name,value,1,0,2) for name,value in labels]
    names += [('.text',0,1,0,3),('.xdata',0,2,0,3),('.pdata',0,3,0,3)]
    if selector: names.append(('frd_h264_dc_corrupt_fp',0,0,0,2))
    symbol_indices = {item[0]:index for index,item in enumerate(names)}
    text_relocs = [(0x1a0,'frd_h264_dc_corrupt_fp',4),(0x1a4,'frd_h264_dc_corrupt_fp',6)] if selector else []
    chunks=[]; section_headers=[]; cursor=20+3*40
    for name, raw, relocs in [('.text',text,text_relocs),('.xdata',xdata,[]),('.pdata',pdata,pdata_relocs)]:
        relocation_bytes=b''.join(struct.pack('<IIH',offset,symbol_indices[target],kind) for offset,target,kind in relocs)
        section_headers.append(struct.pack('<8sIIIIIIHHI',name.encode(),0,0,len(raw),cursor,cursor+len(raw) if relocs else 0,0,len(relocs),0,0x60300020 if name=='.text' else 0x40300040))
        chunks += [bytes(raw),relocation_bytes];cursor+=len(raw)+len(relocation_bytes)
    strings=bytearray(bytes(4));symbol_bytes=bytearray()
    for name,value,section,typ,storage in names:
        encoded=name.encode()
        if len(encoded)>8:
            encoded=struct.pack('<II',0,len(strings));strings.extend(name.encode()+b'\0')
        symbol_bytes.extend(struct.pack('<8sIhHBB',encoded,value,section,typ,storage,0))
    struct.pack_into('<I',strings,0,len(strings))
    return struct.pack('<HHIIIHH',0xaa64,3,0,cursor,len(names),0,0)+b''.join(section_headers)+b''.join(chunks)+symbol_bytes+strings


GOOD_SP = bytes.fromhex('08002028e5d992d910d88ed80ce6e6e6e6c802400ce4e3e3')
GOOD_MAIN = bytes.fromhex('14016028e1d992d910d88ed80ce6e6e6e6c802400ce4e3e3')
GOOD_OUTER = bytes.fromhex('0c016028e1d992d910d88ed80ce6e6e6e6c802400ce4e3e3')
GOOD_WRONG = bytes.fromhex('04002028e5d992d910d88ed80ce6e6e6e6c802400ce4e3e3')

class TempTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="contract-container-only-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()


class TrapBindingTests(TempTests):
    # 手写函数范围与指令位置；object_range 仅替身读取，不运行任何程序。
    def fixture(self, word=0xd43e0000, changed=None):
        sp = 'frd_h264_dc_abi_sp_failure_fragment'
        definitions = [('frd_h264_dc_check_abi',0x1000,0x450,0x418), (sp,0x1450,32,24),
                       ('frd_h264_dc_outer_probe',0x2000,0x430,0x3f8),
                       ('frd_h264_dc_preserve',0x3000,4,None),('frd_h264_dc_corrupt_fp',0x3004,8,None),
                       ('frd_h264_dc_corrupt_sp16',0x300c,8,None),('frd_h264_dc_corrupt_sp8',0x3014,8,None),
                       ('frd_h264_dc_wrong_return',0x301c,16,8),('frd_h264_dc_no_return',0x302c,4,None)]
        callbacks = {'frd_h264_dc_preserve':'c0035fd6','frd_h264_dc_corrupt_fp':'fd031faac0035fd6',
            'frd_h264_dc_corrupt_sp16':'ff430091c0035fd6','frd_h264_dc_corrupt_sp8':'ff230091c0035fd6',
            'frd_h264_dc_wrong_return':'5e000010c0035fd600003ed4ffffff17','frd_h264_dc_no_return':'00000014'}
        raw,linked,symbols,pdata={},{},{},[]
        metadata={'frd_h264_dc_check_abi':GOOD_MAIN,sp:GOOD_SP,'frd_h264_dc_outer_probe':GOOD_OUTER,'frd_h264_dc_wrong_return':GOOD_WRONG}
        for name,rva,length,trap in definitions:
            data=bytearray.fromhex(callbacks[name]) if name in callbacks else bytearray(length)
            if name=='frd_h264_dc_check_abi':
                struct.pack_into('<5I',data,0x1a0,0x90000009,0x91000129,0xeb09007f,0x54001520,0xd63f0060)
            if name==sp:
                data[:]=struct.pack('<8I',0xd63f0060,0x910003e9,0xf2400d3f,0x54000061,0xeb0903bf,0x54ffea80,0xd43e0000,0x17ffffff)
            if trap is not None: struct.pack_into('<I',data,trap,word if changed in (None,name) else 0xd43e0000)
            raw[name]=bytes(data);linked[name]=bytes(data)
            symbols[name]={'rva':rva,'section':1,'value':0}
            if name in metadata:pdata.append({'begin_rva':rva,'length':length,'xdata_hex':metadata[name].hex()})
        # 独立 linker 算术：target=0x3004，selector PC=0x11a0。
        data=bytearray(linked['frd_h264_dc_check_abi'])
        struct.pack_into('<II',data,0x1a0,0xd0000009,0x91001129)
        linked['frd_h264_dc_check_abi']=bytes(data)
        helper_functions=[(name,raw[name]) for name in ('frd_h264_dc_check_abi',sp)]
        negative_functions=[(name,raw[name]) for name,_,_,_ in definitions if name not in ('frd_h264_dc_check_abi',sp)]
        helper_labels=[('frd_h264_dc_abi_pc_after_alloc',4),('frd_h264_dc_abi_pc_body',0x30),('frd_h264_dc_abi_pc_call',0x1b0),('frd_h264_dc_abi_pc_return',0x1b4),('frd_h264_dc_abi_pc_epilogue',0x420),('frd_h264_dc_abi_pc_epilogue_after_d14',0x424),('frd_h264_dc_abi_pc_ret',0x44c),('frd_h264_dc_abi_sp_failure_return',0x454),('frd_h264_dc_abi_sp_failure_trap',0x468),('frd_h264_dc_abi_sp_failure_end',0x470)]
        for name,value in helper_labels:symbols[name]={'rva':0x1000+value,'section':1,'value':value}
        negative_offsets={name:sum(len(r) for _,r in negative_functions[:i]) for i,(name,_) in enumerate(negative_functions)}
        negative_labels=[('frd_h264_dc_outer_pc_return',0x188),('frd_h264_dc_wrong_return_trap',negative_offsets['frd_h264_dc_wrong_return']+8),('frd_h264_dc_wrong_return_end',negative_offsets['frd_h264_dc_wrong_return']+16)]
        symbols['frd_h264_dc_outer_pc_return']={'rva':0x2188,'section':1,'value':0}
        symbols['frd_h264_dc_wrong_return_trap']={'rva':0x3024,'section':1,'value':0}
        symbols['frd_h264_dc_wrong_return_end']={'rva':0x302c,'section':1,'value':0}
        (self.root/'helper.obj').write_bytes(controlled_coff(helper_functions,[(name,metadata[name]) for name,_ in helper_functions],True,helper_labels))
        (self.root/'negative.obj').write_bytes(controlled_coff(negative_functions,[(name,metadata[name]) for name,_ in negative_functions if name in metadata],labels=negative_labels))
        routing = bytearray(); strings = bytearray()
        for name in callbacks:
            routing.extend(struct.pack('<QQ',0x140005100+len(strings),0x140000000+symbols[name]['rva']))
            strings.extend(name.removeprefix('frd_h264_dc_').encode()+b'\0')
        def at(rva,size):
            if 0x5000<=rva and rva+size<=0x5060:return bytes(routing[rva-0x5000:rva-0x5000+size])
            if 0x5100<=rva and rva+size<=0x5100+len(strings):return bytes(strings[rva-0x5100:rva-0x5100+size])
            if rva==0x4000 and size==944:return bytes(912)+bytes.fromhex('3147414944445246')+bytes(24)
            for name,start,length,_ in definitions:
                if start<=rva and rva+size<=start+length:return linked[name][rva-start:rva-start+size]
            raise AssertionError('fixture range')
        symbols['frd_diag_reference']={'rva':0x4000,'section':2,'value':0}
        symbols['frd_h264_dc_private_children']={'rva':0x5000,'section':3,'value':0}
        pe={'symbols':symbols,'pdata':pdata,'at':at,'image_size':0x6000,'image_base':0x140000000,
            'sections':[{'rva':0x1000,'virtual_size':0x2030,'flags':0x60000020},{'rva':0x4000,'virtual_size':944,'flags':0xc0000040},{'rva':0x5000,'virtual_size':0x200,'flags':0x40000040}]}
        def reader(path,name):
            return raw[name], {name:{'section':1,'value':0}}
        return pe,reader

    def test_matching_object_and_pe_with_old_trap_are_rejected(self):
        for name, word in [('frd_h264_dc_check_abi', 0xd421e020),
                           ('frd_h264_dc_wrong_return', 0xd421e040),
                           ('frd_h264_dc_outer_probe', 0xd421e060)]:
            pe, reader = self.fixture(word, name)
            with self.subTest(name=name), mock.patch.object(GATE, 'object_range', side_effect=reader), mock.patch.object(GATE, 'sha', return_value='0'*64):
                with self.assertRaises(GATE.GateError):
                    GATE.bindings(pe, self.root/'helper.obj', self.root/'negative.obj')

    def test_diagnostic_reference_requires_independent_aligned_data_binding(self):
        for kind in ('missing', 'unaligned', 'short', 'executable', 'readonly', 'initial-bytes'):
            pe, reader = self.fixture()
            if kind=='missing': del pe['symbols']['frd_diag_reference']
            elif kind=='unaligned': pe['symbols']['frd_diag_reference']['rva']+=1
            elif kind=='short': pe['sections'][1]['virtual_size']=943
            elif kind=='executable': pe['sections'][1]['flags']|=0x20000000
            elif kind=='readonly': pe['sections'][1]['flags']&=~0x80000000
            else:
                original=pe['at']
                pe['at']=lambda rva,n: bytes(n) if rva==0x4000 else original(rva,n)
            with self.subTest(kind=kind), mock.patch.object(GATE, 'object_range', side_effect=reader), mock.patch.object(GATE, 'sha', return_value='0'*64):
                with self.assertRaises(GATE.GateError):
                    GATE.bindings(pe, self.root/'helper.obj', self.root/'negative.obj')

    def test_three_documented_traps_and_neighbour_length_owner_mutations(self):
        pe, reader = self.fixture()
        with mock.patch.object(GATE, 'object_range', side_effect=reader), mock.patch.object(GATE, 'sha', return_value='0'*64):
            result, _ = GATE.bindings(pe, self.root/'helper.obj', self.root/'negative.obj')
        self.assertEqual(result['functions']['frd_h264_dc_wrong_return']['length'], 0x10)
        for mode in ('neighbour', 'length', 'owner', 'linked-only'):
            pe, original = self.fixture()
            def changed(path, name):
                raw, symbols = original(path, name)
                if name != 'frd_h264_dc_check_abi': return raw, symbols
                if mode == 'neighbour':
                    data = bytearray(raw); data[0x418:0x41c] = b'\0'*4
                    data[0x41c:0x420] = bytes.fromhex('00003ed4')
                    return bytes(data), symbols
                if mode == 'length': return raw+b'\0'*4, symbols
                return raw, symbols
            if mode == 'owner': pe['symbols'].pop('frd_h264_dc_check_abi')
            if mode == 'linked-only':
                before = pe['at']
                pe['at'] = lambda start, size: before(start, size).replace(bytes.fromhex('00003ed4'), bytes.fromhex('200021d4'))
            with self.subTest(mode=mode), mock.patch.object(GATE, 'object_range', side_effect=changed), mock.patch.object(GATE, 'sha', return_value='0'*64):
                with self.assertRaises(GATE.GateError):
                    GATE.bindings(pe, self.root/'helper.obj', self.root/'negative.obj')


class C0MachineControls(TrapBindingTests):
    def admit(self, pe):
        return GATE.bindings(pe,self.root/'helper.obj',self.root/'negative.obj')

    def object_patch(self, path, section_name, offset, replacement):
        data=bytearray(path.read_bytes()); sections,_=GATE.object_layout(path)
        section=next(item for item in sections if item['name']==section_name)
        start=section['raw_offset']+offset;data[start:start+len(replacement)]=replacement;path.write_bytes(data)

    def test_actual_coff_closure_and_two_relocations_bind_exact_linked_words(self):
        pe,_=self.fixture();result,_=self.admit(pe)
        self.assertEqual(result['private_callback_contract']['id'],'C0')
        sections,_=GATE.object_layout(self.root/'helper.obj')
        relocs=next(s for s in sections if s['name']=='.text')['relocations']
        self.assertEqual([(r['offset'],r['kind']) for r in relocs],[(0x1a0,4),(0x1a4,6)])
        raw,_=GATE.object_range(self.root/'helper.obj','frd_h264_dc_check_abi')
        self.assertEqual(GATE.relocated_helper(raw,relocs,0x1000,0x3004),pe['at'](0x1000,0x450))
        # 页差为负、页内低12位都由独立地址值确定。
        linked=GATE.relocated_helper(raw,relocs,0x3000,0x1ffc)
        self.assertEqual(struct.unpack_from('<II',linked,0x1a0),(0xd0ffffe9,0x913ff129))

    def test_selector_relocation_field_addend_opcode_register_range_and_extra_refuse(self):
        import copy
        self.fixture();sections,_=GATE.object_layout(self.root/'helper.obj')
        good=next(s for s in sections if s['name']=='.text')['relocations']
        raw,_=GATE.object_range(self.root/'helper.obj','frd_h264_dc_check_abi')
        mutations=[]
        for index in (0,1):
            for field,value in [('offset',0x1a8),('kind',3)]:
                changed=copy.deepcopy(good);changed[index][field]=value;mutations.append((raw,changed,0x3004))
            for field,value in [('name','frd_h264_dc_corrupt_sp16'),('value',4),('section',1),('type',32),('storage',3)]:
                changed=copy.deepcopy(good);changed[index]['symbol'][field]=value;mutations.append((raw,changed,0x3004))
            for bit in (0,5,10,22,29):
                changed=bytearray(raw);word=struct.unpack_from('<I',changed,0x1a0+index*4)[0];struct.pack_into('<I',changed,0x1a0+index*4,word^(1<<bit))
                mutations.append((bytes(changed),good,0x3004))
        mutations += [(raw,good[:1],0x3004),(raw,good+good[:1],0x3004),(raw,good,1<<34)]
        for data,relocs,target in mutations:
            with self.subTest(relocs=relocs,target=target),self.assertRaises(GATE.GateError):
                GATE.relocated_helper(data,relocs,0x1000,target)

    def test_same_object_and_linked_callback_mutations_refuse_including_multiple_fault(self):
        for name,offset in [('frd_h264_dc_corrupt_fp',0),('frd_h264_dc_corrupt_fp',4),
                            ('frd_h264_dc_corrupt_sp16',0),('frd_h264_dc_wrong_return',0),('frd_h264_dc_no_return',0)]:
            pe,_=self.fixture();path=self.root/'negative.obj';sections,symbols=GATE.object_layout(path)
            symbol=next(item for item in symbols.values() if item['name']==name)
            # 原二指令 FP callback 的 RET 改为 SP 写，产生明确 multi-fault 负控。
            replacement=struct.pack('<I',0x910043ff if offset==4 else 0xd503201f)
            self.object_patch(path,'.text',symbol['value']+offset,replacement)
            original=pe['at'];rva=pe['symbols'][name]['rva'];raw=bytearray(original(rva,len(bytes.fromhex(GATE.CALLBACK_BYTES[name]))));raw[offset:offset+4]=replacement
            pe['at']=lambda address,n,original=original,rva=rva,raw=bytes(raw): raw[address-rva:address-rva+n] if rva<=address and address+n<=rva+len(raw) else original(address,n)
            with self.subTest(name=name,offset=offset),self.assertRaisesRegex(GATE.GateError,'fixed callback'):
                self.admit(pe)

    def test_same_object_and_linked_selector_guard_branch_mutations_refuse(self):
        for offset in (0x1a8,0x1ac,0x1b0):
            pe,_=self.fixture();replacement=struct.pack('<I',0xd503201f)
            self.object_patch(self.root/'helper.obj','.text',offset,replacement)
            old=pe['at'];raw=bytearray(old(0x1000,0x450));raw[offset:offset+4]=replacement
            pe['at']=lambda address,n,old=old,raw=bytes(raw):raw[address-0x1000:address-0x1000+n] if 0x1000<=address and address+n<=0x1450 else old(address,n)
            with self.subTest(offset=offset),self.assertRaisesRegex(GATE.GateError,'selector/call path'):
                self.admit(pe)

    def test_fragment_metadata_header_actions_padding_and_shared_records_refuse(self):
        # object 与 linked xdata 同时变动；raw==linked 不能替代独立固定 frame oracle。
        for name,index,value in [('frd_h264_dc_abi_sp_failure_fragment',4,0xe1),
                                 ('frd_h264_dc_abi_sp_failure_fragment',20,0x0b),
                                 ('frd_h264_dc_abi_sp_failure_fragment',19,0x41),
                                 ('frd_h264_dc_abi_sp_failure_fragment',0,7),
                                 ('frd_h264_dc_abi_sp_failure_fragment',2,0x60),
                                 ('frd_h264_dc_abi_sp_failure_fragment',2,0x24),
                                 ('frd_h264_dc_abi_sp_failure_fragment',23,0),
                                 ('frd_h264_dc_wrong_return',4,0xe1)]:
            pe,_=self.fixture();path=self.root/('helper.obj' if 'fragment' in name else 'negative.obj')
            tables=GATE.object_metadata(path,('frd_h264_dc_check_abi',GATE.SP_FRAGMENT) if 'fragment' in name else ('frd_h264_dc_outer_probe','frd_h264_dc_wrong_return'))
            sections,_=GATE.object_layout(path);xdata=next(s for s in sections if s['name']=='.xdata')['bytes'];base=xdata.index(tables[name])
            self.object_patch(path,'.xdata',base+index,bytes([value]))
            entry=next(e for e in pe['pdata'] if e['begin_rva']==pe['symbols'][name]['rva']);raw=bytearray.fromhex(entry['xdata_hex']);raw[index]=value;entry['xdata_hex']=raw.hex()
            with self.subTest(name=name,index=index),self.assertRaises(GATE.GateError):self.admit(pe)

    def test_missing_duplicate_overlap_foreign_owner_and_trap_plus4_truncation_refuse(self):
        for mode in ('missing','duplicate','overlap','foreign','truncate','shared-xdata'):
            pe,_=self.fixture();sp=next(e for e in pe['pdata'] if e['begin_rva']==0x1450)
            if mode=='missing':pe['pdata'].remove(sp)
            elif mode=='duplicate':pe['pdata'].append(dict(sp))
            elif mode=='overlap':pe['pdata'].append({'begin_rva':0x144c,'length':40,'xdata_hex':GOOD_SP.hex()})
            elif mode=='foreign':sp['begin_rva']=0x144c
            elif mode=='truncate':sp['length']=28
            else:sp['xdata_hex']=GOOD_MAIN.hex()
            with self.subTest(mode=mode),self.assertRaises(GATE.GateError):self.admit(pe)

    def test_fixed_child_table_and_all_linked_instruction_labels_are_bound(self):
        for mode in ('missing-table','writable-table','callback-route','case-name','label-missing','label-offset','label-section'):
            pe,_=self.fixture()
            if mode=='missing-table':del pe['symbols']['frd_h264_dc_private_children']
            elif mode=='writable-table':pe['sections'][2]['flags']|=0x80000000
            elif mode in ('callback-route','case-name'):
                previous=pe['at'];raw=bytearray(previous(0x5000,96))
                struct.pack_into('<Q',raw,24 if mode=='callback-route' else 16,0x14000300c if mode=='callback-route' else 0x140005100)
                pe['at']=lambda rva,n,previous=previous,raw=bytes(raw):raw if rva==0x5000 and n==96 else previous(rva,n)
            else:
                item=pe['symbols']['frd_h264_dc_abi_sp_failure_trap']
                if mode=='label-missing':del pe['symbols']['frd_h264_dc_abi_sp_failure_trap']
                elif mode=='label-offset':item['rva']+=4
                else:item['section']=2
            with self.subTest(mode=mode),self.assertRaises(GATE.GateError):self.admit(pe)

    def test_actual_pdata_wrong_relocation_kind_symbol_addend_and_alias_refuse(self):
        for mode in ('kind','symbol','addend','xdata-addend','alias'):
            self.fixture();path=self.root/'helper.obj';data=bytearray(path.read_bytes())
            sections,_=GATE.object_layout(path);pd=next(s for s in sections if s['name']=='.pdata')
            if mode=='kind':struct.pack_into('<H',data,pd['reloc_offset']+8,4)
            elif mode=='symbol':struct.pack_into('<I',data,pd['reloc_offset']+4,1)
            elif mode=='addend':struct.pack_into('<I',data,pd['raw_offset']+8,4)
            elif mode=='xdata-addend':struct.pack_into('<I',data,pd['raw_offset']+12,0)
            else:
                # 相同有效 begin、不同 symbol/addend，也拒绝为非声明 .rva。
                symbols=GATE.object_layout(path)[1];text_index=next(i for i,v in symbols.items() if v['name']=='.text')
                struct.pack_into('<I',data,pd['reloc_offset']+2*10+4,text_index)
                struct.pack_into('<I',data,pd['raw_offset']+8,0x450)
            path.write_bytes(data)
            with self.subTest(mode=mode),self.assertRaises(GATE.GateError):
                GATE.object_metadata(path,('frd_h264_dc_check_abi',GATE.SP_FRAGMENT))


class SourceAndPathTests(TempTests):
    def test_reviewed_driver_guide_and_support_pins_match_checkout(self):
        root = SCRIPT.parents[1]
        for name, pin in GATE.PINS.items():
            self.assertEqual(GATE.sha(root/GATE.FIXTURE/name), pin)
        self.assertEqual(GATE.sha(root/GATE.SUPPORT), GATE.SUPPORT_SHA)

    def test_diagnostic_header_is_in_real_source_snapshot(self):
        root = SCRIPT.parents[1]
        support = GATE.pinned_support(root)
        actual = GATE.source_snapshot(root, support)
        self.assertIn(GATE.FIXTURE + "/h264-luma-dc-windows-diagnostics.h", actual)

    def test_private_link_has_fixed_bcrypt_dependency(self):
        support = SimpleNamespace(CC=Path("/fixed/clang"), TARGET="aarch64-w64-mingw32",
                                  READOBJ=Path("/fixed/readobj"))
        commands = GATE.commands(Path("/source"), Path("/input"), Path("/out"), support)
        self.assertIn("dependency-libbcrypt.a", commands)
        for variant in range(5):
            self.assertEqual(commands["variant-%d-link" % variant].count("-lbcrypt"), 1)
            self.assertNotIn("-lbcrypt", commands["variant-%d-compile" % variant])

    def test_driver_pin_drift_and_symlink_rejected(self):
        source = self.root / GATE.FIXTURE / "driver.c"
        source.parent.mkdir(parents=True)
        source.write_bytes(b"reviewed")
        support_path = self.root / GATE.SUPPORT
        support_path.parent.mkdir(parents=True)
        support_path.write_bytes(b"support")
        support = SimpleNamespace(source_snapshot=lambda root: {})
        with mock.patch.object(GATE, "PINS", {"driver.c": GATE.sha(source)}), mock.patch.object(GATE, "OWN", ()), mock.patch.object(GATE, "SUPPORT_SHA", GATE.sha(support_path)):
            GATE.source_snapshot(self.root, support)
            source.write_bytes(b"changed")
            with self.assertRaises(GATE.GateError):
                GATE.source_snapshot(self.root, support)
            elsewhere = self.root / "elsewhere"
            elsewhere.write_bytes(b"reviewed")
            source.unlink()
            source.symlink_to(elsewhere)
            with self.assertRaises(GATE.GateError):
                GATE.source_snapshot(self.root, support)

    def test_support_drift_fails_before_import(self):
        path = self.root / GATE.SUPPORT
        path.parent.mkdir(parents=True)
        path.write_bytes(b"unreviewed")
        with mock.patch.object(GATE.importlib.util, "spec_from_file_location") as loader:
            with self.assertRaises(GATE.GateError):
                GATE.pinned_support(self.root)
            loader.assert_not_called()

    def test_fixed_same_run_relationships_no_alias_or_reuse(self):
        source = self.root / "target/task4c5c3b2c-compile-11-2"
        install = self.root / "target/task4c5c3b2c-install-11-2"
        out = self.root / "target/task4c5c3b2e-contract-11-2"
        source.mkdir(parents=True)
        install.mkdir()
        with mock.patch.dict(os.environ, {"GITHUB_RUN_ID": "11", "GITHUB_RUN_ATTEMPT": "2"}), mock.patch.object(GATE, "__file__", str(self.root / GATE.OWN[0])):
            for bad_input, bad_install, bad_out in ((source, source, out), (install, install, out),
                                                   (source, install, source), (source, install, self.root / "target/task4c5c3b2e-contract-11-1"),
                                                   (source, install, self.root / "target/../task4c5c3b2e-contract-11-2")):
                with self.assertRaises(GATE.GateError):
                    GATE.paths(str(self.root), str(bad_input), str(bad_install), str(bad_out))
            GATE.paths(str(self.root), str(source), str(install), str(out))
            with self.assertRaises(FileExistsError):
                GATE.paths(str(self.root), str(source), str(install), str(out))
            out.rmdir()
            out.symlink_to(source, target_is_directory=True)
            with self.assertRaises(FileExistsError):
                GATE.paths(str(self.root), str(source), str(install), str(out))

    def test_cli_has_no_compiler_flags_source_manifest_or_mode_override(self):
        required = ["--source-root", str(self.root), "--compile-input", str(self.root),
                    "--installation", str(self.root), "--output", str(self.root)]
        for extra in ("--compiler=clang", "--flags=-Wno-error", "--target=x86_64", "--source-manifest=/tmp/untrusted", "--out=/tmp/alias"):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                GATE.main(required + [extra])
            self.assertEqual(error.exception.code, 2)

    def test_five_vectors_have_matching_objects_and_no_bad5(self):
        support = SimpleNamespace(CC=Path("/opt/tool/clang"), READOBJ=Path("/opt/tool/readobj"), TARGET="aarch64-w64-mingw32")
        commands = GATE.commands(self.root, self.root / "input", self.root / "output", support)
        for variant in range(5):
            compile_argv = commands["variant-%d-compile" % variant]
            link = commands["variant-%d-link" % variant]
            self.assertIn("-DFRD_CONTRACT_VARIANT=%d" % variant, compile_argv)
            for flag in ("-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fno-omit-frame-pointer", "-fno-optimize-sibling-calls", "-DWINVER=0x0A00", "-D_WIN32_WINNT=0x0A00"):
                self.assertIn(flag, compile_argv)
            helper = "abi.obj" if variant == 0 else "bad-unwind-%d.obj" % variant
            self.assertEqual(link[2:5], [str(self.root / "output" / ("driver-%d.obj" % variant)), str(self.root / "input" / helper), str(self.root / "input/negative.obj")])
        self.assertFalse(any("bad-unwind-5" in argument for argv in commands.values() for argument in argv))


class BinaryAndRecordTests(TempTests):
    def test_same_run_success_is_not_old_run_or_target_authority(self):
        support = SimpleNamespace(SCHEMA="old.schema", TARGET="aarch64-w64-mingw32")
        keys = ("GITHUB_SHA", "GITHUB_REF", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "RUNNER_OS", "RUNNER_ARCH")
        environ = {key: "fixed-" + key for key in keys}
        value = {"schema": support.SCHEMA, "mode": "probe", "status": "compiled-only", "source_root": str(self.root),
                 "build_target": support.TARGET, "failures": [], "host": {"system": "Linux", "machine": "x86_64"}, "github": environ}
        with mock.patch.dict(os.environ, environ):
            GATE.same_run(value, "probe", "compiled-only", self.root, support)
            for field, replacement in (("status", "failed"), ("build_target", "x86_64"), ("source_root", "/other"), ("failures", [{"failure": "real"}])):
                changed = {**value, field: replacement}
                with self.assertRaises(GATE.GateError):
                    GATE.same_run(changed, "probe", "compiled-only", self.root, support)
            changed = {**value, "github": {**environ, "GITHUB_RUN_ID": "old-run"}}
            with self.assertRaises(GATE.GateError):
                GATE.same_run(changed, "probe", "compiled-only", self.root, support)

    def test_pe_container_only_and_wrong_machine_optional_native_image(self):
        path = self.root / "container.exe"
        baseline = container_pe()
        path.write_bytes(baseline)
        parsed = GATE.pe_admit(path)
        self.assertEqual(parsed["machine"], "0xAA64")
        self.assertEqual(parsed["pdata"][0]["length"], 16)
        self.assertEqual({entry["dll"] for entry in parsed["imports"]}, {"KERNEL32.dll", "msvcrt.dll", "bcrypt.dll"})
        self.assertEqual(parsed["imports"][0]["symbols"], [{"name": name} for name in CONTRACT_APIS])
        for offset, fmt, value in ((0x84, "<H", 0x8664), (0x84, "<H", 0xA641),
                                    (0x98, "<H", 0x10B), (0x98 + 68, "<H", 1),
                                    (0x84 + 18, "<H", 0x2002), (0x98 + 108, "<I", 15)):
            data = bytearray(baseline)
            struct.pack_into(fmt, data, offset, value)
            path.write_bytes(data)
            with self.assertRaises(GATE.GateError):
                GATE.pe_admit(path)
        for truncated in (baseline[:63], baseline[:0x97], baseline[:0x500]):
            path.write_bytes(truncated)
            with self.assertRaises(GATE.GateError):
                GATE.pe_admit(path)

    def test_chpe_clr_and_changed_import_closure_rejected(self):
        path = self.root / "container.exe"
        for kind in ("chpe", "clr", "crt"):
            data = container_pe()
            if kind == "chpe":
                struct.pack_into("<II", data, 0x98 + 112 + 80, 0x1400, 208)
                struct.pack_into("<I", data, 0x600, 208)
                struct.pack_into("<Q", data, 0x600 + 200, 1)
            elif kind == "clr":
                struct.pack_into("<II", data, 0x98 + 112 + 112, 0x1400, 72)
            else:
                data = container_pe((DEFAULT_IMPORTS[0], ("other.dll", DEFAULT_IMPORTS[1][1])))
            path.write_bytes(data)
            with self.assertRaises(GATE.GateError):
                GATE.pe_admit(path)

    def test_exact_kernel32_named_apis_and_case_insensitive_dlls(self):
        path = self.root / "container.exe"
        for kernel, crt in (("KERNEL32.dll", "msvcrt.dll"), ("KeRnEl32.DlL", "MSVCRT.DLL")):
            with self.subTest(kernel=kernel, crt=crt):
                path.write_bytes(container_pe(((kernel, CONTRACT_APIS), (crt, ("__C_specific_handler",)), DEFAULT_IMPORTS[2])))
                parsed = GATE.pe_admit(path)
                self.assertEqual(parsed["imports"][0]["symbols"], [{"name": name} for name in CONTRACT_APIS])

    def test_bcrypt_lifecycle_imports_are_fixed_and_provider_bound(self):
        path=self.root/'diagnostic-container.exe'
        for name in BCRYPT_APIS:
            for kind in ('missing', 'ordinal', 'duplicate', 'wrong-provider'):
                names=list(BCRYPT_APIS)
                names[names.index(name)]=1 if kind=='ordinal' else name+'Changed'
                imports=(DEFAULT_IMPORTS[0], DEFAULT_IMPORTS[1], ('bcrypt.dll', names))
                if kind=='duplicate': imports=(DEFAULT_IMPORTS[0], DEFAULT_IMPORTS[1], ('bcrypt.dll', BCRYPT_APIS+(name,)))
                if kind=='wrong-provider': imports=(DEFAULT_IMPORTS[0], ('msvcrt.dll', ('__C_specific_handler', name)), DEFAULT_IMPORTS[2])
                path.write_bytes(container_pe(imports))
                with self.subTest(name=name, kind=kind), self.assertRaises(GATE.GateError):
                    GATE.pe_admit(path)

    def test_each_required_api_missing_changed_spelling_or_ordinal_rejected(self):
        path = self.root / "container.exe"
        for index, api in enumerate(CONTRACT_APIS):
            for kind in ("missing", "spelling", "ordinal"):
                symbols = list(CONTRACT_APIS)
                if kind == "missing":
                    del symbols[index]
                else:
                    symbols[index] = api.lower() if kind == "spelling" else 1
                with self.subTest(api=api, kind=kind):
                    path.write_bytes(container_pe((("KERNEL32.dll", symbols), DEFAULT_IMPORTS[1], DEFAULT_IMPORTS[2])))
                    with self.assertRaisesRegex(GATE.GateError, "KERNEL32 缺少精确命名"):
                        GATE.pe_admit(path)

    def test_each_required_api_from_wrong_provider_rejected_even_with_correct_duplicate(self):
        path = self.root / "container.exe"
        for api in CONTRACT_APIS:
            for duplicate in (False, True):
                symbols = CONTRACT_APIS if duplicate else tuple(name for name in CONTRACT_APIS if name != api)
                with self.subTest(api=api, duplicate=duplicate):
                    path.write_bytes(container_pe((("KERNEL32.dll", symbols), ("msvcrt.dll", ("__C_specific_handler", api)), DEFAULT_IMPORTS[2])))
                    error = "Rtl API 必须由 KERNEL32 提供" if duplicate else "KERNEL32 缺少精确命名"
                    with self.assertRaisesRegex(GATE.GateError, error):
                        GATE.pe_admit(path)

    def test_unexpected_missing_or_duplicate_dll_closure_rejected(self):
        path = self.root / "container.exe"
        for imports in (DEFAULT_IMPORTS + (("NTDLL.dll", CONTRACT_APIS),),
                        DEFAULT_IMPORTS + (("other.dll", ("OtherFunction",)),),
                        (DEFAULT_IMPORTS[0],), (DEFAULT_IMPORTS[1],),
                        (("other.dll", CONTRACT_APIS), DEFAULT_IMPORTS[1]),
                        DEFAULT_IMPORTS + (("kernel32.DLL", ("OtherFunction",)),)):
            with self.subTest(dlls=[item[0] for item in imports]):
                path.write_bytes(container_pe(imports))
                with self.assertRaisesRegex(GATE.GateError, "actual import closure 必须恰为"):
                    GATE.pe_admit(path)

    def test_variant_one_requires_native_unwinder_for_wrong_return_fragment(self):
        path=self.root/'container.exe'
        for kernel,crt in (('KERNEL32.dll','msvcrt.dll'),('KeRnEl32.DlL','MSVCRT.DLL')):
            path.write_bytes(container_pe(((kernel,CONTRACT_APIS),(crt,('__C_specific_handler',)),DEFAULT_IMPORTS[2])))
            GATE.pe_admit(path,variant=1)
            path.write_bytes(container_pe(((kernel,CONTRACT_APIS[:2]+CONTRACT_APIS[3:]),(crt,('__C_specific_handler',)),DEFAULT_IMPORTS[2])))
            with self.assertRaisesRegex(GATE.GateError,'KERNEL32 缺少精确命名 RtlVirtualUnwind'):
                GATE.pe_admit(path,variant=1)

    def test_other_variants_require_virtual_unwind_regardless_of_filename(self):
        # 即使名称宣称 variant1，也只有显式可信 selector 可以选该规则。
        path = self.root / "abi-contract-1.exe"
        for variant in (0, 1, 2, 3, 4):
            with self.subTest(variant=variant):
                path.write_bytes(container_pe())
                GATE.pe_admit(path, variant=variant)
                path.write_bytes(container_pe((("KERNEL32.dll", (CONTRACT_APIS[:2] + CONTRACT_APIS[3:])), DEFAULT_IMPORTS[1], DEFAULT_IMPORTS[2])))
                with self.assertRaisesRegex(GATE.GateError, "KERNEL32 缺少精确命名 RtlVirtualUnwind"):
                    GATE.pe_admit(path, variant=variant)
        # 私有默认值只保留 variant0 的完整 required 集合。
        with self.assertRaisesRegex(GATE.GateError, "KERNEL32 缺少精确命名 RtlVirtualUnwind"):
            GATE.pe_admit(path)

    def test_variant_one_each_required_api_missing_spelling_or_ordinal_rejected(self):
        path = self.root / "container.exe"
        for index, api in enumerate(CONTRACT_APIS):
            for kind in ("missing", "spelling", "ordinal"):
                symbols = list(CONTRACT_APIS)
                if kind == "missing":
                    del symbols[index]
                else:
                    symbols[index] = api.lower() if kind == "spelling" else 1
                with self.subTest(api=api, kind=kind):
                    path.write_bytes(container_pe((("KERNEL32.dll", symbols), DEFAULT_IMPORTS[1], DEFAULT_IMPORTS[2])))
                    with self.assertRaisesRegex(GATE.GateError, "KERNEL32 缺少精确命名 " + api):
                        GATE.pe_admit(path, variant=1)

    def test_variant_one_all_three_provider_protections_remain(self):
        path = self.root / "container.exe"
        for api in CONTRACT_APIS:
            for duplicate in (False, True):
                symbols = CONTRACT_APIS if duplicate else tuple(name for name in CONTRACT_APIS if name != api)
                with self.subTest(api=api, duplicate=duplicate):
                    path.write_bytes(container_pe((("KERNEL32.dll", symbols), ("msvcrt.dll", ("__C_specific_handler", api)), DEFAULT_IMPORTS[2])))
                    error = "KERNEL32 缺少精确命名" if not duplicate else "Rtl API 必须由 KERNEL32 提供"
                    with self.assertRaisesRegex(GATE.GateError, error):
                        GATE.pe_admit(path, variant=1)

    def test_variant_one_dll_closure_is_unchanged(self):
        path = self.root / "container.exe"
        minimal = (("KERNEL32.dll", (CONTRACT_APIS[:2] + CONTRACT_APIS[3:])), DEFAULT_IMPORTS[1], DEFAULT_IMPORTS[2])
        for imports in (minimal + (("NTDLL.dll", ("RtlVirtualUnwind",)),),
                        minimal + (("other.dll", ("OtherFunction",)),),
                        (minimal[0],), (minimal[1],),
                        (("other.dll", (CONTRACT_APIS[:2] + CONTRACT_APIS[3:])), minimal[1]),
                        minimal + (("kernel32.DLL", ("RtlVirtualUnwind",)),)):
            with self.subTest(dlls=[item[0] for item in imports]):
                path.write_bytes(container_pe(imports))
                with self.assertRaisesRegex(GATE.GateError, "actual import closure 必须恰为"):
                    GATE.pe_admit(path, variant=1)

    def test_variant_selector_rejects_noninteger_bool_and_out_of_range(self):
        path = self.root / "container.exe"
        path.write_bytes(container_pe())
        for variant in (None, True, False, "1", 1.0, -1, 5, [], {}):
            with self.subTest(variant=variant):
                with self.assertRaisesRegex(GATE.GateError, "PE variant 必须为整数 0..4"):
                    GATE.pe_admit(path, variant=variant)

    def test_actual_input_missing_mutated_or_malformed_not_receipt_success(self):
        # trusted support COFF parser，手写容器只测试 bytes gate。
        support = GATE.pinned_support(SCRIPT.parents[1])
        path = self.root / "abi.obj"
        path.write_bytes(COFF)
        item = {"outcome": "compiled", "object": {"path": str(path), "sha256": GATE.sha(path), "size": len(COFF)}}
        GATE.actual_object(self.root, "abi", item, support)
        path.write_bytes(COFF[:-4] + b"xxxx")
        with self.assertRaises(GATE.GateError):
            GATE.actual_object(self.root, "abi", item, support)
        path.write_bytes(b"MZ" + COFF[2:])
        with self.assertRaises(support.GateError):
            GATE.actual_object(self.root, "abi", item, support)
        path.unlink()
        with self.assertRaises(OSError):
            GATE.actual_object(self.root, "abi", item, support)

    def test_receipt_argv_is_only_data(self):
        support = SimpleNamespace(TOOL_ROOT=Path("/opt/fixed"))
        argv = ["/opt/fixed/bin/clang", "--version"]
        for stream in ("stdout", "stderr"):
            (self.root / ("probe." + stream)).write_bytes(b"")
        value = {"argv": ["/bin/sh", "-c", "untrusted"], "cwd": str(self.root),
                 "environment": {"PATH": "/opt/fixed/bin:/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "HOME": "/tmp"},
                 "pid": 123, "elapsed_ns": 1, "wait_terminal": "exited", "returncode": 0,
                 "stdout": {"name": "probe.stdout", "sha256": GATE.sha(self.root / "probe.stdout")},
                 "stderr": {"name": "probe.stderr", "sha256": GATE.sha(self.root / "probe.stderr")}}
        GATE.write_json(self.root / "probe.receipt.json", value)
        with self.assertRaises(GATE.GateError):
            GATE.receipt(self.root, "probe", argv, self.root, support)
        value["argv"] = argv
        (self.root / "probe.receipt.json").unlink()
        GATE.write_json(self.root / "probe.receipt.json", value)
        self.assertEqual(GATE.receipt(self.root, "probe", argv, self.root, support)["argv"], argv)


class ArchiveTests(TempTests):
    def test_zip_strict_members_extra_tree_duplicate_and_hash_change(self):
        for variant in ("positive", "extra", "duplicate", "changed", "symlink"):
            folder = self.root / variant
            folder.mkdir()
            (folder / "raw.txt").write_bytes(b"actual raw bytes")
            allowed = {"raw.txt"}
            evidence = GATE.Evidence(folder, allowed)
            evidence.register(folder / "raw.txt")
            GATE.artifact(folder, allowed, evidence)
            if variant == "positive":
                GATE.verify_bundle(folder, {"raw.txt"}, GATE.SCHEMA)
                continue
            if variant == "extra":
                (folder / "unknown.txt").write_bytes(b"untrusted")
            elif variant == "changed":
                (folder / "raw.txt").write_bytes(b"changed")
            elif variant == "symlink":
                (folder / "raw.txt").unlink()
                (folder / "raw.txt").symlink_to(self.root / "positive/raw.txt")
            else:
                with self.assertWarns(UserWarning):
                    with zipfile.ZipFile(folder / "evidence.zip", "a") as archive:
                        archive.writestr("raw.txt", b"actual raw bytes")
                record = GATE.load_json(folder / "archive.json")
                record["sha256"] = GATE.sha(folder / "evidence.zip")
                (folder / "archive.json").unlink()
                GATE.write_json(folder / "archive.json", record)
            with self.assertRaises(GATE.GateError):
                GATE.verify_bundle(folder, {"raw.txt"}, GATE.SCHEMA)

    def test_tar_rejects_alias_traversal_duplicate_hardlink_and_special(self):
        for index, names in enumerate((("fixed", "fixed/ok"), ("fixed", "fixed/../out"),
                                      ("fixed", "fixed/./ok"), ("fixed", "fixed//ok"),
                                      ("fixed", "fixed/ok", "fixed/ok"), ("other",),
                                      ("fixed", "fixed/hardlink"), ("fixed", "fixed/fifo"),
                                      ("fixed", "fixed/absolute-symlink"), ("fixed", "fixed/escape-symlink"))):
            archive_path = self.root / ("archive-%d.tar.xz" % index)
            with tarfile.open(archive_path, "w:xz") as archive:
                for name in names:
                    member = tarfile.TarInfo(name)
                    if name in ("fixed", "other"):
                        member.type = tarfile.DIRTYPE
                    elif name.endswith("hardlink"):
                        member.type, member.linkname = tarfile.LNKTYPE, "fixed/ok"
                    elif name.endswith("fifo"):
                        member.type = tarfile.FIFOTYPE
                    elif name.endswith("absolute-symlink"):
                        member.type, member.linkname = tarfile.SYMTYPE, "/outside"
                    elif name.endswith("escape-symlink"):
                        member.type, member.linkname = tarfile.SYMTYPE, "../../outside"
                    archive.addfile(member)
            support = SimpleNamespace(ARCHIVE_NAME="fixed.tar.xz", ARCHIVE_SHA=GATE.sha(archive_path))
            if index == 0:
                self.assertEqual(GATE.archive_inventory(archive_path, support)["ok"]["kind"], "file")
            else:
                with self.assertRaises(GATE.GateError):
                    GATE.archive_inventory(archive_path, support)


class StopOnFailureTests(TempTests):
    def model(self, failure):
        source, install, out = (self.root / "source", self.root / "install", self.root / "out")
        for folder in (source, install, out):
            folder.mkdir()
        calls = []
        support = SimpleNamespace(CC=Path("/opt/tool/clang"), READOBJ=Path("/opt/tool/readobj"),
                                  TOOL_ROOT=Path("/opt/tool"),
                                  TARGET="aarch64-w64-mingw32", host_gate=lambda: None,
                                  coff_admit=lambda path: {"machine": "0xAA64"},
                                  readobj_agrees=lambda text, path: None)
        def require_zero(record):
            if record["wait_terminal"] != "exited" or record["returncode"] != 0:
                raise GATE.GateError("modeled command failed")
        support.require_zero = require_zero
        def terminal(folder, label, argv, **kwargs):
            # orchestration 故障模型只保存事实，不执行工具或 native。
            kwargs["validator"]()
            calls.append(label)
            (folder / (label + ".stdout")).write_bytes(b"")
            (folder / (label + ".stderr")).write_bytes(b"modeled failure diagnostic")
            record = {"argv": argv, "wait_terminal": "exited", "returncode": 0, "pid": 1, "failure": None}
            for stream in ("stdout", "stderr"):
                record[stream] = {"sha256": GATE.sha(folder / (label + "." + stream))}
            if label.endswith("-compile"):
                if failure != "missing-object":
                    (folder / "driver-0.obj").write_bytes(COFF)
                if failure == "compile-failure":
                    record["returncode"] = 1
            if label.endswith("-link") and failure == "link-failure":
                (folder / "abi-contract-0.exe").write_bytes(b"partial failed linker bytes")
                (folder / "abi-contract-0.map").write_bytes(b"partial map")
                record["returncode"] = 1
            GATE.write_json(folder / (label + ".receipt.json"), record)
            if failure == "registered-json-drift" and label == "variant-0-compile-expansion":
                (folder / "authorization.json").write_bytes(b'{"altered":"between commands"}\n')
            kwargs["validator"]()
            return record
        support.terminal = terminal
        summary = {"variants": [], "not_attempted": list(range(5)), "status": "failed"}
        allowed = set(GATE.FIXED_MEMBERS)
        evidence = GATE.Evidence(out, allowed)
        with mock.patch.object(GATE, "source_snapshot", return_value={}), mock.patch.object(GATE, "admit_inputs", return_value=({}, {}, {}, {})), mock.patch.object(GATE, "tools_match", return_value={}), mock.patch.object(GATE, "CRT", ()):
            with self.assertRaises((GATE.GateError, OSError)):
                GATE.build(self.root, source, install, out, support, summary, allowed, evidence)
        self.assertEqual(summary["not_attempted"], [1, 2, 3, 4])
        self.assertTrue(all(label.startswith("variant-0-") for label in calls))
        self.assertEqual(len(calls), len(set(calls)))
        self.assertTrue((out / (calls[-1] + ".receipt.json")).is_file())
        self.assertEqual((out / (calls[-1] + ".stderr")).read_bytes(), b"modeled failure diagnostic")
        self.assertNotEqual(summary["status"], "compiled-only")
        return calls, out, summary

    def test_compile_failure_preserves_partial_object_and_no_next_variant(self):
        calls, out, summary = self.model("compile-failure")
        self.assertEqual(calls, ["variant-0-compile-expansion", "variant-0-compile"])
        self.assertEqual((out / "driver-0.obj").read_bytes(), COFF)
        self.assertEqual(summary["first_command_failure"]["label"], "variant-0-compile")

    def test_link_failure_preserves_partial_pe_map_and_no_next_variant(self):
        calls, out, summary = self.model("link-failure")
        self.assertEqual(calls[-1], "variant-0-link")
        self.assertEqual((out / "abi-contract-0.exe").read_bytes(), b"partial failed linker bytes")
        self.assertEqual((out / "abi-contract-0.map").read_bytes(), b"partial map")
        self.assertEqual(summary["first_command_failure"]["label"], "variant-0-link")

    def test_zero_compile_but_missing_actual_output_stops(self):
        calls, out, summary = self.model("missing-object")
        self.assertEqual(calls[-1], "variant-0-compile")
        self.assertFalse((out / "driver-0.obj").exists())

    def test_zero_link_but_missing_actual_pe_stops(self):
        calls, out, summary = self.model("missing-pe")
        self.assertEqual(calls[-1], "variant-0-link")
        self.assertFalse((out / "abi-contract-0.exe").exists())

    def test_registered_json_drift_in_post_validator_stops_before_compile_or_next_variant(self):
        calls, out, summary = self.model("registered-json-drift")
        self.assertEqual(calls, ["variant-0-compile-expansion"])
        self.assertFalse((out / "driver-0.obj").exists())


class VariantAdmissionCallTests(TempTests):
    def test_each_trusted_loop_variant_is_explicitly_bound_at_pe_admission(self):
        # 每轮在 PE admission 主动停止；只检测参数传递，不授予 compile/native 成功。
        self.assertEqual(GATE.VARIANTS, (0, 1, 2, 3, 4))
        seen = []
        for expected in range(5):
            with self.subTest(variant=expected):
                base = self.root / str(expected)
                source, install, out = (base / "source", base / "install", base / "out")
                for folder in (source, install, out):
                    folder.mkdir(parents=True)
                calls = []
                support = SimpleNamespace(CC=Path("/opt/tool/clang"), READOBJ=Path("/opt/tool/readobj"),
                                          TOOL_ROOT=Path("/opt/tool"), TARGET="aarch64-w64-mingw32",
                                          host_gate=lambda: None, coff_admit=lambda path: {},
                                          readobj_agrees=lambda text, path: None)
                def terminal(folder, label, argv, **kwargs):
                    kwargs["validator"]()
                    calls.append(label)
                    if label.endswith("-compile"):
                        self.assertIn("-DFRD_CONTRACT_VARIANT=%d" % expected, argv)
                        (folder / ("driver-%d.obj" % expected)).write_bytes(b"opaque modeled object")
                    elif label.endswith("-link"):
                        (folder / ("abi-contract-%d.exe" % expected)).write_bytes(b"opaque modeled PE")
                        (folder / ("abi-contract-%d.map" % expected)).write_bytes(b"opaque modeled map")
                    # 冲突的 receipt metadata 不得成为 selector；record 仍只是数据。
                    record = {"argv": argv, "variant": (expected + 1) % 5,
                              "wait_terminal": "exited", "returncode": 0}
                    for stream in ("stdout", "stderr"):
                        path = folder / (label + "." + stream)
                        path.write_bytes(b"orchestration-only model")
                        record[stream] = {"sha256": GATE.sha(path)}
                    GATE.write_json(folder / (label + ".receipt.json"), record)
                    kwargs["validator"]()
                    return record
                def stop_at_admission(path, *, variant):
                    self.assertEqual(path, out / ("abi-contract-%d.exe" % expected))
                    self.assertIs(type(variant), int)
                    self.assertEqual(variant, expected)
                    seen.append(variant)
                    raise GATE.GateError("modeled stop at explicit variant admission")
                support.terminal = terminal
                summary = {"variants": [], "not_attempted": list(range(5)), "status": "failed"}
                allowed = set(GATE.FIXED_MEMBERS)
                evidence = GATE.Evidence(out, allowed)
                with mock.patch.object(GATE, "VARIANTS", (expected,)), mock.patch.object(GATE, "source_snapshot", return_value={}), mock.patch.object(GATE, "admit_inputs", return_value=({}, {}, {}, {})), mock.patch.object(GATE, "tools_match", return_value={}), mock.patch.object(GATE, "CRT", ()), mock.patch.object(GATE, "pe_admit", side_effect=stop_at_admission) as admission, mock.patch.object(GATE, "bindings") as bindings:
                    with self.assertRaisesRegex(GATE.GateError, "modeled stop at explicit variant admission"):
                        GATE.build(self.root, source, install, out, support, summary, allowed, evidence)
                admission.assert_called_once_with(out / ("abi-contract-%d.exe" % expected), variant=expected)
                bindings.assert_not_called()
                self.assertEqual(calls, ["variant-%d-%s" % (expected, suffix) for suffix in
                                        ("compile-expansion", "compile", "object-readobj", "link-expansion", "link", "pe-readobj")])
                self.assertEqual(summary["not_attempted"], [value for value in range(5) if value != expected])
                self.assertEqual(summary["status"], "failed")
                self.assertEqual([item["status"] for item in summary["variants"]], ["failed"])
                self.assertFalse((out / "terminal-status.json").exists())
        self.assertEqual(seen, [0, 1, 2, 3, 4])


class EvidenceFinalizationFailureTests(TempTests):
    def setup_evidence(self):
        out = self.root / "evidence"
        out.mkdir()
        allowed = set(GATE.FIXED_MEMBERS)
        evidence = GATE.Evidence(out, allowed)
        # 只模拟进入封装前的 orchestration 状态；不是 compiler/Windows 成功证据。
        summary = {"schema": GATE.SCHEMA, "status": "compiled-only", "failures": [],
                   "scope": "fault-injected packaging state only; native never evaluated"}
        return out, allowed, evidence, summary

    def assert_failed(self, out, result):
        self.assertEqual(result, 1)
        summary = GATE.load_json(out / "summary.json")
        terminal = GATE.load_json(out / "terminal-status.json")
        failure = GATE.load_json(out / "archive-failure.json")
        self.assertEqual(summary["status"], "failed")
        self.assertTrue(summary["failures"])
        self.assertEqual(terminal["status"], "failed")
        self.assertFalse(terminal["archive_verified"])
        self.assertEqual(terminal["archive_failure"]["phase"], "archive-finalization")
        self.assertEqual(failure["first_failure"], summary["first_failure"])
        self.assertTrue(failure["partial_archive_is_not_accepted"])

    def test_registered_source_copy_uses_original_pin_and_drift_is_not_refreshed(self):
        out, allowed, evidence, summary = self.setup_evidence()
        path = out / "inputs/reviewed.c"
        path.parent.mkdir()
        original = b"reviewed source bytes"
        path.write_bytes(original)
        pin = hashlib.sha256(original).hexdigest()
        evidence.register(path, pin=pin)
        path.write_bytes(b"unapproved changed source")
        with self.assertRaises(GATE.GateError):
            evidence.check()
        with self.assertRaises(GATE.GateError):
            evidence.register(path)  # 不允许重新 hash 接纳漂移。
        self.assertEqual(evidence.expected[path]["sha256"], pin)
        with contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        self.assert_failed(out, result)
        self.assertFalse((out / "evidence.zip").exists())

    def test_registered_bindings_and_dynamic_argv_json_drift_is_rejected(self):
        for name in ("variant-0-pe-bindings.json", "variant-0-disassemble-main.argv.json"):
            folder = self.root / name.removesuffix(".json")
            folder.mkdir()
            allowed = set(GATE.FIXED_MEMBERS)
            evidence = GATE.Evidence(folder, allowed)
            evidence.write(name, {"generated": "frozen real-material identity"})
            pin = evidence.expected[folder / name]["sha256"]
            (folder / name).write_bytes(b'{"generated":"tampered"}\n')
            with self.assertRaises(GATE.GateError):
                evidence.check()
            with self.assertRaises(GATE.GateError):
                GATE.artifact(folder, allowed, evidence)
            self.assertEqual(evidence.expected[folder / name]["sha256"], pin)
            self.assertFalse((folder / "evidence.zip").exists())

    def test_final_actual_member_mutated_after_bundle_verification_is_not_accepted(self):
        out, allowed, evidence, summary = self.setup_evidence()
        path = out / "driver-0.obj"
        path.write_bytes(b"opaque actual-member bytes for identity-only failure control")
        evidence.register(path)
        pin = evidence.expected[path]["sha256"]
        original_verify = GATE.verify_bundle
        def verify_then_mutate(*args, **kwargs):
            value = original_verify(*args, **kwargs)
            path.write_bytes(b"changed after ZIP/current-file consistency check")
            return value
        with mock.patch.object(GATE, "verify_bundle", side_effect=verify_then_mutate), contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        self.assert_failed(out, result)
        self.assertEqual(evidence.expected[path]["sha256"], pin)
        self.assertTrue((out / "evidence.zip").is_file())  # 原失败 attempt 不删除、不重封装。

    def test_archive_exception_persists_failed_terminal_summary_first_error_and_raw(self):
        out, allowed, evidence, summary = self.setup_evidence()
        raw = out / "variant-0-link.stderr"
        raw.write_bytes(b"preserved raw first-attempt diagnostic")
        evidence.register(raw)
        def archive_failure(*args):
            self.assertEqual(GATE.load_json(out / "summary.json")["status"], "packaging-pending")
            (out / "evidence.zip").write_bytes(b"partial ZIP attempt")
            raise OSError("modeled archive write exception")
        with mock.patch.object(GATE, "artifact", side_effect=archive_failure) as archive, contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        archive.assert_called_once()
        self.assert_failed(out, result)
        persisted = GATE.load_json(out / "summary.json")
        self.assertEqual(persisted["first_failure"]["message"], "modeled archive write exception")
        self.assertEqual(raw.read_bytes(), b"preserved raw first-attempt diagnostic")
        self.assertEqual((out / "evidence.zip").read_bytes(), b"partial ZIP attempt")

    def test_archive_failure_keeps_existing_build_first_error(self):
        out, allowed, evidence, summary = self.setup_evidence()
        previous = {"phase": "build", "type": "GateError", "message": "original compiler failure"}
        summary.update({"status": "failed", "first_failure": previous, "failures": [previous]})
        with mock.patch.object(GATE, "artifact", side_effect=OSError("later archive exception")), contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        self.assert_failed(out, result)
        persisted = GATE.load_json(out / "summary.json")
        self.assertEqual(persisted["first_failure"], previous)
        self.assertEqual(persisted["failures"][0], previous)
        self.assertEqual(persisted["failures"][1]["phase"], "archive-finalization")

    def combined_finalization_failure(self, fault):
        out, allowed, evidence, summary = self.setup_evidence()
        raw = out / "variant-0-link.stderr"
        raw.write_bytes(b"opaque raw bytes; orchestration fault control only")
        evidence.register(raw)
        original_verify = GATE.verify_bundle
        original_failed = GATE.controlled_failed_json
        checks = {"verification": 0, "terminal_writes": 0}
        def verify(*args, **kwargs):
            checks["verification"] += 1
            if checks["verification"] == 2:
                self.assertFalse((out / "terminal-status.json").exists())
                self.assertTrue((out / GATE.TERMINAL_PRECOMMIT).is_file())
                if fault == "second-verification":
                    raise OSError("modeled second final verification failure")
            return original_verify(*args, **kwargs)
        def input_check():
            if fault == "final-input" and checks["verification"] == 2:
                self.assertFalse((out / "terminal-status.json").exists())
                raise OSError("modeled final input check failure")
        def failed_write(folder, name, value):
            if name == "terminal-status.json":
                checks["terminal_writes"] += 1
                raise OSError("modeled blocked failed-terminal persistence")
            return original_failed(folder, name, value)
        evidence.input_validator = input_check
        with mock.patch.object(GATE, "verify_bundle", side_effect=verify), mock.patch.object(GATE, "controlled_failed_json", side_effect=failed_write), mock.patch.object(GATE, "commit_terminal") as commit, contextlib.redirect_stdout(io.StringIO()) as output:
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        commit.assert_not_called()
        self.assertEqual(result, 1)
        self.assertEqual(checks["verification"], 2)
        self.assertEqual(checks["terminal_writes"], 1)
        self.assertFalse((out / "terminal-status.json").exists())  # 不需要撤回已发布 success。
        self.assertEqual(GATE.load_json(out / "summary.json")["status"], "failed")
        failure = GATE.load_json(out / "archive-failure.json")
        self.assertFalse(failure["archive_verified"])
        self.assertEqual(failure["archive_failure"]["phase"], "archive-finalization")
        self.assertEqual(GATE.load_json(out / GATE.TERMINAL_PRECOMMIT)["authority_filename"], "terminal-status.json")
        self.assertEqual(raw.read_bytes(), b"opaque raw bytes; orchestration fault control only")
        self.assertTrue((out / "evidence.zip").is_file())
        record = json.loads(output.getvalue())
        self.assertEqual(record["status"], "failed")
        self.assertEqual([item["file"] for item in record["persistence_errors"]], ["terminal-status.json"])

    def test_second_final_verification_and_failed_terminal_write_failure_leave_no_success_authority(self):
        self.combined_finalization_failure("second-verification")

    def test_final_input_check_and_failed_terminal_write_failure_leave_no_success_authority(self):
        self.combined_finalization_failure("final-input")

    def test_atomic_publish_failure_and_failed_terminal_write_failure_leave_authority_absent(self):
        out, allowed, evidence, summary = self.setup_evidence()
        original_failed = GATE.controlled_failed_json
        def failed_write(folder, name, value):
            if name == "terminal-status.json":
                raise OSError("modeled failed-terminal persistence blocked")
            return original_failed(folder, name, value)
        with mock.patch.object(GATE.os, "rename", side_effect=OSError("modeled atomic publication failure")) as rename, mock.patch.object(GATE, "controlled_failed_json", side_effect=failed_write), contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        rename.assert_called_once_with(out / GATE.TERMINAL_PRECOMMIT, out / "terminal-status.json")
        self.assertEqual(result, 1)
        self.assertFalse((out / "terminal-status.json").exists())
        self.assertTrue((out / GATE.TERMINAL_PRECOMMIT).is_file())
        self.assertEqual(GATE.load_json(out / "summary.json")["status"], "failed")
        failure = GATE.load_json(out / "archive-failure.json")
        self.assertFalse(failure["archive_verified"])
        self.assertEqual(failure["archive_failure"]["message"], "modeled atomic publication failure")


if __name__ == "__main__":
    unittest.main()
