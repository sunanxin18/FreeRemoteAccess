// Actual Index AST callbacks executed with bounded policy/adapter dependencies, no UI/device.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');const ts=require(process.env.FRD_ARKTS_MODULE);
const loader=path.resolve(path.dirname(require.resolve(process.env.FRD_ARKTS_MODULE)),'../../..');const parserOptions=ts.readConfigFile(path.join(loader,'tsconfig.json'),p=>fs.readFileSync(p,'utf8')).config.compilerOptions;
const p=path.resolve(__dirname,'../main/ets/pages/Index.ets'),source=fs.readFileSync(p,'utf8'),ast=ts.createSourceFile(p,source,ts.ScriptTarget.Latest,true,ts.ScriptKind.ETS,parserOptions);assert.equal(ast.parseDiagnostics.length,0);const methods=new Map(),calls=[];function visit(n){if(ts.isMethodDeclaration(n))methods.set(n.name.getText(ast),n);if(ts.isCallExpression(n)&&ts.isPropertyAccessExpression(n.expression))calls.push(n);ts.forEachChild(n,visit);}visit(ast);
function owner(n){while(n&&!ts.isMethodDeclaration(n))n=n.parent;return n?.name.getText(ast);}function callback(method,event){const c=calls.find(c=>owner(c)===method&&c.expression.name.text===event);assert.ok(c);return c.arguments[0];}
function transpile(text,scope={}){const r=ts.transpileModule(text,{fileName:'consumer.ts',compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020},reportDiagnostics:true});assert.equal(r.diagnostics.length,0);const exports={};vm.runInNewContext(r.outputText,{exports,...scope});return exports;}
const policy=transpile(fs.readFileSync(path.resolve(__dirname,'../main/ets/common/RemoteImePolicy.ets'),'utf8'));
const island=transpile(fs.readFileSync(path.resolve(__dirname,'../main/ets/common/SessionIslandPolicy.ets'),'utf8'));
const mapping=transpile(fs.readFileSync(path.resolve(__dirname,'../main/ets/pages/InputMapping.ets'),'utf8'));
const names=['geometry','release','focus','localOwnership','syncQueuedKeyEpoch','flushPendingKeyUps','dispatchKey','islandAction','surfaceFocusChanged','acceptSurfaceReturn','refreshSurfaceFocus','observeIme','toggleKeyboard','surfaceAreaChanged','setImeNotice','clearImeNotice'];
let count=0;function test(n,f){f();count++;console.log('PASS '+n);}
test('formal keyboard action and actual surface lifecycle methods exist',()=>{for(const n of names)assert.ok(methods.has(n),'normal Index integration missing '+n);assert.ok(calls.some(c=>owner(c)==='sessionIsland'&&c.expression.name.text==='islandButton'&&c.arguments[1]?.getText(ast)==='4'));});
const events=[];const native={remoteSurfaceIdentity:()=> '7:11',remoteSurfaceFocusToken:()=>native.actualFocus!==false?'7:11:1':'',appFocus:(...a)=>{events.push(['focus',...a]);return native.focusCode||0;},appGeometry:(...a)=>{events.push(['geometry',...a]);return 0;},appInput:(...a)=>{events.push(['input',...a]);return 0;}};
const body=names.filter(n=>methods.has(n)).map(n=>methods.get(n).getText(ast)).join('\n');let cb='';for(const name of ['onFocus','onBlur','onAreaChange']){const arrow=callback('remoteSurface',name);cb+=`${name}(${arrow.parameters.map(p=>p.getText(ast)).join(',')})${ts.isBlock(arrow.body)?arrow.body.getText(ast):'{'+arrow.body.getText(ast)+';}'}\n`;}
const {Harness}=transpile(`class Harness{${body}\n${cb}} exports.Harness=Harness;`,{native,deviceInfo:{deviceType:'phone'},KeyType:{Down:0,Up:1},imeOwnsKeyProducer:policy.imeOwnsKeyProducer,usbHid:mapping.usbHid,qualifiedPhysicalKey:mapping.qualifiedPhysicalKey,IslandInput:island.IslandInput});
function harness(){native.actualFocus=true;const h=new Harness();const ime={enabled:false,mode(){return this.enabled;},loading(){return false;},close(){this.enabled=false;events.push(['close']);},request(){this.enabled=true;events.push(['request']);},observe(s,focus,active){events.push(['observe',focus,active]);}};Object.assign(h,{model:{page:3,session:'100',generation:'1',geometryRevision:'2',ime:{installed:true,protocolTextInput:true}},ime,controller:{getXComponentSurfaceId:()=> '9007199254740993'},surfaceFocusId:'9007199254740993',surfaceFocusToken:'7:11:1',imeCloseIntent:false,imeSelected:false,keyboard:new island.ShellKeyboardOwnership(),queuedKeys:new island.QueuedRemoteKeys(),remoteFocused:true,surfaceHasFocus:true,shellActive:true,detailsOpen:false,surfaceWidth:10,surfaceHeight:10,zoomScale:1,panX:0,panY:0,actionBusy:false,check(){},now(){return 10;},refreshIsland(){},returnToSurface(){events.push(['return']);},closeDetails(){},collapseIsland(){},getUIContext(){return{vp2px:v=>v,getFocusController:()=>({requestFocus:()=>events.push(['requestFocus'])})};},island:{reveal(){}},bridgeNotice:''});h.keyboard.enterRemote();return h;}
function key(code,down=true){return{keyCode:code,type:down?0:1,metaKey:0,getModifierKeyState:()=>false};}
test('actual resize callback updates geometry without blur; actual blur invalidates first',()=>{events.length=0;const h=harness();h.ime.enabled=true;h.onAreaChange({}, {width:100,height:60});assert.ok(events.some(e=>e[0]==='geometry'));assert.equal(events.some(e=>e[0]==='focus'),false);assert.equal(h.ime.mode(),true);native.actualFocus=false;h.onBlur();assert.equal(events.findIndex(e=>e[0]==='close')<events.findIndex(e=>e[0]==='focus'),true);assert.equal(h.surfaceHasFocus,false);});
test('requestFocus void and fake ArkTS onFocus are not proof; native-only poll is proof',()=>{events.length=0;const h=harness();native.actualFocus=false;h.surfaceHasFocus=false;h.remoteFocused=false;h.toggleKeyboard();assert.ok(events.some(e=>e[0]==='requestFocus'));h.onFocus();assert.equal(events.filter(e=>e[0]==='observe'&&e[1]).length,0);native.actualFocus=true;h.observeIme();assert.ok(events.some(e=>e[0]==='observe'&&e[1]));});
test('keyboard close intent survives blur before pointer or keyboard activation',()=>{for(const input of ['pointer','keyboard']){events.length=0;const h=harness();h.ime.enabled=true;native.actualFocus=false;h.onBlur();assert.equal(h.ime.mode(),false);h.islandAction(4);assert.equal(events.filter(e=>e[0]==='request').length,0,input);h.islandAction(4);assert.equal(events.filter(e=>e[0]==='request').length,1,input);}});
test('system IME owns dispatch and root; plain physical mode remains and old held Up cleanup first',()=>{events.length=0;const h=harness();h.ime.enabled=true;for(const code of [2017,2054,2055,2070,2015]){assert.equal(h.dispatchKey(key(code)),true);assert.equal(h.dispatchKey(key(code,false)),true);}assert.equal(events.filter(e=>e[0]==='input').length,0);h.ime.enabled=false;h.keyboard.reset();h.keyboard.enterRemote();h.dispatchKey(key(2017));h.dispatchKey(key(2017,false));assert.equal(events.filter(e=>e[0]==='input').length,2);h.queuedKeys.recordDown(2017,4,true);h.queuedKeys.isolate();h.ime.enabled=true;h.dispatchKey(key(2017,false));assert.equal(h.queuedKeys.hasPending(),false);});
test('keyboard action retains actual already-focused surface evidence without fabricated onFocus',()=>{events.length=0;const h=harness();h.toggleKeyboard();assert.ok(events.some(e=>e[0]==='observe'&&e[1]));assert.equal(h.surfaceHasFocus,true);});
test('failed prior-mode release prevents a new IME producer',()=>{events.length=0;const h=harness();native.focusCode=-11;h.toggleKeyboard();native.focusCode=0;assert.equal(events.some(e=>e[0]==='request'),false);assert.equal(h.ime.mode(),false);});



