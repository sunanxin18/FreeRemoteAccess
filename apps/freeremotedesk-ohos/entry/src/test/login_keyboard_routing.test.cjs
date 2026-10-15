// 使用 SDK 的真实 ArkTS AST 验证绑定范围，并执行源码提取的回调；不加载设备或凭据。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const modulePath = process.env.FRD_ARKTS_MODULE;
assert.ok(modulePath, 'FRD_ARKTS_MODULE 必须指向 SDK ets-loader 的 typescript/lib/typescript.js');
const ts = require(modulePath);
assert.ok(ts.ScriptKind.ETS !== undefined, '必须使用支持 ArkTS 的 SDK parser');
const loader = path.resolve(path.dirname(require.resolve(modulePath)), '../../..');
const options = ts.readConfigFile(path.join(loader, 'tsconfig.json'), p => fs.readFileSync(p, 'utf8')).config.compilerOptions;
const indexPath = path.resolve(__dirname, '../main/ets/pages/Index.ets');
const source = fs.readFileSync(indexPath, 'utf8');
const ast = ts.createSourceFile(indexPath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.ETS, options);
assert.equal(ast.parseDiagnostics.length, 0);
const calls = [];
const methods = new Map();
function visit(node) {
  if (ts.isMethodDeclaration(node)) methods.set(node.name.getText(ast), node);
  if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) calls.push(node);
  ts.forEachChild(node, visit);
}
visit(ast);
function owner(node) { while (node && !ts.isMethodDeclaration(node)) node = node.parent; return node?.name.getText(ast); }
function componentRoot(call) {
  let node = call;
  while (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) node = node.expression.expression;
  return ts.isEtsComponentExpression(node) || ts.isCallExpression(node) ? node.expression.getText(ast) : '';
}
let count = 0;
function test(name, run) { run(); count++; console.log(`PASS ${name}`); }
test('登录表单及祖先没有下发拦截，唯一拦截绑定远程XComponent', () => {
  const dispatch = calls.filter(c => c.expression.name.text === 'onKeyEventDispatch');
  assert.equal(dispatch.length, 1);
  assert.equal(owner(dispatch[0]), 'remoteSurface');
  assert.equal(componentRoot(dispatch[0]), 'XComponent');
  assert.equal(calls.filter(c => c.expression.name.text === 'onKeyPreIme').length, 0);
});
test('地址使用URL输入模式，端口和密码保留各自模式，用户名保持普通文本', () => {
  function inputField(call) {
    let node = call;
    while (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) node = node.expression.expression;
    if (!(ts.isEtsComponentExpression(node) || ts.isCallExpression(node)) || node.expression.getText(ast) !== 'TextInput') return '';
    const props = node.arguments?.[0];
    if (!props || !ts.isObjectLiteralExpression(props)) return '';
    return props.properties.find(p => ts.isPropertyAssignment(p) && p.name.getText(ast) === 'text')?.initializer.getText(ast) || '';
  }
  const types = calls.filter(c => c.expression.name.text === 'type' && componentRoot(c) === 'TextInput');
  assert.deepEqual(types.filter(c => inputField(c) === 'this.address').map(c => c.arguments[0].getText(ast)), ['InputType.URL']);
  assert.deepEqual(types.filter(c => inputField(c) === 'this.port').map(c => c.arguments[0].getText(ast)), ['InputType.Number']);
  assert.equal(types.filter(c => inputField(c) === 'this.username').length, 0);
  assert.deepEqual(types.filter(c => owner(c) === 'passwordInput').map(c => c.arguments[0].getText(ast)), ['InputType.Password']);
});
const bubble = calls.find(c => owner(c) === 'build' && c.expression.name.text === 'onKeyEvent');
assert.ok(bubble, '根容器需普通冒泡回调保留真实尾键清理');
assert.equal(componentRoot(bubble), 'Stack');
const passwordKey = calls.find(c => owner(c) === 'passwordInput' && c.expression.name.text === 'onKeyEvent');
const passwordSubmit = calls.find(c => owner(c) === 'passwordInput' && c.expression.name.text === 'onSubmit');
function callbackMethod(name, call) {
  const arrow = call.arguments[0]; assert.ok(ts.isArrowFunction(arrow));
  return `${name}(${arrow.parameters.map(p => p.getText(ast)).join(',')}) ${arrow.body.getText(ast)}`;
}
function compile(text, scope = {}) {
  const compiled = ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true });
  assert.equal(compiled.diagnostics.length, 0);
  const exports = {}; vm.runInNewContext(compiled.outputText, { exports, ...scope }); return exports;
}
const policy = compile(fs.readFileSync(path.resolve(__dirname, '../main/ets/common/SessionIslandPolicy.ets'), 'utf8'));
const mapping = compile(fs.readFileSync(path.resolve(__dirname, '../main/ets/pages/InputMapping.ets'), 'utf8'));
const sent = [], actions = [], timers = [];
const native = { appInput: (...a) => { sent.push(a); return 0; }, appAction: (...a) => { actions.push(a); return 0; } };
const extracted = ['dispatchKey', 'syncQueuedKeyEpoch', 'flushPendingKeyUps', 'submit'].map(n => methods.get(n).getText(ast)).join('\n');
const { Harness } = compile(`class Harness { ${extracted}\n${callbackMethod('bubble', bubble)}\n${callbackMethod('passwordKey', passwordKey)}\n${callbackMethod('passwordSubmit', passwordSubmit)}\n}\nexports.Harness = Harness;`,
  { native, usbHid: mapping.usbHid, qualifiedPhysicalKey: mapping.qualifiedPhysicalKey, KeyType: { Down: 0, Up: 1 }, IslandInput: { Keyboard: 2 }, setTimeout: cb => timers.push(cb) });
