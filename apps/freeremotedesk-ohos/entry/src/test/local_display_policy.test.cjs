// 主机执行真实 EntryAbility 编译结果与公开 API mock；不冒充设备/协商证据。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require(process.env.FRD_TYPESCRIPT_MODULE || 'typescript');
const base = path.resolve(__dirname, '../main/ets');
function compile(file, requireMock, globals = {}) {
  const source = fs.readFileSync(path.join(base, file), 'utf8');
  const result = ts.transpileModule(source, {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020},reportDiagnostics:true,fileName:file.replace('.ets','.ts')});
  assert.equal((result.diagnostics || []).length,0);
  const exports = {};
  vm.runInNewContext(result.outputText, {exports,require:requireMock, ...globals});
  return exports;
}
const policy = compile('common/LocalDisplayPolicy.ets');
let cases = 0;
async function test(name, run) { await run(); cases++; console.log(`PASS ${name}`); }
function harness(device='phone') {
  const messages = []; const pending = []; const errors=[];
  let properties={id:11,displayId:7,windowRect:{left:0,top:0,width:1260,height:2720},drawableRect:{left:0,top:100,width:1260,height:2520},isLayoutFullScreen:false};
  let screen={id:7,width:1260,height:2720,densityPixels:3.5,rotation:0,on(){},off(){},getAvailableArea(){return new Promise((resolve,reject)=>pending.push({resolve,reject}));}};
  const handlers={}; const displayHandlers={};
  const win={getWindowProperties(){return properties;},on(event,fn){handlers[event]=fn;},off(event){delete handlers[event];},setPreferredOrientation(){return Promise.resolve();},getWindowAvoidArea(){return {leftRect:{width:0},rightRect:{width:0},topRect:{height:0},bottomRect:{height:0}};}};
  const display={getDisplayByIdSync(id){assert.equal(id,properties.displayId);return screen;},on(event,fn){displayHandlers[event]=fn;},off(event){delete displayHandlers[event];}};
  const native={initializeApp(){},shutdownApp(){},setForeground(){},appFocus(){},appLocalDisplay(text){messages.push(JSON.parse(text));return 0;}};
  const modules={...policy};
  class UIAbility { context={filesDir:'/nonsecret/mock',eventHub:{on(){},off(){}},getApplicationContext(){return {setColorMode(){}};}}; }
  const requireMock=(name)=> {
    if(name==='@kit.AbilityKit')return {UIAbility,ConfigurationConstant:{ColorMode:{COLOR_MODE_NOT_SET:0}}};
    if(name==='@kit.ArkUI')return {window:{AvoidAreaType:{TYPE_CUTOUT:1,TYPE_SYSTEM:2},Orientation:{},WindowEventType:{WINDOW_ACTIVE:1,WINDOW_INACTIVE:2}},display};
    if(name==='@kit.PerformanceAnalysisKit')return {hilog:{info(){},warn(...args){errors.push(args);},error(){}}};
    if(name==='@kit.BasicServicesKit')return {deviceInfo:{deviceType:device}};
    if(name==='libentry.so')return {default:native};
    if(name.includes('LocalDisplayPolicy'))return modules;
    if(name.includes('PhoneOrientationPolicy'))return compile('common/PhoneOrientationPolicy.ets');
    if(name.includes('PhoneFullscreenPolicy'))return compile('common/PhoneFullscreenPolicy.ets');
    throw Error(name);
  };
  const Ability=compile('entryability/EntryAbility.ets',requireMock,{AppStorage:{setOrCreate(){}},canIUse(){return false;}}).default;
  const ability=new Ability();ability.onCreate();ability.onForeground();
  ability.onWindowStageCreate({getMainWindowSync(){return win;},loadContent(_page,fn){fn({code:0});}});
  return {ability,messages,pending,handlers,displayHandlers,errors,get properties(){return properties;},set properties(p){properties=p;},get screen(){return screen;},set screen(s){screen=s;}};
}
(async()=>{
  await test('revision/window/stage拒绝迟到完成，destroy后新window隔离',()=>{
    const p=new policy.LocalDisplayPolicy();p.attach();const first=p.begin(11);const second=p.begin(11);
    assert.equal(p.isCurrent(first),false);assert.equal(p.isCurrent(second),true);
    p.detach();p.attach();const next=p.begin(12);assert.equal(p.isCurrent(second),false);assert.equal(p.isCurrent(next),true);
  });
  await test('真实EntryAbility consumer保留portrait像素与非整数density，不重复缩放',()=>{
    const h=harness();const latest=h.messages.at(-1);
    assert.equal(latest.phone,true);assert.equal(latest.geometry.width,1260);assert.equal(latest.geometry.height,2720);
    assert.equal(latest.geometry.density,3.5);assert.equal(latest.geometry.workArea,null);
    assert.equal(latest.geometry.drawable.top,100);assert.equal(latest.windowId,11);
  });
  await test('异步系统工作区按同一revision提交，窗口事件刷新后旧结果不污染',async()=>{
    const h=harness();const first=h.pending.at(-1);h.handlers.windowRectChange();const current=h.pending.at(-1);
    const before=h.messages.length;first.resolve({left:0,top:25,width:1260,height:2600});await new Promise(setImmediate);
    assert.equal(h.messages.length,before);
    current.resolve({left:0,top:30,width:1260,height:2600});await new Promise(setImmediate);
    assert.equal(h.messages.at(-1).geometry.workArea.top,30);
    assert.ok(h.messages.at(-1).revision>h.messages[before-1].revision);
  });
  await test('工作区不支持或零区保留显式保守来源输入，PC4K原始尺寸',async()=>{
    const h=harness('pc');h.screen.width=3840;h.screen.height=2160;h.screen.densityPixels=1.5;
    h.properties.windowRect={left:-50,top:20,width:3000,height:1800};h.properties.drawableRect={left:10,top:50,width:2980,height:1700};
    h.handlers.windowRectChange();const latest=h.messages.at(-1);assert.equal(latest.geometry.width,3840);assert.equal(latest.phone,false);assert.equal(latest.geometry.window.left,-50);
    h.pending.at(-1).reject({code:801});await new Promise(setImmediate);assert.equal(h.messages.at(-1).geometry.workArea,null);
    h.handlers.windowRectChange();h.pending.at(-1).resolve({left:0,top:0,width:0,height:0});await new Promise(setImmediate);assert.equal(h.messages.at(-1).geometry.workArea,null);
  });
  await test('窗口销毁清快照、撤监听，旧异步回包不能复活；前后台显式失效',async()=>{
    const h=harness();const first=h.pending.at(-1);h.ability.onBackground();assert.equal(h.messages.at(-1).reason,'background');
    first.resolve({left:0,top:0,width:1260,height:2700});await new Promise(setImmediate);assert.equal(h.messages.at(-1).reason,'background');
    h.ability.onForeground();assert.ok(h.messages.at(-1).geometry);const stale=h.pending.at(-1);
    h.ability.onWindowStageDestroy();assert.equal(h.messages.at(-1).reason,'window_destroyed');assert.equal(h.handlers.windowRectChange,undefined);assert.equal(h.displayHandlers.change,undefined);
    stale.resolve({left:0,top:0,width:1260,height:2700});await new Promise(setImmediate);assert.equal(h.messages.at(-1).reason,'window_destroyed');
  });
  await test('所属显示器缺失不会偷偷采用default屏，回调前旋转触发新快照',async()=>{
    const h=harness();const first=h.pending.at(-1);h.screen.width=2720;h.screen.height=1260;h.screen.rotation=1;
    h.properties.windowRect={left:0,top:0,width:2720,height:1260};h.properties.drawableRect={left:0,top:50,width:2720,height:1160};
    first.resolve({left:0,top:0,width:1260,height:2700});await new Promise(setImmediate);
    assert.equal(h.messages.at(-1).geometry.width,2720);assert.equal(h.messages.at(-1).geometry.workArea,null);
    h.properties.displayId=undefined;h.handlers.displayIdChange();assert.equal(h.messages.at(-1).reason,'display_unavailable');
  });
  await test('Fix1真实EntryAbility系统area负原点与部分越界保留drawable并记录原因',async()=>{
    for(const area of [{left:-1,top:0,width:1260,height:2720},{left:0,top:-1,width:1260,height:2720},{left:0,top:0,width:1261,height:2720},{left:0,top:0,width:1260,height:2721}]) {
      const h=harness();h.pending.at(-1).resolve(area);await new Promise(setImmediate);
      assert.equal(h.messages.at(-1).geometry.workArea,null);assert.equal(h.messages.at(-1).reason,'invalid_work_area');
    }
    const h=harness();h.pending.at(-1).resolve({left:0,top:0,width:1260,height:2720});await new Promise(setImmediate);
    assert.equal(h.messages.at(-1).geometry.workArea.width,1260);assert.equal(h.messages.at(-1).reason,null);
  });
  await test('Fix1真实EntryAbility异步期间SDK矩形对象原地改变也重新采样',async()=>{
    const h=harness();h.properties.drawableRect.top=110;h.properties.drawableRect.height=2510;
    h.pending.at(-1).resolve({left:0,top:0,width:1260,height:2720});await new Promise(setImmediate);
    assert.equal(h.messages.at(-1).geometry.workArea,null);assert.equal(h.messages.at(-1).geometry.drawable.top,110);
  });
  await test('Fix1显式px矩形转换保留signed值且不借用SDK对象',()=>{
    assert.equal(typeof policy.makeDisplayRect,'function');
    const converted=policy.makeDisplayRect(-50,60,2980,1700);
    assert.equal(converted.left,-50);assert.equal(converted.top,60);assert.equal(converted.width,2980);assert.equal(converted.height,1700);
  });
  await test('Fix1 SDK矩形字段顺序和额外字段不改变几何，不循环refresh',async()=>{
    const h=harness();
    h.properties.windowRect={height:2720,width:1260,top:0,left:0,sdkExtra:'ignored'};
    h.properties.drawableRect={height:2520,width:1260,top:100,left:0,sdkExtra:'ignored'};
    const pending=h.pending.length;
    h.pending.at(-1).resolve({height:2720,width:1260,top:0,left:0,sdkExtra:'ignored'});await new Promise(setImmediate);
    assert.equal(h.pending.length,pending);assert.equal(h.messages.at(-1).geometry.workArea.width,1260);
    assert.equal(h.messages.at(-1).geometry.workArea.sdkExtra,undefined);
  });
  console.log(`${cases} local display cases passed`);
})().catch(error=>{console.error(error);process.exitCode=1;});