// I1 regression: use the actual island epochs, Index ownership methods and IME
// adapter together. A request for an already-focused node emits no new onFocus.
const lifecycleNames = [...new Set([
  ...names, 'returnToSurface', 'syncIsland', 'shellActivityChanged', 'surfaceInteraction',
  'closeDetails', 'collapseIsland', 'refreshIsland', 'aboutToDisappear'
])];
const adapterSource = fs.readFileSync(
  path.resolve(__dirname, '../main/ets/common/RemoteImeAdapter.ets'), 'utf8');

async function settleLifecycle() {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}

function lifecycleHarness() {
  const observations = {
    attaches: 0, requests: 0, focusEvents: 0, actualFocus: false,
    refuseFocus: false, nativeEngine: '7', nativeGeneration: '11', focusEpoch: 1, surfaceId: '9007199254740993', clock: 0, sources: [], notices: [], nativeFocus: []
  };
  const listeners = new Map();
  const inputMethod = { TextInputType: { TEXT: 0 }, EnterKeyType: { NEWLINE: 8 } };
  const adapter = transpile(adapterSource, {
    setTimeout, clearTimeout,
    require: name => name === './RemoteImePolicy' ? policy : { inputMethod }
  });
  const controller = {
    on(name, listener) { listeners.set(name, listener); },
    off(name, listener) {
      assert.equal(listeners.get(name), listener);
      listeners.delete(name);
    },
    attachWithUIContext() {
      observations.attaches++;
      return Promise.resolve();
    },
    detach() { return Promise.resolve(); },
    changeSelection() { return Promise.resolve(); }
  };
  const ime = new adapter.RemoteImeAdapter(
    () => controller,
    {
      source: json => { observations.sources.push(JSON.parse(json)); return 0; },
      operation: () => 0
    },
    () => ({ isBoundary: true, previous: 0, next: 0 }),
    message => observations.notices.push(message)
  );
  let h;
  const lifecycleNative = {
    remoteSurfaceIdentity(surfaceId) { return surfaceId === observations.surfaceId ? `${observations.nativeEngine}:${observations.nativeGeneration}` : ''; },
    remoteSurfaceFocusToken(surfaceId) { return surfaceId === observations.surfaceId && observations.actualFocus ? `${observations.nativeEngine}:${observations.nativeGeneration}:${observations.focusEpoch}` : ''; },
    appFocus(foreground, focused) {
      observations.nativeFocus.push({ foreground, focused });
      h.model.ime.sourceAvailable = foreground && focused;
      return 0;
    },
    appInput() { return 0; },
    appGeometry() { return 0; }
  };
  const compiled = transpile(`class LifecycleHarness {
    ${lifecycleNames.filter(name => methods.has(name)).map(name => methods.get(name).getText(ast)).join('\n')}
    ${cb}
  } exports.LifecycleHarness = LifecycleHarness;`, {
    native: lifecycleNative, deviceInfo: { deviceType: 'phone' },
    IslandPin: island.IslandPin, IslandInput: island.IslandInput,
    imeOwnsKeyProducer: policy.imeOwnsKeyProducer,
    usbHid: mapping.usbHid, qualifiedPhysicalKey: mapping.qualifiedPhysicalKey,
    KeyType: { Down: 0, Up: 1 }, clearInterval
  });
  h = new compiled.LifecycleHarness();
  Object.assign(h, {
    ime,
    model: {
      page: 2, session: '100', generation: '1', geometryRevision: '2',
      localDisplayEvidence: { stage: 1, windowId: 7 },
      ime: {
        installed: true, protocolTextInput: true, sourceAvailable: false,
        state: 'detached', ticket: null, lastAcceptedSerial: 0, lastDispatchedSerial: 0
      }
    },
    island: new island.SessionIslandPolicy(),
    keyboard: new island.ShellKeyboardOwnership(),
    queuedKeys: new island.QueuedRemoteKeys(),
    surfaceTouch: new island.SurfaceTouchOwnership(),
    detailsScope: new island.DialogCallbackScope(),
    controller: { getXComponentSurfaceId: () => observations.surfaceId }, surfaceFocusId: '', surfaceFocusToken: '',
    shellActive: true, surfaceHasFocus: false, remoteFocused: false,
    detailsOpen: false, focusTicket: 0, actionPage: 2, actionBusy: false,
    islandExpanded: false, islandShown: false, tooltipText: '', bridgeNotice: '',
    imeCloseIntent: false, imeSelected: false, imeLoading: false, timer: -1,
    now: () => observations.clock,
    check(code) { assert.equal(code, 0); },
    getUIContext() {
      return {
        getFocusController: () => ({
          clearFocus() { observations.actualFocus = false; lifecycleNative.appFocus(h.shellActive, false); },
          requestFocus() {
            observations.requests++;
            if (!observations.actualFocus && !observations.refuseFocus) {
              observations.actualFocus = true;
              observations.focusEvents++; observations.focusEpoch++;
              lifecycleNative.appFocus(h.shellActive, true);
              // libraryname 只分派原生焦点回调，不调用 ArkTS onFocus。
            }
          }
        })
      };
    }
  });
  h.syncIsland();
  return {
    h, observations,
    setFocusToken(getToken) { lifecycleNative.remoteSurfaceFocusToken = getToken; },
    setIdentity(getIdentity) { lifecycleNative.remoteSurfaceIdentity = getIdentity; },
    realFocus() {
      observations.actualFocus = true;
      observations.focusEvents++;
      h.observeIme();
    },
    realBlur() {
      observations.actualFocus = false;
      // 原生 Owner 在 UI 轮询前已撤销来源。
      lifecycleNative.appFocus(h.shellActive, false);
      h.observeIme();
    },
    async acknowledgeSource() {
      await settleLifecycle();
      const source = observations.sources.filter(source => source.state === 'attached').at(-1);
      if (source) {
        h.model.ime.state = 'attached';
        h.model.ime.ticket = source.ticket;
        h.observeIme();
      }
      await settleLifecycle();
      return source?.ticket;
    },
    async close() { h.ime.close(); await settleLifecycle(); }
  };
}

