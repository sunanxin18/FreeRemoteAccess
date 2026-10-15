// Real registered Ability callbacks -> actual Index methods -> actual IME adapter.
// Native/SDK endpoints model reviewed contracts; this is a host consumer, not HAP runtime proof.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require(process.env.FRD_ARKTS_MODULE);
const base = path.resolve(__dirname, '../main/ets');
const loader = path.resolve(path.dirname(require.resolve(process.env.FRD_ARKTS_MODULE)), '../../..');
const parserOptions = ts.readConfigFile(path.join(loader, 'tsconfig.json'), p => fs.readFileSync(p, 'utf8')).config.compilerOptions;
function compile(source, scope = {}, fileName = 'consumer.ts') {
  const result = ts.transpileModule(source, { fileName, compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020
  }, reportDiagnostics: true });
  assert.equal(result.diagnostics.length, 0);
  const exports = {};
  vm.runInNewContext(result.outputText, { exports, ...scope });
  return exports;
}
function moduleFile(file, scope = {}) { return compile(fs.readFileSync(path.join(base, file), 'utf8'), scope); }
const policy = moduleFile('common/RemoteImePolicy.ets');
const island = moduleFile('common/SessionIslandPolicy.ets');
const displayPolicy = moduleFile('common/LocalDisplayPolicy.ets');
const fullscreen = moduleFile('common/PhoneFullscreenPolicy.ets');
const orientation = moduleFile('common/PhoneOrientationPolicy.ets');
const indexFile = path.join(base, 'pages/Index.ets');
const indexAst = ts.createSourceFile(indexFile, fs.readFileSync(indexFile, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.ETS, parserOptions);
assert.equal(indexAst.parseDiagnostics.length, 0);
const methods = new Map();
const properties = new Map();
const calls = [];
function visit(node) {
  if (ts.isMethodDeclaration(node)) methods.set(node.name.getText(indexAst), node);
  if (ts.isPropertyDeclaration(node)) properties.set(node.name.getText(indexAst), node);
  if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) calls.push(node);
  ts.forEachChild(node, visit);
}
visit(indexAst);
function owner(node) { while (node && !ts.isMethodDeclaration(node)) node = node.parent; return node?.name.getText(indexAst); }
const callbackMethods = ['onFocus', 'onBlur', 'onAreaChange'].map(event => {
  const call = calls.find(call => owner(call) === 'remoteSurface' && call.expression.name.text === event);
  const arrow = call.arguments[0];
  return `${event}(${arrow.parameters.map(p => p.getText(indexAst)).join(',')})${ts.isBlock(arrow.body) ? arrow.body.getText(indexAst) : '{' + arrow.body.getText(indexAst) + ';}'}`;
}).join('\n');
const methodNames = ['geometry', 'focus', 'release', 'localOwnership', 'observeIme', 'toggleKeyboard',
  'surfaceFocusChanged', 'acceptSurfaceReturn', 'refreshSurfaceFocus', 'surfaceAreaChanged', 'surfaceInteraction', 'syncQueuedKeyEpoch', 'flushPendingKeyUps',
  'syncIsland', 'shellActivityChanged', 'refreshIsland', 'closeDetails', 'collapseIsland', 'aboutToDisappear',
  'setImeNotice', 'clearImeNotice'];