function harness(page = 0) {
  const h = new Harness();
  Object.assign(h, { model: { page, canConnect: true, session: '101', generation: '1', geometryRevision: '2' },
    keyboard: new policy.ShellKeyboardOwnership(), queuedKeys: new policy.QueuedRemoteKeys(), shellActive: true,
    check() {}, localOwnership() { this.keyboard.enterLocal(); }, revealIsland() { this.reveals = (this.reveals || 0) + 1; this.keyboard.enterLocal(); },
    focusIslandAfterLayout() {}, returnToSurface() { this.returns = (this.returns || 0) + 1; }, submitEvent: 0 });
  return h;
}
function event(code, down, modifiers = []) { return { keyCode: code, type: down ? 0 : 1, metaKey: 0, stopped: 0,
  getModifierKeyState: keys => keys.every(k => modifiers.includes(k)), stopPropagation() { this.stopped++; } }; }
test('登录数字/字母/句点/Meta/Tab事件保持本地，不阻止冒泡或发送远程', () => {
  sent.length = 0; const h = harness();
  for (const code of [2000,2001,2009,2017,2044,2049,2076]) for (const down of [true,false]) {
    const e = event(code, down); h.bubble(e); assert.equal(e.stopped, 0);
  }
  assert.equal(sent.length, 0);
});
test('登录页真实尾Up仍按原会话清理隔离但不吃本地按键', () => {
  sent.length = 0; const h = harness(); h.queuedKeys.setEpoch('91','4','3'); h.queuedKeys.recordDown(2001,30,true); h.queuedKeys.isolate();
  const e = event(2001,false); h.bubble(e); assert.equal(e.stopped,0); assert.equal(sent.length,1);
  assert.equal(sent[0][0],'91'); assert.equal(sent[0][1],'4'); assert.equal(sent[0][2],'3'); assert.equal(h.queuedKeys.hasPending(),false);
});
test('密码Enter同一按住周期只提交一次，根冒泡不重复提交', () => {
  actions.length = 0; const h = harness(); const down = event(2054,true); h.passwordKey(down); h.bubble(down);
  h.passwordSubmit(); h.passwordSubmit(); assert.equal(actions.length,1); assert.equal(down.stopped,0);
  while(timers.length) timers.shift()(); h.passwordSubmit(); assert.equal(actions.length,1);
  const up = event(2054,false); h.passwordKey(up); h.bubble(up);
  h.passwordKey(event(2054,true)); h.passwordSubmit(); assert.equal(actions.length,2);
});
test('浮动岛本地Escape和CtrlAltHome仍消费且重复Home不重开', () => {
  const h = harness(3), escape = event(2070,true); h.bubble(escape); assert.equal(h.returns,1); assert.equal(escape.stopped,1);
  const home = event(2081,true,['Ctrl','Alt']); h.bubble(home); h.bubble(event(2081,true,['Ctrl','Alt'])); assert.equal(h.reveals,1); assert.equal(home.stopped,1);
});
console.log(`${count} login keyboard routing cases passed`);