async function lifecycleTest(name, run) {
  await run();
  count++;
  console.log('PASS ' + name);
}

(async () => {
  await lifecycleTest('I3 pending return cannot consume same-id native generation replacement', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); await fixture.acknowledgeSource();
      h.getUIContext = () => ({ getFocusController: () => ({
        clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; }, requestFocus() {}
      }) });
      h.toggleKeyboard(); h.observeIme();
      observations.nativeGeneration = '12'; observations.focusEpoch = 1; observations.actualFocus = true;
      h.observeIme(); await settleLifecycle();
      assert.equal(h.model.generation, '1'); assert.equal(h.surfaceFocusToken, '7:12:1');
      assert.equal(h.remoteFocused, false); assert.equal(h.model.ime.sourceAvailable, false);
      assert.equal(h.surfaceReturn, undefined); assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 1);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('I3 native engine replacement invalidates old intent, then a new explicit return succeeds without IME attach', async () => {
    for (const kind of ['engine', 'generation']) {
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      try {
        h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
        h.toggleKeyboard(); await fixture.acknowledgeSource();
        h.getUIContext = () => ({ getFocusController: () => ({
          clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; }, requestFocus() {}
        }) });
        h.toggleKeyboard(); h.observeIme();
        if (kind === 'engine') observations.nativeEngine = '8'; else observations.nativeGeneration = '12';
        observations.actualFocus = true; observations.focusEpoch = 1;
        h.observeIme(); await settleLifecycle();
        assert.equal(h.model.generation, '1'); assert.equal(h.remoteFocused, false, kind);
        assert.equal(h.surfaceReturn, undefined); assert.equal(h.ime.mode(), false);
        h.getUIContext = () => ({ getFocusController: () => ({
          clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; },
          requestFocus() { observations.actualFocus = true; observations.focusEpoch++; }
        }) });
        h.returnToSurface(); h.observeIme(); await settleLifecycle();
        assert.equal(h.remoteFocused, true, kind); assert.equal(h.model.ime.sourceAvailable, true);
        assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 1);
      } finally { await fixture.close(); }
    }
  });

  await lifecycleTest('I3 first unfocused return uses fresh identity with empty previous token and waits for actual same-lifecycle Focus', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland();
      assert.equal(h.surfaceFocusToken, ''); assert.equal(observations.actualFocus, false);
      h.getUIContext = () => ({ getFocusController: () => ({ clearFocus() {}, requestFocus() {} }) });
      h.returnToSurface(); h.observeIme(); await settleLifecycle();
      assert.equal(h.surfaceReturn.nativeIdentity, '7:11'); assert.equal(h.surfaceReturn.previousToken, '');
      assert.equal(h.surfaceHasFocus, false); assert.equal(h.remoteFocused, false);
      assert.equal(observations.attaches, 0); assert.equal(h.ime.mode(), false);
      observations.actualFocus = true; observations.focusEpoch++;
      h.observeIme(); assert.equal(h.remoteFocused, true); assert.equal(h.ime.mode(), false);
      assert.equal(observations.attaches, 0);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('I3 unknown or throwing identity closes return without requesting focus or authorizing input', async () => {
    for (const kind of ['empty', 'throw']) {
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      try {
        h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
        h.toggleKeyboard(); await fixture.acknowledgeSource();
        const before = observations.requests;
        fixture.setIdentity(() => { if (kind === 'throw') throw new Error('fixed metadata failure'); return ''; });
        h.toggleKeyboard(); h.observeIme(); await settleLifecycle();
        assert.equal(observations.requests, before, kind); assert.equal(h.remoteFocused, false);
        assert.equal(h.surfaceReturn, undefined); assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 1);
      } finally { await fixture.close(); }
    }
  });

  await lifecycleTest('I3 unseen existing focus is sampled before clearFocus and cannot be treated as a new proof from a void request', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); observations.actualFocus = true;
      assert.equal(h.surfaceFocusToken, '');
      h.getUIContext = () => ({ getFocusController: () => ({ clearFocus() {}, requestFocus() {} }) });
      h.returnToSurface(); h.observeIme();
      assert.equal(h.surfaceReturn.previousToken, '7:11:1');
      assert.equal(h.remoteFocused, false); assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 0);
      observations.focusEpoch++; h.observeIme(); assert.equal(h.remoteFocused, true);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('I3 empty previous token still rejects native engine or generation replacement and a fresh retry succeeds', async () => {
    for (const kind of ['engine', 'generation', 'prefix-collision']) {
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      try {
        h.model.page = 3; h.syncIsland();
        h.getUIContext = () => ({ getFocusController: () => ({
          clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; }, requestFocus() {}
        }) });
        h.returnToSurface(); h.observeIme(); assert.equal(h.surfaceReturn.previousToken, '');
        if (kind === 'engine') observations.nativeEngine = '8';
        else observations.nativeGeneration = kind === 'prefix-collision' ? '110' : '12';
        observations.actualFocus = true; h.observeIme();
        assert.equal(h.surfaceReturn, undefined); assert.equal(h.remoteFocused, false);
        assert.equal(h.model.generation, '1'); assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 0);
        h.getUIContext = () => ({ getFocusController: () => ({
          clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; },
          requestFocus() { observations.actualFocus = true; observations.focusEpoch++; }
        }) });
        h.returnToSurface(); h.observeIme(); assert.equal(h.remoteFocused, true);
        assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 0);
      } finally { await fixture.close(); }
    }
  });

  await lifecycleTest('I2 actual ready IME close returns through real clearFocus/requestFocus and preserves shell authorization at next poll', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); await fixture.acknowledgeSource();
      h.toggleKeyboard(); h.observeIme(); await settleLifecycle();
      assert.equal(h.surfaceHasFocus, true); assert.equal(h.ime.mode(), false);
      assert.equal(observations.attaches, 1); assert.equal(h.remoteFocused, true);
      assert.equal(h.model.ime.sourceAvailable, true);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('I2 delayed native focus after actual return restores shell only with new proof and does not reopen IME', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); await fixture.acknowledgeSource();
      h.getUIContext = () => ({ getFocusController: () => ({
        clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; },
        requestFocus() { observations.requests++; }
      }) });
      h.toggleKeyboard(); h.observeIme(); await settleLifecycle();
      assert.equal(h.remoteFocused, false); assert.equal(h.surfaceHasFocus, false);
      assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 1);
      observations.actualFocus = true; observations.focusEpoch++; h.model.ime.sourceAvailable = true;
      h.observeIme(); await settleLifecycle();
      assert.equal(h.remoteFocused, true); assert.equal(h.model.ime.sourceAvailable, true);
      assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 1);
      const before = observations.nativeFocus.length;
      for (let i = 0; i < 20; i++) h.observeIme();
      assert.equal(observations.nativeFocus.length, before);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('I2 pending actual return cannot authorize a failed, expired or superseded focus request', async () => {
    for (const kind of ['throw', 'no-proof', 'details', 'island', 'background', 'session', 'generation', 'window', 'stage', 'focus-ticket', 'expired']) {
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      try {
        h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
        h.toggleKeyboard(); await fixture.acknowledgeSource();
        h.getUIContext = () => ({ getFocusController: () => ({
          clearFocus() { observations.actualFocus = false; h.model.ime.sourceAvailable = false; },
          requestFocus() { if (kind === 'throw') throw new Error('fixed request failure'); }
        }) });
        h.toggleKeyboard(); h.observeIme();
        if (kind === 'details') { h.localOwnership(); h.detailsOpen = true; }
        if (kind === 'island') h.localOwnership();
        if (kind === 'background') { h.shellActive = false; h.shellActivityChanged(); h.shellActive = true; h.shellActivityChanged(); }
        if (kind === 'session') h.model.session = '101';
        if (kind === 'generation') h.model.generation = '2';
        if (kind === 'window') h.model.localDisplayEvidence.windowId++;
        if (kind === 'stage') h.model.localDisplayEvidence.stage++;
        if (kind === 'focus-ticket') h.focusTicket++;
        if (kind === 'expired') observations.clock = 2101;
        if (kind !== 'no-proof') { observations.actualFocus = true; observations.focusEpoch++; }
        h.observeIme(); await settleLifecycle();
        assert.equal(h.remoteFocused, false, kind); assert.equal(h.ime.mode(), false, kind);
        assert.equal(h.model.ime.sourceAvailable, false, kind); assert.equal(observations.attaches, 1, kind);
      } finally { await fixture.close(); }
    }
  });

  await lifecycleTest('libraryname native-only focus reaches SDK without any ArkTS onFocus callback', async () => {
    const fixture = lifecycleHarness();
    try {
      const { h, observations } = fixture;
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); await fixture.acknowledgeSource();
      assert.equal(observations.attaches, 1); assert.equal(h.ime.loading(), false);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('native getter failure closes current source; a fake ArkTS callback cannot reopen it', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); await fixture.acknowledgeSource();
      const original = h.controller.getXComponentSurfaceId;
      h.controller.getXComponentSurfaceId = () => { throw new Error('fixed test'); };
      h.observeIme(); await settleLifecycle();
      assert.equal(h.surfaceHasFocus, false); assert.equal(h.ime.mode(), false);
      observations.actualFocus = false; h.controller.getXComponentSurfaceId = original;
      h.onFocus(); h.observeIme(); assert.equal(observations.attaches, 1);
      assert.equal(h.surfaceHasFocus, false);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('new native surface closes the old ready ticket even if it is already focused, and needs an explicit request', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); const first = await fixture.acknowledgeSource();
      observations.surfaceId = '18446744073709551615';
      h.observeIme(); await settleLifecycle();
      assert.equal(h.ime.mode(), false); assert.equal(h.remoteFocused, false);
      for (let i = 0; i < 20; ++i) h.observeIme();
      assert.equal(observations.attaches, 1);
      h.toggleKeyboard(); const second = await fixture.acknowledgeSource();
      assert.equal(observations.attaches, 2); assert.ok(second.editorSerial > first.editorSerial);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('same surfaceId and window native generation replacement between polls closes old ticket and requires explicit open', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); const first = await fixture.acknowledgeSource();
      const id = observations.surfaceId;
      observations.nativeGeneration = '12'; // 新代际已聚焦，两次轮询之间没有 false 间隙。
      h.observeIme(); await settleLifecycle();
      assert.equal(observations.surfaceId, id); assert.equal(h.surfaceHasFocus, true);
      assert.equal(h.ime.mode(), false); assert.equal(h.remoteFocused, false);
      for (let i = 0; i < 30; i++) h.observeIme();
      assert.equal(observations.attaches, 1);
      h.toggleKeyboard(); const second = await fixture.acknowledgeSource();
      assert.equal(observations.attaches, 2); assert.ok(second.editorSerial > first.editorSerial);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('native Blur and Focus between UI polls replaces the same-generation focus identity', async () => {
    const fixture = lifecycleHarness(); const { h, observations } = fixture;
    try {
      h.model.page = 3; h.syncIsland(); fixture.realFocus(); h.surfaceInteraction();
      h.toggleKeyboard(); const first = await fixture.acknowledgeSource();
      observations.focusEpoch++; // 原生 Blur 已撤销 Owner；轮询前已再次真实 Focus。
      h.observeIme(); await settleLifecycle();
      assert.equal(h.surfaceHasFocus, true); assert.equal(h.ime.mode(), false);
      assert.equal(h.remoteFocused, false); assert.equal(observations.attaches, 1);
      h.toggleKeyboard(); const second = await fixture.acknowledgeSource();
      assert.ok(second.editorSerial > first.editorSerial); assert.equal(observations.attaches, 2);
    } finally { await fixture.close(); }
  });

  if (process.env.FRD_NATIVE_FOCUS_TRACE) {
    await lifecycleTest('I3 actual NAPI unfocused metadata and focus trace feed real Index return, reject engine replacement, then accept fresh explicit retry', async () => {
      const trace = JSON.parse(fs.readFileSync(process.env.FRD_NATIVE_FOCUS_TRACE, 'utf8'));
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      let identity = trace.unfocusedIdentity, token = trace.unfocusedToken, metadataReads = 0;
      let focusImmediately = false;
      try {
        h.model.page = 3; h.syncIsland();
        h.controller.getXComponentSurfaceId = () => trace.surfaceId;
        fixture.setIdentity(() => { metadataReads++; return identity; });
        fixture.setFocusToken(() => token);
        h.getUIContext = () => ({ getFocusController: () => ({
          clearFocus() { assert.ok(metadataReads > 0); token = ''; h.model.ime.sourceAvailable = false; },
          requestFocus() { if (focusImmediately) token = trace.newEngineRefocusedToken; }
        }) });
        h.returnToSurface(); h.observeIme();
        assert.equal(h.surfaceReturn.nativeIdentity, trace.unfocusedIdentity);
        assert.equal(h.surfaceReturn.previousToken, ''); assert.equal(h.surfaceHasFocus, false);
        assert.equal(h.remoteFocused, false); assert.equal(metadataReads, 1, 'ordinary poll does not query metadata');
        token = trace.initial; h.observeIme(); assert.equal(h.remoteFocused, true);
        assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 0);
        h.returnToSurface(); h.observeIme(); token = trace.newEngineToken;
        h.observeIme(); assert.equal(h.remoteFocused, false); assert.equal(h.surfaceReturn, undefined);
        assert.equal(h.model.generation, '1'); assert.equal(h.ime.mode(), false);
        identity = trace.newEngineIdentity; focusImmediately = true;
        h.returnToSurface(); h.observeIme(); await settleLifecycle();
        assert.equal(h.remoteFocused, true); assert.equal(h.ime.mode(), false); assert.equal(observations.attaches, 0);
      } finally { await fixture.close(); }
    });

    await lifecycleTest('production native callback/getter trace is consumed by actual Index across same-window generation and focus replacements', async () => {
      const trace = JSON.parse(fs.readFileSync(process.env.FRD_NATIVE_FOCUS_TRACE, 'utf8'));
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      let token = trace.initial;
      try {
        h.model.page = 3; h.syncIsland(); observations.surfaceId = trace.surfaceId;
        h.controller.getXComponentSurfaceId = () => trace.surfaceId;
        fixture.setFocusToken(() => token);
        h.surfaceInteraction(); h.toggleKeyboard(); const first = await fixture.acknowledgeSource();
        assert.equal(observations.attaches, 1);
        for (const next of [trace.blurFocus, trace.rebuilt]) {
          token = next; h.observeIme(); await settleLifecycle();
          assert.equal(h.surfaceHasFocus, true); assert.equal(h.ime.mode(), false);
          assert.equal(h.remoteFocused, false);
          const before = observations.attaches;
          for (let i = 0; i < 20; ++i) h.observeIme();
          assert.equal(observations.attaches, before);
          h.toggleKeyboard(); const current = await fixture.acknowledgeSource();
          assert.equal(observations.attaches, before + 1); assert.ok(current.editorSerial > first.editorSerial);
        }
      } finally { await fixture.close(); }
    });
  }

  await lifecycleTest('native focus remains independent from details and island local keyboard ownership', async () => {
    for (const kind of ['details', 'island']) {
      const fixture = lifecycleHarness(); const { h, observations } = fixture;
      try {
        h.model.page = 3; h.syncIsland(); h.keyboard.enterLocal();
        if (kind === 'details') h.detailsOpen = true;
        fixture.realFocus(); h.ime.request(h.getUIContext(), true, 0); h.observeIme();
        await settleLifecycle(); assert.equal(observations.attaches, 0);
        assert.equal(h.surfaceHasFocus, true); assert.equal(h.remoteFocused, false);
        assert.equal(observations.nativeFocus.some(event => event.focused), false);
      } finally { await fixture.close(); }
    }
  });

  await lifecycleTest('page2 to page3 preserves the same focused XComponent and opens IME without another onFocus', async () => {
    const fixture = lifecycleHarness();
    const { h, observations } = fixture;
    try {
      fixture.realFocus();
      assert.equal(h.surfaceHasFocus, true);
      const before = h.island.epoch();
      h.model.page = 3;
      h.syncIsland();
      assert.ok(h.island.epoch() > before);
      assert.equal(h.surfaceHasFocus, true, 'island epoch is not a real surface blur');
      assert.equal(h.remoteFocused, false, 'new session authorization still starts local');
      h.surfaceInteraction();
      h.toggleKeyboard();
      await fixture.acknowledgeSource();
      assert.equal(observations.focusEvents, 1, 'same focused node must not need duplicate onFocus');
      assert.equal(observations.requests, 1);
      assert.equal(observations.attaches, 1);
      assert.equal(h.ime.loading(), false);
      assert.deepEqual(observations.notices, []);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('same session polling retains source while new session revokes authorization and preserves actual node focus', async () => {
    const fixture = lifecycleHarness();
    const { h, observations } = fixture;
    try {
      h.model.page = 3;
      h.syncIsland();
      fixture.realFocus();
      h.surfaceInteraction();
      h.toggleKeyboard();
      const first = await fixture.acknowledgeSource();
      const epoch = h.island.epoch();
      h.syncIsland();
      assert.equal(h.island.epoch(), epoch);
      assert.equal(h.ime.mode(), true);
      assert.equal(observations.attaches, 1);

      h.model.session = '200';
      h.model.generation = '2';
      h.syncIsland();
      assert.ok(h.island.epoch() > epoch);
      assert.equal(h.surfaceHasFocus, true);
      assert.equal(h.remoteFocused, false);
      assert.equal(h.ime.mode(), false, 'epoch revokes the old source immediately');
      h.surfaceInteraction();
      h.toggleKeyboard();
      const second = await fixture.acknowledgeSource();
      assert.equal(second.session, '200');
      assert.equal(second.generation, '2');
      assert.ok(second.editorSerial > first.editorSerial);
      assert.equal(observations.focusEvents, 1);
      assert.equal(observations.attaches, 2);
      assert.deepEqual(observations.notices, []);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('background without node blur revokes authorization, foreground stays idle, explicit open uses retained node focus and a new ticket', async () => {
    const fixture = lifecycleHarness();
    const { h, observations } = fixture;
    try {
      h.model.page = 3;
      h.syncIsland();
      fixture.realFocus();
      h.surfaceInteraction();
      h.toggleKeyboard();
      const first = await fixture.acknowledgeSource();
      h.shellActive = false;
      h.shellActivityChanged();
      assert.equal(observations.actualFocus, true, 'the focused ArkUI node emitted no blur');
      assert.equal(h.surfaceHasFocus, true, 'background revokes authorization, not an unblurred node record');
      assert.equal(h.remoteFocused, false);
      assert.equal(h.ime.mode(), false);
      h.shellActive = true;
      h.shellActivityChanged();
      h.observeIme();
      await settleLifecycle();
      assert.equal(h.remoteFocused, false, 'foreground never restores native authorization automatically');
      assert.equal(h.ime.mode(), false);
      assert.equal(observations.attaches, 1, 'foreground alone cannot autoattach');

      h.toggleKeyboard();
      const second = await fixture.acknowledgeSource();
      assert.equal(observations.focusEvents, 1, 'repeated requestFocus on the same node emits no new event');
      assert.equal(observations.attaches, 2);
      assert.ok(second.editorSerial > first.editorSerial);
      assert.equal(h.ime.loading(), false);
      assert.deepEqual(observations.notices, []);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('background with actual node blur keeps explicit open waiting until a real new onFocus', async () => {
    const fixture = lifecycleHarness();
    const { h, observations } = fixture;
    try {
      h.model.page = 3;
      h.syncIsland();
      fixture.realFocus();
      h.surfaceInteraction();
      h.toggleKeyboard();
      const first = await fixture.acknowledgeSource();
      h.shellActive = false;
      h.shellActivityChanged();
      fixture.realBlur();
      assert.equal(h.surfaceHasFocus, false);
      assert.equal(h.remoteFocused, false);
      assert.equal(h.ime.mode(), false);
      h.shellActive = true;
      h.shellActivityChanged();
      h.observeIme();
      await settleLifecycle();
      assert.equal(observations.attaches, 1);
      observations.refuseFocus = true;
      h.toggleKeyboard();
      await settleLifecycle();
      assert.equal(h.surfaceHasFocus, false);
      assert.equal(h.remoteFocused, false);
      assert.equal(observations.attaches, 1, 'requestFocus returning void does not prove focus');
      fixture.realFocus();
      const second = await fixture.acknowledgeSource();
      assert.equal(observations.focusEvents, 2);
      assert.equal(observations.attaches, 2);
      assert.ok(second.editorSerial > first.editorSerial);
      assert.deepEqual(observations.notices, []);
    } finally { await fixture.close(); }
  });

  await lifecycleTest('real blur stays closed across stable epoch and native ownership restoration without a real focus event', async () => {
    const fixture = lifecycleHarness();
    const { h, observations } = fixture;
    try {
      h.model.page = 3;
      h.syncIsland();
      fixture.realFocus();
      h.surfaceInteraction();
      h.toggleKeyboard();
      await fixture.acknowledgeSource();
      fixture.realBlur();
      assert.equal(h.surfaceHasFocus, false);
      assert.equal(h.ime.mode(), false);
      h.syncIsland();
      h.surfaceInteraction();
      assert.equal(h.remoteFocused, true);
      assert.equal(h.surfaceHasFocus, false);
      observations.refuseFocus = true;
      h.toggleKeyboard();
      await settleLifecycle();
      assert.equal(observations.attaches, 1);
      observations.clock = 2101;
      h.observeIme();
      assert.equal(h.ime.mode(), false);
      assert.equal(observations.attaches, 1);
      assert.equal(observations.notices.at(-1), '系统输入法暂时不可用，请重新点击键盘');
    } finally { await fixture.close(); }
  });

  await lifecycleTest('page0 removal and component disappearance clear physical focus evidence', async () => {
    for (const destroy of ['page0', 'disappear']) {
      const fixture = lifecycleHarness();
      const { h } = fixture;
      try {
        h.model.page = 3;
        h.syncIsland();
        fixture.realFocus();
        h.surfaceInteraction();
        h.toggleKeyboard();
        await fixture.acknowledgeSource();
        if (destroy === 'page0') { h.model.page = 0; h.syncIsland(); }
        else { h.aboutToDisappear(); }
        assert.equal(h.surfaceHasFocus, false);
        assert.equal(h.remoteFocused, false);
        assert.equal(h.ime.mode(), false);
      } finally { await fixture.close(); }
    }
  });
  console.log(`${count} real surface tests passed`);
})().catch(error => { console.error(error); process.exitCode = 1; });
