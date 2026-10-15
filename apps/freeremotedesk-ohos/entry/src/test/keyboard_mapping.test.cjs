// SDK 的 KeyCode 是输入，独立 USB HID fixture 是期望；执行真实 mapping 与 Index consumer。
const assert = require('node:assert/strict');
const test = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const modulePath = process.env.FRD_ARKTS_MODULE;
assert.ok(modulePath, 'FRD_ARKTS_MODULE 必须指向实际 SDK ArkTS loader');
const ts = require(modulePath);
const sdk = path.resolve(path.dirname(require.resolve(modulePath)), '../../../../../api/@ohos.multimodalInput.keyCode.d.ts');
const declarations = fs.readFileSync(sdk, 'utf8');
const codes = Object.fromEntries([...declarations.matchAll(/\b(KEYCODE_[A-Z0-9_]+)\s*=\s*(\d+)/g)].map(m => [m[1], Number(m[2])]));
function compile(text, scope = {}) {
  const result = ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true });
  assert.equal(result.diagnostics.length, 0);
  const exports = {}; vm.runInNewContext(result.outputText, { exports, ...scope }); return exports;
}
const mapping = compile(fs.readFileSync(path.resolve(__dirname, '../main/ets/pages/InputMapping.ets'), 'utf8'));
const policy = compile(fs.readFileSync(path.resolve(__dirname, '../main/ets/common/SessionIslandPolicy.ets'), 'utf8'));
const index = path.resolve(__dirname, '../main/ets/pages/Index.ets');
const source = fs.readFileSync(index, 'utf8');
const loader = path.resolve(path.dirname(require.resolve(modulePath)), '../../..');
const options = ts.readConfigFile(path.join(loader, 'tsconfig.json'), p => fs.readFileSync(p, 'utf8')).config.compilerOptions;
const ast = ts.createSourceFile(index, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.ETS, options);
assert.equal(ast.parseDiagnostics.length, 0);
const methods = new Map();
function visit(node) { if (ts.isMethodDeclaration(node)) methods.set(node.name.getText(ast), node); ts.forEachChild(node, visit); }
visit(ast);
const sent = [];
const native = { appInput: (...args) => { sent.push(args); return 0; } };
const extracted = ['dispatchKey', 'syncQueuedKeyEpoch', 'flushPendingKeyUps'].map(name => methods.get(name).getText(ast)).join('\n');
const { Harness } = compile(`class Harness { ${extracted} }\nexports.Harness = Harness;`,
  { native, ...mapping, KeyType: { Down: 0, Up: 1 }, IslandInput: { Keyboard: 2 } });
function harness() {
  const h = new Harness();
  Object.assign(h, { model: { page: 3, session: '101', generation: '1', geometryRevision: '2' },
    keyboard: new policy.ShellKeyboardOwnership(), queuedKeys: new policy.QueuedRemoteKeys(), shellActive: true,
    remoteFocused: true, detailsOpen: false, check() {}, localOwnership() { this.keyboard.enterLocal(); },
    release() { this.releases = (this.releases || 0) + 1; }, revealIsland() { this.keyboard.enterLocal(); }, focusIslandAfterLayout() {} });
  h.keyboard.enterRemote(); return h;
}
function code(name) { assert.ok(codes[`KEYCODE_${name}`] !== undefined, `SDK 缺少 ${name}`); return codes[`KEYCODE_${name}`]; }
function event(name, down, mods = [], numLock = false) { return { keyCode: typeof name === 'number' ? name : code(name),
  type: down ? 0 : 1, metaKey: 0, numLock, getModifierKeyState: keys => keys.every(k => mods.includes(k)) }; }