const settle = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); };
function harness() {
  const events = { focus: [], sources: [], operations: [], logs: [], attach: 0, detach: 0, geometry: 0, display: 0, held: false };
  const listeners = new Map();
  let h;
  let validDisplay = false;
  let nativeFocused = false;
  let nodeFocused = true;
  const native = {
    initializeApp() {}, shutdownApp() {},
    remoteSurfaceIdentity(surfaceId) { return surfaceId === '9007199254740993' ? '7:11' : ''; },
    remoteSurfaceFocusToken(surfaceId) { return surfaceId === '9007199254740993' && nodeFocused ? '7:11:1' : ''; },
    appFocus(foreground, focused, remote) {
      events.focus.push([foreground, focused, remote]);
      nativeFocused = foreground && focused && remote;
      if (h) {
        h.model.ime.sourceAvailable = nativeFocused && validDisplay;
        if (!nativeFocused) { h.model.ime.state = 'detached'; h.model.ime.ticket = null; events.held = false; }
      }
      return 0;
    },
    setForeground(active) { if (!active) native.appFocus(false, false, false); },
    appLocalDisplay(json) {
      const message = JSON.parse(json); events.display++;
      validDisplay = !!message.geometry && message.windowId > 0;
      if (h) {
        h.model.localDisplayEvidence = validDisplay ? { stage: message.stage, windowId: message.windowId } : undefined;
        h.model.ime.sourceAvailable = validDisplay && nativeFocused;
      }
      return 0;
    },
    appImeSource(json) {
      const source = JSON.parse(json); events.sources.push(source);
      if (source.state === 'detached') { h.model.ime.state = 'detached'; h.model.ime.ticket = null; }
      return 0;
    },
    appImeOperation(json) { events.operations.push(JSON.parse(json)); return 0; },
    imeGraphemeNeighbors() { return { isBoundary: true, previous: 0, next: 0 }; },
    appGeometry() { events.geometry++; events.held = false; return 0; },
    appInput() { return 0; }
  };
  const controller = {
    on(name, listener) { if(!controller.bound)throw {code:12800009}; listeners.set(name, listener); },
    off(name, listener) { assert.equal(listeners.get(name), listener); listeners.delete(name); },
    attachWithUIContext(_context, _config, options) { assert.equal(options.showKeyboard, false); events.attach++; return Promise.resolve().then(() => { controller.bound = true; }); },
    detach() { events.detach++; return Promise.resolve(); },
    changeSelection() { return Promise.resolve(); },
    showTextInput() { assert.equal(listeners.size,14); return Promise.resolve(); }
  };
  const inputMethod = { getController: () => controller, TextInputType: { TEXT: 0 }, EnterKeyType: { NEWLINE: 8 } };
  const adapter = moduleFile('common/RemoteImeAdapter.ets', {
    setTimeout, clearTimeout, require: name => name === './RemoteImePolicy' ? policy : { inputMethod }
  });
  const hilog = { info(...args) { events.logs.push(args); }, warn() {}, error() {} };
  const Index = compile(`class IndexHarness {
    ${properties.get('ime').getText(indexAst)}
    ${methodNames.filter(name => methods.has(name)).map(name => methods.get(name).getText(indexAst)).join('\n')}
    ${callbackMethods}
  } exports.IndexHarness = IndexHarness;`, {
    native, inputMethod, RemoteImeAdapter: adapter.RemoteImeAdapter, hilog,
    deviceInfo: { deviceType: 'phone' }, IslandPin: island.IslandPin, clearInterval
  }).IndexHarness;
  h = new Index();
  Object.assign(h, {
    model: { page: 3, session: '100', generation: '1', geometryRevision: '2', ime: {
      installed: true, protocolTextInput: true, sourceAvailable: false, state: 'detached', ticket: null,
      lastAcceptedSerial: 0, lastDispatchedSerial: 0
    } },
    island: new island.SessionIslandPolicy(), keyboard: new island.ShellKeyboardOwnership(),
    queuedKeys: new island.QueuedRemoteKeys(), surfaceTouch: new island.SurfaceTouchOwnership(),
    detailsScope: new island.DialogCallbackScope(), controller: { getXComponentSurfaceId: () => '9007199254740993' }, surfaceFocusId: '', surfaceFocusToken: '', shellActive: false, surfaceHasFocus: false,
    remoteFocused: false, detailsOpen: false, actionPage: 3, focusTicket: 0, actionBusy: false,
    bridgeNotice: '', imeNotice: '', imeCloseIntent: false, timer: -1, zoomScale: 1, panX: 0, panY: 0,
    surfaceWidth: 100, surfaceHeight: 60, now: () => 10,
    check(code) { assert.equal(code, 0); },
    getUIContext() { return { vp2px: value => value, getFocusController: () => ({ requestFocus() {} }) }; }
  });
  function makeWindow(id) {
    const handlers = {};
    const properties = { id, displayId: 1, isLayoutFullScreen: false,
      windowRect: { left: 0, top: 0, width: 100, height: 60 },
      drawableRect: { left: 0, top: 0, width: 100, height: 60 } };
    return { handlers, properties, on(event, fn) { handlers[event] = fn; }, off(event) { delete handlers[event]; },
      getWindowProperties() { return properties; },
      getWindowAvoidArea() { return { leftRect: { width: 0 }, rightRect: { width: 0 }, topRect: { height: 0 }, bottomRect: { height: 0 } }; }
    };
  }
  const screen = { id: 1, width: 100, height: 60, densityPixels: 1, rotation: 0, on() {}, off() {},
    getAvailableArea() { return new Promise(() => {}); } };
  class UIAbility { context = { filesDir: '/nonsecret/mock', eventHub: { on() {}, off() {} },
    getApplicationContext: () => ({ setColorMode() {} }) }; }
  const window = { AvoidAreaType: { TYPE_CUTOUT: 1, TYPE_SYSTEM: 2, TYPE_KEYBOARD: 3 },
    WindowEventType: { WINDOW_ACTIVE: 1, WINDOW_INACTIVE: 2 }, Orientation: {} };
  const moduleMap = {
    '@kit.AbilityKit': { UIAbility, ConfigurationConstant: { ColorMode: { COLOR_MODE_NOT_SET: 0 } } },
    '@kit.ArkUI': { window, display: { getDisplayByIdSync: () => screen, on() {}, off() {} } },
    '@kit.PerformanceAnalysisKit': { hilog }, '@kit.BasicServicesKit': { deviceInfo: { deviceType: 'phone' } },
    'libentry.so': { default: native }, '../common/PhoneFullscreenPolicy': fullscreen,
    '../common/PhoneOrientationPolicy': orientation, '../common/LocalDisplayPolicy': displayPolicy
  };
  const Ability = moduleFile('entryability/EntryAbility.ets', {
    require: name => { assert.ok(moduleMap[name], name); return moduleMap[name]; }, canIUse: () => false,
    AppStorage: { setOrCreate(key, value) {
      if (key === 'frd_window_active' && h.shellActive !== value) { h.shellActive = value; h.shellActivityChanged(); }
    } }
  }).default;
  const ability = new Ability();
  ability.onCreate(); ability.onForeground();
  const win = makeWindow(7);
  function attachWindow(current) { ability.onWindowStageCreate({ getMainWindowSync: () => current, loadContent(_page, done) { done({ code: 0 }); } }); }
  attachWindow(win);
  h.syncIsland(); h.onFocus(); h.surfaceInteraction();
  return { h, events, listeners, native, ability, win, makeWindow, attachWindow,
    blur() { nodeFocused = false; native.appFocus(h.shellActive, false, false); h.observeIme(); },
    async open() {
      h.toggleKeyboard(); await settle();
      const source = events.sources.filter(source => source.state === 'attached').at(-1);
      assert.ok(source, 'actual adapter publishes source after SDK attach');
      h.model.ime.state = 'attached'; h.model.ime.ticket = source.ticket; h.observeIme(); await settle();
      assert.equal(h.ime.loading(), false); return source.ticket;
    },
    async close() { h.ime.close(); await settle(); }
  };
}
let count = 0;
async function test(name, run) {
  if (process.env.FRD_IME_FILTER && !name.includes(process.env.FRD_IME_FILTER)) return;
  await run(); count++; console.log('PASS ' + name);
}
(async () => {
  await test('registered avoidAreaChange keeps ready ticket and no SDK detach', async () => {
    const f = harness();
    try {
      const ticket = await f.open(); const before = f.events.focus.length;
      f.win.handlers.avoidAreaChange({ type: 3, area: {} }); f.h.observeIme(); await settle();
      assert.equal(f.h.ime.mode(), true, 'keyboard avoidance is not a real blur');
      assert.equal(f.h.ime.loading(), false);
      assert.deepEqual(f.h.model.ime.ticket, ticket);
      assert.equal(f.events.detach, 0); assert.equal(f.events.focus.length, before);
    } finally { await f.close(); }
  });
  await test('registered windowSizeChange plus real Index resize keeps source and cleans held geometry', async () => {
    const f = harness();
    try {
      const ticket = await f.open();
      f.h.syncQueuedKeyEpoch(); f.h.queuedKeys.recordDown(2017, 4, true); f.events.held = true;
      f.win.properties.windowRect.height = 40; f.win.properties.drawableRect.height = 40;
      f.win.handlers.windowSizeChange({ width: 100, height: 40 });
      f.h.onAreaChange({}, { width: 100, height: 40 }); f.h.observeIme(); await settle();
      assert.equal(f.h.ime.mode(), true); assert.deepEqual(f.h.model.ime.ticket, ticket);
      assert.equal(f.events.detach, 0); assert.equal(f.events.held, false); assert.equal(f.events.geometry, 1);
      assert.equal(f.h.queuedKeys.allowsDown(2017), false, 'old held key remains isolated until real tail Up');
      assert.equal(f.h.queuedKeys.hasPending(), false, 'no synthetic Up before the real tail event');
      assert.equal(f.h.queuedKeys.tailUp(2017), true); assert.equal(f.h.flushPendingKeyUps(), true);
      assert.equal(f.h.queuedKeys.hasPending(), false);
    } finally { await f.close(); }
  });
  await test('real inactive background destroy and surface blur still close the current source', async () => {
    for (const kind of ['inactive', 'background', 'destroy', 'blur']) {
      const f = harness();
      try {
        await f.open();
        if (kind === 'inactive') f.win.handlers.windowEvent(2);
        if (kind === 'background') f.ability.onBackground();
        if (kind === 'destroy') f.ability.onWindowStageDestroy();
        if (kind === 'blur') f.blur();
        f.h.observeIme(); await settle();
        assert.equal(f.h.ime.mode(), false, kind); assert.equal(f.events.detach, 1, kind);
        assert.equal(f.events.sources.at(-1).state, 'detached', kind);
      } finally { await f.close(); }
    }
  });
  await test('old window callbacks cannot refresh or deactivate a replacement stage', async () => {
    const f = harness();
    try {
      await f.open(); const stale = { ...f.win.handlers };
      f.ability.onWindowStageDestroy(); await settle();
      const replacement = f.makeWindow(8); f.attachWindow(replacement);
      f.h.surfaceInteraction(); await f.open();
      const before = f.events.focus.length; const displayBefore = f.events.display; const ticket = f.h.model.ime.ticket;
      stale.avoidAreaChange({ type: 3, area: {} }); stale.windowSizeChange({ width: 1, height: 1 }); stale.windowEvent(2);
      stale.windowRectChange(); stale.displayIdChange();
      f.h.observeIme(); await settle();
      assert.equal(f.h.shellActive, true); assert.equal(f.h.ime.mode(), true);
      assert.equal(f.events.focus.length, before); assert.equal(f.events.display, displayBefore); assert.deepEqual(f.h.model.ime.ticket, ticket);
    } finally { await f.close(); }
  });
  await test('unknown invalid window geometry stays closed', async () => {
    const f = harness();
    try {
      await f.open(); f.win.properties.displayId = undefined; f.win.handlers.windowSizeChange({});
      f.h.observeIme(); await settle(); assert.equal(f.h.ime.mode(), false);
    } finally { await f.close(); }
  });
  await test('new explicit retry clears only the prior IME notice; form return drops obsolete IME status', async () => {
    for (const unrelated of [false, true]) {
      const f = harness();
      try {
        f.blur(); f.h.remoteFocused = false;
        f.h.toggleKeyboard(); f.h.now = () => 2101; f.h.observeIme();
        assert.ok(f.h.bridgeNotice.includes('系统输入法'));
        if (unrelated) f.h.bridgeNotice = '持久连接错误';
        f.h.now = () => 2200; f.h.toggleKeyboard();
        assert.equal(f.h.bridgeNotice, unrelated ? '持久连接错误' : '');
        f.h.ime.close();
        // A second failed request must be distinguishable from the cleared first failure.
        f.h.toggleKeyboard(); f.h.now = () => 4300; f.h.observeIme();
        assert.ok(f.h.bridgeNotice.includes('系统输入法'));
        if (unrelated) f.h.bridgeNotice = '持久连接错误';
        f.h.model.page = 0; f.h.syncIsland();
        assert.equal(f.h.bridgeNotice, unrelated ? '持久连接错误' : '');
      } finally { await f.close(); }
    }
  });
  await test('actual Index adapter initializer publishes numeric-only bounded HilOG stages', async () => {
    const f = harness();
    try {
      await f.open();
      const logs = f.events.logs.filter(args => args[1] === 'FrdIme');
      assert.ok(logs.length >= 8); assert.ok(logs.length <= 25);
      for (const args of logs) for (const value of args.slice(3)) assert.ok(typeof value === 'number');
      const before = logs.length;
      for (let i = 0; i < 100; i++) f.h.observeIme();
      assert.equal(f.events.logs.filter(args => args[1] === 'FrdIme').length, before);
    } finally { await f.close(); }
  });
  console.log(`${count} real Ability lifecycle tests passed`);
})().catch(error => { console.error(error); process.exitCode = 1; });
