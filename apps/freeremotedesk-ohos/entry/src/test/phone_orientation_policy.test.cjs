// 主机侧纯策略测试；不加载窗口、NAPI、会话内容或设备。
// FRD_TYPESCRIPT_MODULE 指向 SDK 自带 typescript/lib/typescript.js 即可运行。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require(process.env.FRD_TYPESCRIPT_MODULE || 'typescript');
const sourcePath = path.resolve(__dirname, '../main/ets/common/PhoneOrientationPolicy.ets');
const result = ts.transpileModule(fs.readFileSync(sourcePath, 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  reportDiagnostics: true,
  fileName: 'PhoneOrientationPolicy.ts'
});
assert.equal((result.diagnostics || []).length, 0);
const exportsObject = {};
vm.runInNewContext(result.outputText, { exports: exportsObject });
const { PhoneOrientationPolicy: Policy, PhoneOrientation: Orientation } = exportsObject;
let cases = 0;
function test(name, body) { body(); cases++; console.log(`PASS ${name}`); }
function take(policy, orientation) {
  const request = policy.nextRequest();
  assert.ok(request);
  assert.equal(request.orientation, orientation);
  return request;
}
function initial() {
  const policy = new Policy();
  policy.attachWindow();
  const request = take(policy, Orientation.Portrait);
  assert.equal(policy.complete(request), true);
  return policy;
}

test('登录前默认竖屏，轮询不反复申请', () => {
  const policy = initial();
  for (let i = 0; i < 5; i++) { policy.observe(0, ''); assert.equal(policy.nextRequest(), undefined); }
});
test('连接中和等待首帧不触发横屏', () => {
  const policy = initial();
  for (const page of [1, 2, 2, 5, 0]) { policy.observe(page, 'session-a'); assert.equal(policy.nextRequest(), undefined); }
});
test('仅已连接且非空会话申请横屏一次', () => {
  const policy = initial();
  policy.observe(3, ''); assert.equal(policy.nextRequest(), undefined);
  policy.observe(3, 'session-a');
  const request = take(policy, Orientation.Landscape);
  policy.complete(request);
  for (let i = 0; i < 5; i++) { policy.observe(3, 'session-a'); assert.equal(policy.nextRequest(), undefined); }
});
test('断开中和失败页保留方向，回登录后恢复竖屏', () => {
  const policy = initial();
  policy.observe(3, 'session-a'); policy.complete(take(policy, Orientation.Landscape));
  for (const page of [4, 5]) { policy.observe(page, ''); assert.equal(policy.nextRequest(), undefined); }
  policy.observe(0, ''); policy.complete(take(policy, Orientation.Portrait));
  policy.observe(0, ''); assert.equal(policy.nextRequest(), undefined);
});
test('重连同一编号在回登录后仍重新申请横屏', () => {
  const policy = initial();
  policy.observe(3, 'session-a'); policy.complete(take(policy, Orientation.Landscape));
  policy.observe(0, ''); policy.complete(take(policy, Orientation.Portrait));
  policy.observe(3, 'session-a'); policy.complete(take(policy, Orientation.Landscape));
});
test('无登录页的新会话也重新默认横屏', () => {
  const policy = initial();
  policy.observe(3, 'session-a'); policy.complete(take(policy, Orientation.Landscape));
  policy.observe(3, 'session-b'); policy.complete(take(policy, Orientation.Landscape));
});
test('请求串行并合并等待期间的最新方向', () => {
  const policy = new Policy(); policy.attachWindow();
  const first = take(policy, Orientation.Portrait);
  policy.observe(3, 'session-a'); policy.observe(0, ''); policy.observe(3, 'session-b');
  assert.equal(policy.nextRequest(), undefined);
  assert.equal(policy.complete(first), true);
  const latest = take(policy, Orientation.Landscape);
  assert.equal(latest.revision, 4);
  policy.complete(latest); assert.equal(policy.nextRequest(), undefined);
});
test('请求失败也不随轮询自动重试，可在新会话重试', () => {
  const policy = initial();
  policy.observe(3, 'session-a'); const failed = take(policy, Orientation.Landscape);
  policy.complete(failed);
  policy.observe(3, 'session-a'); assert.equal(policy.nextRequest(), undefined);
  policy.observe(3, 'session-b'); policy.complete(take(policy, Orientation.Landscape));
});
test('销毁窗口后不申请，旧完成不能消费新窗口请求', () => {
  const policy = initial(); policy.observe(3, 'session-a');
  const old = take(policy, Orientation.Landscape);
  policy.detachWindow(); assert.equal(policy.nextRequest(), undefined);
  policy.attachWindow(); const current = take(policy, Orientation.Landscape);
  assert.equal(policy.complete(old), false);
  assert.equal(policy.nextRequest(), undefined);
  assert.equal(policy.complete(current), true);
  assert.equal(policy.nextRequest(), undefined);
});
test('窗口重建补发当前偏好一次，不把等待首帧算作登录成功', () => {
  const policy = initial(); policy.observe(2, 'session-a');
  policy.detachWindow(); policy.attachWindow(); policy.complete(take(policy, Orientation.Portrait));
  policy.observe(3, 'session-a'); policy.complete(take(policy, Orientation.Landscape));
  policy.detachWindow(); policy.attachWindow(); policy.complete(take(policy, Orientation.Landscape));
  policy.observe(3, 'session-a'); assert.equal(policy.nextRequest(), undefined);
});
test('窗口销毁期间收到登录转换，重建使用竖屏', () => {
  const policy = initial(); policy.observe(3, 'session-a'); policy.complete(take(policy, Orientation.Landscape));
  policy.detachWindow(); policy.observe(0, ''); assert.equal(policy.nextRequest(), undefined);
  policy.attachWindow(); policy.complete(take(policy, Orientation.Portrait));
});
console.log(`${cases} policy cases passed`);