// 由 USB HID Keyboard/Keypad usage 固定值独立给出，不从 mapping 反算。
const fixtures = [
  ...'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('').map((name, i) => [name, 0x04 + i]),
  ['1',0x1e],['2',0x1f],['3',0x20],['4',0x21],['5',0x22],['6',0x23],['7',0x24],['8',0x25],['9',0x26],['0',0x27],
  ['ENTER',0x28],['ESCAPE',0x29],['DEL',0x2a],['TAB',0x2b],['SPACE',0x2c],['MINUS',0x2d],['EQUALS',0x2e],
  ['LEFT_BRACKET',0x2f],['RIGHT_BRACKET',0x30],['BACKSLASH',0x31],['SEMICOLON',0x33],['APOSTROPHE',0x34],['GRAVE',0x35],
  ['COMMA',0x36],['PERIOD',0x37],['SLASH',0x38],['CAPS_LOCK',0x39],
  ...Array.from({length:12},(_,i) => [`F${i+1}`,0x3a+i]),
  ['SYSRQ',0x46],['SCROLL_LOCK',0x47],['INSERT',0x49],['MOVE_HOME',0x4a],['PAGE_UP',0x4b],['FORWARD_DEL',0x4c],
  ['MOVE_END',0x4d],['PAGE_DOWN',0x4e],['DPAD_RIGHT',0x4f],['DPAD_LEFT',0x50],['DPAD_DOWN',0x51],['DPAD_UP',0x52],
  ['NUM_LOCK',0x53],['NUMPAD_DIVIDE',0x54],['NUMPAD_MULTIPLY',0x55],['NUMPAD_SUBTRACT',0x56],['NUMPAD_ADD',0x57],['NUMPAD_ENTER',0x58],
  ['NUMPAD_1',0x59],['NUMPAD_2',0x5a],['NUMPAD_3',0x5b],['NUMPAD_4',0x5c],['NUMPAD_5',0x5d],['NUMPAD_6',0x5e],
  ['NUMPAD_7',0x5f],['NUMPAD_8',0x60],['NUMPAD_9',0x61],['NUMPAD_0',0x62],['NUMPAD_DOT',0x63],['102ND',0x64],['MENU',0x65],
  ['CTRL_LEFT',0xe0],['SHIFT_LEFT',0xe1],['ALT_LEFT',0xe2],['CTRL_RIGHT',0xe4],['SHIFT_RIGHT',0xe5],['ALT_RIGHT',0xe6]
];
test('实际 SDK 主键区、小键盘、左右修饰与编辑键精确映射并平衡下发', () => {
  for (const [name, hid] of fixtures) {
    assert.equal(mapping.usbHid(code(name)), hid, name);
    sent.length = 0; const h = harness(); h.dispatchKey(event(name,true)); h.dispatchKey(event(name,false));
    assert.deepEqual(sent.map(a => [a[3],a[4],a[7],a[8]]), name === '102ND' ? [] : [[2,1,hid,0],[2,0,hid,0]], name);
  }
});
test('NumLock 两态不改写小键盘物理 HID，修饰位保持 Shift Ctrl Alt', () => {
  for (const numLock of [false,true]) {
    sent.length = 0; const h = harness();
    h.dispatchKey(event('NUMPAD_1',true,['Shift','Ctrl','Alt'],numLock));
    h.dispatchKey(event('NUMPAD_1',false,['Shift','Ctrl','Alt'],numLock));
    assert.deepEqual(sent.map(a => [a[4],a[7],a[8]]), [[1,0x59,7],[0,0x59,7]]);
  }
});
test('Pause 和未资格键不生成远端事件，后续普通键仍可用', () => {
  for (const name of ['BREAK','102ND','NUMPAD_COMMA','NUMPAD_EQUALS','NUMPAD_LEFT_PAREN','NUMPAD_RIGHT_PAREN','NUMPAD_PLUSMINUS','FN',999999]) {
    sent.length = 0; const h = harness(); h.dispatchKey(event(name,true)); h.dispatchKey(event(name,false));
    assert.equal(sent.length,0, `${name}`);
    h.dispatchKey(event('A',true)); h.dispatchKey(event('A',false)); assert.equal(sent.length,2);
  }
});
test('未资格旧来源真实 Up 仍先清理，登录保持 false', () => {
  for (const page of [0,3]) {
    sent.length = 0; const h = harness(); h.model.page = page;
    h.queuedKeys.setEpoch('91','4','3'); h.queuedKeys.recordDown(code('BREAK'),0x48,true); h.queuedKeys.isolate();
    assert.equal(h.dispatchKey(event('BREAK',false)), page !== 0);
    assert.equal(sent.length,1); assert.deepEqual(sent[0].slice(0,5),['91','4','3',2,0]); assert.equal(sent[0][7],0x48);
    assert.equal(h.queuedKeys.hasPending(),false);
  }
});
test('重复回调、local、失焦与系统快捷键不产生重复或越权远程输入', () => {
  sent.length=0; const h=harness();
  h.dispatchKey(event('A',true)); h.dispatchKey(event('A',false)); h.dispatchKey(event('A',false)); assert.equal(sent.length,2);
  for (const configure of [h => h.keyboard.enterLocal(),h => h.remoteFocused=false,h => h.shellActive=false,h => h.model.page=0]) {
    sent.length=0; const h=harness(); configure(h); h.dispatchKey(event('A',true)); h.dispatchKey(event('A',false)); assert.equal(sent.length,0);
  }
  sent.length=0; const meta=harness(); meta.dispatchKey(event('META_LEFT',true)); meta.dispatchKey(event('A',true)); assert.equal(sent.length,0); assert.equal(meta.releases,1);
  sent.length=0; const sas=harness(); sas.dispatchKey(event('FORWARD_DEL',true,['Ctrl','Alt'])); assert.equal(sent.length,0);
});
test('收窄资格后当前来源仍持有的键真实 Up 走清理，不留下 ledger', () => {
  sent.length=0; const h=harness(); h.syncQueuedKeyEpoch();
  h.keyboard.key(code('BREAK'),true); h.queuedKeys.recordDown(code('BREAK'),0x48,true);
  h.dispatchKey(event('BREAK',false));
  assert.equal(sent.length,1); assert.equal(sent[0][4],0); assert.equal(sent[0][7],0x48);
  assert.equal(h.queuedKeys.allowsDown(code('BREAK')),true);
});
test('孤立未资格 Up 不释放旁侧合法 held，重复未资格 Down 不进入队列', () => {
  sent.length=0; const h=harness(); h.dispatchKey(event('A',true));
  h.dispatchKey(event('BREAK',false)); h.dispatchKey(event(999999,false));
  h.dispatchKey(event('BREAK',true)); h.dispatchKey(event('BREAK',true)); h.dispatchKey(event('BREAK',false));
  h.dispatchKey(event('A',false));
  assert.deepEqual(sent.map(a => [a[4],a[7]]),[[1,4],[0,4]]);
});
