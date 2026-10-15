// 只运行纯 shell 策略；不加载设备、窗口、会话或凭据。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require(process.env.FRD_TYPESCRIPT_MODULE || 'typescript');
const source = fs.readFileSync(path.resolve(__dirname, '../main/ets/common/SessionIslandPolicy.ets'), 'utf8');
const result = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 }, reportDiagnostics: true, fileName: 'SessionIslandPolicy.ts' });
assert.equal((result.diagnostics || []).length, 0);
const output = {}; vm.runInNewContext(result.outputText, { exports: output });
const { SessionIslandPolicy: Policy, IslandPin: Pin, IslandInput: Input, ShellKeyboardOwnership: Keyboard } = output;
let count = 0;
function test(name, body) { if (process.env.FRD_POLICY_TEST_FILTER && !name.includes(process.env.FRD_POLICY_TEST_FILTER)) return; body(); count++; console.log(`PASS ${name}`); }
function connected(now = 0) { const policy = new Policy(); policy.observe(3, 'a', true, now); return policy; }
test('成功空闲3秒收起，非成功阶段始终显示', () => {
 const p = connected(); assert.equal(p.expanded(), true); p.advance(2999, p.epoch()); assert.equal(p.expanded(), true); p.advance(3000, p.epoch()); assert.equal(p.expanded(), false);
 for (const page of [1, 2, 4, 5]) { p.observe(page, '', true, 4000); p.advance(999999, p.epoch()); assert.equal(p.expanded(), true); }
});
test('鼠标显式展开后离开700ms，重入取消deadline', () => {
 const p = connected(); p.reveal(Input.Mouse, 0); p.pin(Pin.Hover, true, 10); p.pin(Pin.Hover, false, 100);
 p.advance(799, p.epoch()); assert.equal(p.expanded(), true); p.pin(Pin.Hover, true, 799); p.advance(9000, p.epoch()); assert.equal(p.expanded(), true);
 p.pin(Pin.Hover, false, 9001); p.advance(9701, p.epoch()); assert.equal(p.expanded(), false);
});
test('触摸抬起3秒收起，按住不收起', () => {
 const p = connected(); p.reveal(Input.Touch, 0); p.pin(Pin.Press, true, 0); p.advance(9000, p.epoch()); assert.equal(p.expanded(), true);
 p.pin(Pin.Press, false, 9001); p.advance(12000, p.epoch()); assert.equal(p.expanded(), true); p.advance(12001, p.epoch()); assert.equal(p.expanded(), false);
});
test('键盘焦点/提示/详情/错误分别pin，最后离开才倒计时', () => {
 for (const pin of [Pin.Focus, Pin.Tooltip, Pin.Details, Pin.Error]) {
  const p = connected(); p.reveal(Input.Mouse, 0); p.pin(pin, true, 0); p.advance(9000, p.epoch()); assert.equal(p.expanded(), true);
  p.pin(pin, false, 9001); p.advance(9701, p.epoch()); assert.equal(p.expanded(), false);
 }
 const p = connected(); p.pin(Pin.Hover, true, 0); p.pin(Pin.Focus, true, 0); p.pin(Pin.Hover, false, 1); p.advance(9000, p.epoch()); assert.equal(p.expanded(), true);
});
test('后台与回登录清理状态，旧世代不能隐藏新会话', () => {
 const p = connected(); const old = p.epoch(); p.pin(Pin.Details, true, 0); p.observe(3, 'a', false, 100); assert.equal(p.shown(), false);
 p.observe(3, 'b', true, 200); assert.notEqual(p.epoch(), old); p.advance(999999, old); assert.equal(p.expanded(), true);
 p.advance(3200, p.epoch()); assert.equal(p.expanded(), false); p.observe(0, '', true, 3300); assert.equal(p.shown(), false);
 p.observe(1, '', true, 3400); assert.equal(p.expanded(), true);
});
test('同会话轮询不重置deadline，失败返回连接开始新世代', () => {
 const p = connected(); const epoch = p.epoch(); p.observe(3, 'a', true, 2900); assert.equal(p.epoch(), epoch); p.advance(3000, epoch); assert.equal(p.expanded(), false);
 p.observe(5, 'a', true, 4000); const failed = p.epoch(); p.observe(1, '', true, 4100); assert.notEqual(p.epoch(), failed);
});
test('显式收起不会关闭详情或正在按下的动作', () => {
 const p = connected(); p.pin(Pin.Details, true, 0); p.collapse(); assert.equal(p.expanded(), true); p.pin(Pin.Details, false, 1); p.pin(Pin.Press, true, 2); p.collapse(); assert.equal(p.expanded(), true); p.pin(Pin.Press, false, 3); p.collapse(); assert.equal(p.expanded(), false);
});
test('键盘Local/Remote与快捷键尾键隔离，不靠timer恢复', () => {
 const k = new Keyboard(); k.enterRemote(); assert.equal(k.canSend(), true);
 k.key(2072, true); k.key(2045, true); k.key(2081, true); k.enterLocal(); assert.equal(k.canSend(), false);
 k.enterRemote(); assert.equal(k.canSend(), false); assert.equal(k.key(2081, false), true); assert.equal(k.key(2072, false), true); assert.equal(k.canSend(), false); assert.equal(k.key(2045, false), true); assert.equal(k.canSend(), true);
});
test('本地普通键不进入远程，按住本地键回surface继续隔离', () => {
 const k = new Keyboard(); k.enterLocal(); k.key(2054, true); k.enterRemote(); assert.equal(k.canSend(), false); assert.equal(k.key(2054, false), true); assert.equal(k.canSend(), true);
 k.enterLocal(); k.key(2017, true); k.key(2017, false); k.enterRemote(); assert.equal(k.canSend(), true);
});
test('失活清理held键并保持Local，不把旧键带入新会话', () => {
 const k = new Keyboard(); k.enterRemote(); k.key(2072, true); k.reset(); assert.equal(k.canSend(), false); k.enterRemote(); assert.equal(k.canSend(), true);
});
test('Home重复down可识别，抬起后下一次首down重新可用', () => {
 const k = new Keyboard(); assert.equal(k.isHeld(2081), false); k.key(2081, true); assert.equal(k.isHeld(2081), true); k.enterLocal(); k.key(2081, true); assert.equal(k.isHeld(2081), true); k.key(2081, false); assert.equal(k.isHeld(2081), false);
});
test('双Meta独立跟踪，失焦后Up仍清除且重置无残留', () => {
 const k = new Keyboard(); k.enterRemote(); k.key(2076, true); assert.equal(k.systemModifierHeld(), true); k.enterLocal(); k.key(2077, true); k.key(2076, false); assert.equal(k.systemModifierHeld(), true); k.key(2077, false); assert.equal(k.systemModifierHeld(), false); k.enterRemote(); assert.equal(k.canSend(), true); k.key(2076, true); k.reset(); assert.equal(k.systemModifierHeld(), false);
});
const { SurfaceTouchOwnership: Touch, SurfaceTouchPhase: Phase, SurfaceTouchDecision: Decision, DialogCallbackScope: Dialog } = output;
test('多点后单点Move/Up不能复活，全部结束后新单点Down恢复', () => {
 const touch = new Touch(); const k = new Keyboard();
 function event(phase, contacts) { const decision = touch.observe(phase, contacts); if (decision === Decision.Release) k.enterLocal(); if (decision === Decision.Restore) k.enterRemote(); return decision; }
 assert.equal(event(Phase.Down, 1), Decision.Restore); assert.equal(k.canSend(), true);
 assert.equal(event(Phase.Down, 2), Decision.Release); assert.equal(k.canSend(), false);
 event(Phase.Up, 1); event(Phase.Move, 1); assert.equal(k.canSend(), false);
 event(Phase.Down, 1); assert.equal(k.canSend(), false);
 event(Phase.Up, 0); event(Phase.Move, 0); assert.equal(k.canSend(), false);
 assert.equal(event(Phase.Down, 1), Decision.Restore); assert.equal(k.canSend(), true);
});
test('Cancel立即失去输入，尾部Move/Up不恢复，新起点可恢复', () => {
 const touch = new Touch(); touch.observe(Phase.Down, 1);
 assert.equal(touch.observe(Phase.Cancel, 0), Decision.Release);
 assert.equal(touch.observe(Phase.Move, 1), Decision.Ignore); assert.equal(touch.observe(Phase.Up, 0), Decision.Ignore);
 assert.equal(touch.observe(Phase.Down, 1), Decision.Restore);
 touch.reset(); assert.equal(touch.observe(Phase.Move, 1), Decision.Ignore);
});
test('旧详情键回调在重连/后台/关闭后不污染新keyboard状态', () => {
 const dialog = new Dialog(); const k = new Keyboard();
 const old = dialog.begin(1); assert.equal(dialog.observeKey(old, 1, true, k, 2076, true), true); assert.equal(k.systemModifierHeld(), true);
 dialog.invalidate(); k.reset(); const current = dialog.begin(2);
 dialog.observeKey(old, 2, true, k, 2076, true); assert.equal(k.systemModifierHeld(), false);
 dialog.observeKey(current, 2, false, k, 2081, true); assert.equal(k.isHeld(2081), false);
 dialog.observeKey(current, 3, true, k, 2076, true); assert.equal(k.systemModifierHeld(), false);
 assert.equal(dialog.observeKey(current, 2, true, k, 2054, true), true); assert.equal(k.isHeld(2054), true);
 dialog.invalidate(); k.reset(); dialog.observeKey(current, 2, true, k, 2076, true); assert.equal(k.systemModifierHeld(), false);
});
const { QueuedRemoteKeys: Keys } = output;
test('已入队Ctrl/Alt尾Up清理两端隔离，本地Home不进入ABI', () => {
 const keys = new Keys(); const backendHeld = new Set(); const backendQuarantine = new Set(); const remote = [];
 keys.setEpoch('101', '7', '12');
 for (const [code, hid] of [[2072,224],[2045,226]]) { keys.recordDown(code,hid,true); backendHeld.add(hid); remote.push(['down',hid]); }
 keys.isolate(); for (const hid of backendHeld) backendQuarantine.add(hid); backendHeld.clear();
 assert.equal(keys.tailUp(2081), false); assert.equal(keys.tailUp(2072), true); assert.equal(keys.tailUp(2045), true);
 for (const item of keys.pending()) { assert.equal(backendQuarantine.delete(item.hid), true); keys.completeCleanup(item,true); }
 assert.equal(keys.hasPending(),false); assert.equal(backendQuarantine.size,0); assert.equal(remote.length,2);
 // 与现有Rust route一致：隔离Up不产生remote命令，随后全新Ctrl/Alt不再被隔离。
 for (const hid of [224,226]) { assert.equal(backendQuarantine.has(hid),false); remote.push(['new-down',hid]); }
 assert.equal(remote.length,4);
});
test('未accepted Down不建账，cleanup失败保留到显式重试', () => {
 const keys = new Keys(); keys.setEpoch('101','1','12'); keys.recordDown(2017,4,false); keys.isolate(); assert.equal(keys.tailUp(2017),false);
 keys.recordDown(2017,4,true); keys.isolate(); assert.equal(keys.tailUp(2017),true); keys.completeCleanup(keys.pending()[0],false); assert.equal(keys.hasPending(),true); assert.equal(keys.pending()[0].hid,4);
 keys.completeCleanup(keys.pending()[0],true); assert.equal(keys.hasPending(),false);
 keys.recordDown(2018,5,true); keys.normalUp(2018,false); assert.equal(keys.hasPending(),true); keys.completeCleanup(keys.pending()[0],true); assert.equal(keys.hasPending(),false);
});
test('page0无snapshot身份仍以原来源提交真实尾Up', () => {
 const keys = new Keys(); keys.setEpoch('101','7','12'); keys.recordDown(2017,4,true); keys.isolate();
 keys.setEpoch('','',''); assert.equal(keys.tailUp(2017),true);
 const pending = keys.pending()[0]; assert.equal(pending.session,'101'); assert.equal(pending.generation,'7'); assert.equal(pending.revision,'12');
 keys.completeCleanup(pending,true); assert.equal(keys.hasPending(),false);
});
test('geometry更新保留原合法材料，新session同码Down不能覆盖旧来源', () => {
 const keys = new Keys(); keys.setEpoch('101','7','12'); keys.recordDown(2017,4,true); keys.isolate();
 keys.setEpoch('101','7','44'); keys.setEpoch('202','8','2');
 assert.equal(keys.allowsDown(2017),false); keys.recordDown(2017,4,true);
 assert.equal(keys.tailUp(2017),true); const old = keys.pending()[0];
 assert.equal(old.session,'101'); assert.equal(old.generation,'7'); assert.equal(old.revision,'12');
 keys.completeCleanup(old,true); assert.equal(keys.allowsDown(2017),true);
 keys.recordDown(2017,4,true); keys.isolate(); keys.tailUp(2017);
 keys.completeCleanup(old,true); assert.equal(keys.pending()[0].session,'202');
});
test('跨page0 QueueFull清理不丢失，只显式retry成功后移除', () => {
 const keys = new Keys(); keys.setEpoch('101','7','12'); keys.recordDown(2072,224,true); keys.isolate(); keys.setEpoch('','',''); keys.tailUp(2072);
 const original = keys.pending()[0]; keys.completeCleanup(original,false); assert.equal(keys.hasPending(),true);
 keys.setEpoch('202','9','3'); assert.equal(keys.allowsDown(2072),false); assert.equal(keys.pending()[0].session,'101');
 keys.completeCleanup(keys.pending()[0],true); assert.equal(keys.hasPending(),false); assert.equal(keys.allowsDown(2072),true);
});
test('无合法非零来源不建Down账，普通current Up不借旧来源删除', () => {
 const keys = new Keys(); keys.setEpoch('101','7','0'); keys.recordDown(2017,4,true); keys.isolate(); assert.equal(keys.tailUp(2017),false);
 keys.setEpoch('101','7','12'); keys.recordDown(2017,4,true); assert.equal(keys.allowsDown(2017),true);
 keys.setEpoch('202','8','2'); keys.normalUp(2017,true); assert.equal(keys.tailUp(2017),true); assert.equal(keys.pending()[0].session,'101');
});
test('普通current成功Up正常结账，旧隔离来源不被新current Up删除', () => {
 const keys = new Keys(); keys.setEpoch('101','7','12'); keys.recordDown(2017,4,true); keys.normalUp(2017,true); keys.isolate(); assert.equal(keys.tailUp(2017),false);
 keys.recordDown(2017,4,true); keys.isolate(); keys.setEpoch('202','8','2'); keys.normalUp(2017,true); assert.equal(keys.tailUp(2017),true); assert.equal(keys.pending()[0].session,'101');
});
test('详情只join非空行，不因缺失warning或身份产生空白段', () => {
 assert.equal(output.compactDetailsLines(['已连接','','  ','安全存储正常','','服务器指纹：abc']), '已连接\n安全存储正常\n服务器指纹：abc');
 assert.equal(output.compactDetailsLines(['已连接','','','','']), '已连接');
});
console.log(`${count} policy cases passed`);
