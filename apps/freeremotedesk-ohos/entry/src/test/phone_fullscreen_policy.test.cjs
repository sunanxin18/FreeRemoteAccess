// SDK 编译纯策略及真实 Ability 调用顺序；窗口行为仍需模拟器验证。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require(process.env.FRD_TYPESCRIPT_MODULE || 'typescript');
function load(relative, imports = {}, globals = {}) {
  const source = fs.readFileSync(path.resolve(__dirname, '../main/ets', relative), 'utf8');
  const result = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 }, reportDiagnostics: true });
  assert.equal((result.diagnostics || []).length, 0);
  const exports = {};
  vm.runInNewContext(result.outputText, { exports, require: name => { assert.ok(name in imports, name); return imports[name]; }, ...globals });
  return exports;
}
const policyModule = load('common/PhoneFullscreenPolicy.ets');
const Policy = policyModule.PhoneFullscreenPolicy;
let cases = 0;
async function test(name, body) { await body(); cases++; console.log(`PASS ${name}`); }
function ready() { const p = new Policy(); p.attachWindow(); p.complete(p.nextRequest()); return p; }
function request(p, fullscreen) { const r = p.nextRequest(); assert.ok(r); assert.equal(r.fullscreen, fullscreen); return r; }
const deviceInfo = { deviceType: 'phone' };
const Entry = load('entryability/EntryAbility.ets', {
  '@kit.AbilityKit': { UIAbility: class {}, ConfigurationConstant: {} },
  '@kit.ArkUI': { window: {} }, '@kit.PerformanceAnalysisKit': { hilog: { warn() {} } },
  '@kit.BasicServicesKit': { deviceInfo }, 'libentry.so': { default: { appFocus() {} } },
  '../common/PhoneFullscreenPolicy': policyModule,
  '../common/LocalDisplayPolicy': load('common/LocalDisplayPolicy.ets'),
  '../common/PhoneOrientationPolicy': load('common/PhoneOrientationPolicy.ets')
}, { canIUse: () => true }).default;
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
function ability() {
  const calls = []; const a = new Entry();
  a.contentReady = true; a.phoneFullscreen.attachWindow(); a.updateCutout = () => {};
  a.mainWindow = {
    async setWindowLayoutFullScreen(value) { calls.push(`layout:${value}`); },
    async setWindowSystemBarEnable(names) { calls.push(`bars:${names.join(',')}`); },
    async setSpecificSystemBarEnabled(name, value) { calls.push(`${name}:${value}`); }
  };
  return { a, calls };
}
(async () => {
  await test('登录和等待首帧保持非沉浸，不随快照重复请求', () => {
    const p = ready(); for (const page of [0, 1, 2, 5, 3]) { p.observe(page, ''); assert.equal(p.nextRequest(), undefined); }
  });
  await test('有效已连接会话进入全屏，断开和错误页保持到登录', () => {
    const p = ready(); p.observe(3, 'a'); p.complete(request(p, true));
    for (const page of [3, 4, 5]) { p.observe(page, 'a'); assert.equal(p.nextRequest(), undefined); }
    p.observe(0, ''); p.complete(request(p, false));
  });
  await test('串行合并最新偏好且过期请求不继续执行', () => {
    const p = ready(); p.observe(3, 'a'); const r = request(p, true);
    p.observe(0, ''); assert.equal(p.isCurrent(r), false); assert.equal(p.nextRequest(), undefined);
    assert.equal(p.complete(r), true); assert.equal(p.isCurrent(request(p, false)), true);
  });
  await test('销毁后旧回调不能消费新窗口请求', () => {
    const p = ready(); p.observe(3, 'a'); const old = request(p, true);
    p.detachWindow(); assert.equal(p.isCurrent(old), false); p.attachWindow(); const current = request(p, true);
    assert.equal(p.complete(old), false); assert.equal(p.isCurrent(current), true);
  });
  await test('失败请求消费一次，回登录后重连可重新请求', () => {
    const p = ready(); p.observe(3, 'a'); p.complete(request(p, true));
    p.observe(3, 'a'); assert.equal(p.nextRequest(), undefined);
    p.observe(0, ''); p.complete(request(p, false)); p.observe(3, 'b'); request(p, true);
  });
  await test('真实 Ability 顺序开启全屏、隐藏状态三键栏和手势条', async () => {
    const { a, calls } = ability(); a.phoneFullscreen.observe(3, 'a');
    await a.setPhoneFullscreen(a.mainWindow, request(a.phoneFullscreen, true));
    assert.deepEqual(calls, ['layout:true', 'bars:', 'navigationIndicator:false']);
  });
  await test('真实 Ability 返回登录恢复布局及三个系统栏', async () => {
    const { a, calls } = ability();
    await a.setPhoneFullscreen(a.mainWindow, request(a.phoneFullscreen, false));
    assert.deepEqual(calls, ['layout:false', 'bars:status,navigation', 'navigationIndicator:true']);
  });
  await test('真实 Ability await 期间返回登录不再隐藏旧会话系统栏', async () => {
    const { a, calls } = ability(); const gate = deferred();
    a.mainWindow.setWindowLayoutFullScreen = async value => { calls.push(`layout:${value}`); if (value) { await gate.promise; } };
    a.phoneFullscreen.observe(3, 'a'); const work = a.setPhoneFullscreen(a.mainWindow, request(a.phoneFullscreen, true));
    a.phoneFullscreen.observe(0, ''); gate.resolve(); await work;
    for (let i = 0; i < 6; i++) { await Promise.resolve(); }
    assert.deepEqual(calls, ['layout:true', 'layout:false', 'bars:status,navigation', 'navigationIndicator:true']);
  });
  await test('真实 Ability 销毁窗口后旧异步操作不能写系统栏', async () => {
    const { a, calls } = ability(); const gate = deferred();
    a.mainWindow.setWindowLayoutFullScreen = async () => { calls.push('layout'); await gate.promise; };
    a.phoneFullscreen.observe(3, 'a'); const work = a.setPhoneFullscreen(a.mainWindow, request(a.phoneFullscreen, true));
    a.phoneFullscreen.detachWindow(); a.mainWindow = undefined; gate.resolve(); await work;
    assert.deepEqual(calls, ['layout']);
  });
  await test('PC 不调用手机全屏 API', () => {
    const { a, calls } = ability(); deviceInfo.deviceType = '2in1'; a.applyPhoneFullscreen(); deviceInfo.deviceType = 'phone';
    assert.deepEqual(calls, []);
  });
  console.log(`${cases} fullscreen tests passed`);
})().catch(error => { console.error(error); process.exitCode = 1; });
