const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const ts=require(process.env.FRD_ARKTS_MODULE);
function compile(name,modules={},globals={}){const p=path.resolve(__dirname,'../main/ets/common/'+name+'.ets');const source=fs.existsSync(p)?fs.readFileSync(p,'utf8'):'';const ast=ts.createSourceFile(p,source,ts.ScriptTarget.Latest,true,ts.ScriptKind.ETS);assert.equal(ast.parseDiagnostics.length,0);const out=ts.transpileModule(source,{fileName:p,compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.CommonJS},reportDiagnostics:true});assert.equal(out.diagnostics.length,0);const exports={};vm.runInNewContext(out.outputText,{exports,require:n=>modules[n],setTimeout,clearTimeout,...globals});return exports;}
const policy=compile('RemoteImePolicy');let count=0;async function test(n,f){if(process.env.FRD_IME_FILTER&&!n.includes(process.env.FRD_IME_FILTER))return;await f();count++;console.log('PASS '+n);}const tick=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
function make(options={}) {
  const diagnostics=[], sources=[], operations=[], notices=[], listeners=new Map(), offs=[];
  let resolve, reject, bound=false;
  const controller={
    on(name, callback) {
      if(!bound)throw {code:12800009};
      assert.ok(!listeners.has(name));listeners.set(name,callback);
    },
    off(name, callback) {assert.equal(listeners.get(name),callback);offs.push(name);listeners.delete(name);},
    attachWithUIContext(context, config, options) {
      bound=false;this.attached=(this.attached||0)+1;
      assert.equal(listeners.has('insertText'),false);assert.equal(options.showKeyboard,false);
      assert.equal(config.inputAttribute.consumeKeyEvents,false);assert.equal(config.windowId,7);
      this.ctx=context;this.options=options;
      return new Promise((accept,decline)=>{resolve=()=>{bound=true;accept();};reject=decline;if(immediateAttach)resolve();});
    },
    detach() {this.detached=(this.detached||0)+1;bound=false;return Promise.resolve();},
    changeSelection() {return Promise.resolve().then(()=>{this.selectionConfirmed=true;});},
    showTextInput() {
      assert.equal(listeners.size,14);assert.equal(this.selectionConfirmed,true);assert.equal(a.loading(),true);
      assert.equal(snap.ime.state,'attached');assert.equal(snap.ime.ticket,sources.filter(source=>source.state==='attached').at(-1).ticket);
      this.shown=(this.shown||0)+1;return Promise.resolve();
    }
  };
  const immediateAttach=options.immediateAttach;
  const module=compile('RemoteImeAdapter',{'@kit.IMEKit':{inputMethod:{getController:()=>controller,TextInputType:{TEXT:0},EnterKeyType:{NEWLINE:8}}},'./RemoteImePolicy':policy},options.timers||{});
  assert.equal(typeof module.RemoteImeAdapter,'function','actual system adapter must exist');
  const bridge={source:json=>{sources.push(JSON.parse(json));return bridge.code||0;},operation:json=>{operations.push(JSON.parse(json));return bridge.opCode||0;}};
  const a=new module.RemoteImeAdapter(()=>{if(options.controllerError)throw options.controllerError;return controller;},bridge,
    ()=>({isBoundary:true,previous:0,next:0}),notice=>notices.push(notice),event=>{
      diagnostics.push(event);if(options.diagnosticThrows)throw Error('diagnostic callback fixture');
    });
  const snap={page:3,session:'100',generation:'2',localDisplayEvidence:{stage:3,windowId:7},ime:{installed:true,protocolTextInput:true,sourceAvailable:true,state:'detached',ticket:null,lastAcceptedSerial:0,lastDispatchedSerial:0}};
  return {a,module,diagnostics,controller,listeners,offs,sources,operations,notices,bridge,snap,resolve:()=>resolve(),reject:(error=Error('fixed'))=>reject(error)};
}
async function attached(h){h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();h.resolve();await tick();h.snap.ime.state='attached';h.snap.ime.ticket=h.sources[0].ticket;h.a.observe(h.snap,true,true,2);await tick();}
(async()=>{
await test('SDK contract attaches hidden before registering insertText and all fourteen callbacks', async () => {
  const h=make(); h.a.request({},true,0); h.a.observe(h.snap,true,true,1); await tick();
  assert.equal(h.controller.attached,1,'SDK attach must start before subscriptions; old failure=' + JSON.stringify(h.diagnostics.at(-1)));
  assert.equal(h.listeners.size,0); assert.equal(h.sources.length,0); assert.equal(h.controller.options.showKeyboard,false);
  h.resolve(); await tick(); assert.equal(h.listeners.size,14); assert.equal(h.sources.length,1);
  assert.equal(h.controller.shown||0,0); h.a.close(); await tick();
});
async function published(h, show=true) {
  h.a.request({},show,0);h.a.observe(h.snap,true,true,1);await tick();h.resolve();await tick();
}
function owner(h, now=2) {h.snap.ime.state='attached';h.snap.ime.ticket=h.sources.filter(s=>s.state==='attached').at(-1).ticket;h.a.observe(h.snap,true,true,now);}
for(const immediateAttach of [false,true]) for(const show of [false,true]) {
  await test(`SDK ${immediateAttach?'immediate':'deferred'} attach then exact Owner and confirmed selection precede show=${show}`,async()=>{
    const h=make({immediateAttach});await published(h,show);assert.equal(h.controller.shown||0,0);assert.equal(h.a.loading(),true);
    owner(h);await tick();assert.equal(h.controller.shown||0,show?1:0);assert.equal(h.a.loading(),false);
    for(let i=3;i<30;i++)h.a.observe(h.snap,true,true,i);await tick();assert.equal(h.controller.attached,1);assert.equal(h.controller.shown||0,show?1:0);
    assert.ok(h.diagnostics.some(e=>e.stage===9&&e.outcome===2));assert.equal(h.diagnostics.filter(e=>e.stage===12&&e.outcome===2).length,show?1:0);
    h.a.close();await tick();
  });
}
await test('SDK initial selection must settle even after exact Owner; pending selection cannot show or flush staged text',async()=>{
  const h=make();let selectionResolve;h.controller.changeSelection=()=>new Promise(resolve=>{selectionResolve=()=>{h.controller.selectionConfirmed=true;resolve();};});
  await published(h);owner(h);await tick();assert.equal(h.controller.shown||0,0);assert.equal(h.a.loading(),true);
  h.listeners.get('insertText')('同');assert.equal(h.operations.length,0);selectionResolve();await tick();
  // 首次 selection 必须自行完成；后续编辑的 selection 不能代替确认。
  assert.equal(h.controller.shown||0,0);h.a.close();await tick();assert.equal(h.controller.detached,1);
});
await test('SDK initial selection success after exact Owner starts show once and releases the exact current queue',async()=>{
  const h=make();let selectionResolve;h.controller.changeSelection=()=>new Promise(resolve=>{selectionResolve=()=>{h.controller.selectionConfirmed=true;resolve();};});
  await published(h);owner(h);await tick();assert.equal(h.controller.shown||0,0);selectionResolve();await tick();
  assert.equal(h.controller.shown,1);assert.equal(h.a.loading(),false);h.a.close();await tick();
});
for(const kind of ['throw','reject']) await test('SDK show '+kind+' records stage12 code and detaches completed attachment',async()=>{
  const h=make();const original=h.controller.showTextInput;h.controller.showTextInput=function(){original.call(this);if(kind==='throw')throw {code:12800009,message:'private-show'};return Promise.reject({code:12800009,message:'private-show'});};
  await published(h);owner(h);await tick();const terminal=h.diagnostics.at(-1);
  assert.equal(terminal.stage,12);assert.equal(terminal.outcome,kind==='throw'?3:4);assert.equal(terminal.code,12800009);
  assert.equal(h.a.mode(),false);assert.equal(h.controller.detached,1);assert.equal(h.listeners.size,0);assert.equal(h.sources.at(-1).state,'detached');
  assert.equal(JSON.stringify(h.diagnostics).includes('private-show'),false);assert.equal(h.diagnostics.some(e=>e.stage===9),false);
});
for(const reason of ['close','replace','focus','foreground','window','stage','session','generation','source','owner']) {
  for(const phase of ['before','during']) await test(`SDK ${reason} ${phase} show synchronously invalidates old closures and serializes cleanup`,async()=>{
    const h=make();let finishShow;const original=h.controller.showTextInput;h.controller.showTextInput=function(){original.call(this);return new Promise(resolve=>{finishShow=resolve;});};
    await published(h);const insert=h.listeners.get('insertText'),query=h.listeners.get('getLeftTextOfCursor');
    h.listeners.get('setPreviewText')('ab',{start:-1,end:-1});await tick();
    if(phase==='during'){owner(h);await tick();assert.equal(h.controller.shown,1);}else{owner(h);}
    if(reason==='close')h.a.close();else if(reason==='replace'){h.a.request({},false,10);h.a.observe(h.snap,true,true,11);}
    else{
      if(reason==='window')h.snap.localDisplayEvidence.windowId++;
      if(reason==='stage')h.snap.localDisplayEvidence.stage++;
      if(reason==='session')h.snap.session='101';if(reason==='generation')h.snap.generation='3';
      if(reason==='source')h.snap.ime.sourceAvailable=false;if(reason==='owner')h.snap.ime.ticket=null;
      h.a.observe(h.snap,reason!=='focus',reason!=='foreground',10);
    }
    assert.equal(query(9),'');insert('stale');assert.equal(h.operations.length,0);await tick();
    assert.equal(h.controller.shown||0,phase==='during'?1:0);
    if(phase==='during'){assert.equal(h.controller.detached||0,0);const first=h.diagnostics[0].attempt,count=h.diagnostics.filter(e=>e.attempt===first).length;finishShow();await tick();assert.equal(h.diagnostics.filter(e=>e.attempt===first).length,count);}
    assert.equal(h.controller.detached,1);assert.equal(h.diagnostics.some(e=>e.stage===9),false);
    if(reason==='replace'){assert.equal(h.controller.attached,2);h.resolve();await tick();owner(h,12);await tick();assert.equal(h.a.loading(),false);assert.equal(h.controller.shown||0,phase==='during'?1:0);}
    h.a.close();await tick();
  });
}
for(const trigger of ['watchdog','deadline']) await test('SDK show pending '+trigger+' owns stage12 timeout; late resolve cannot ready or unpoison',async()=>{
  const timers=logicalTimers(),h=make({timers});let finishShow;h.controller.showTextInput=()=>new Promise(resolve=>{finishShow=resolve;});
  await published(h);owner(h);await tick();const id=timers.next()[0];h.listeners.get('insertText')('同');assert.equal(h.operations.length,0);
  if(trigger==='watchdog')timers.fire(id);else h.a.observe(h.snap,true,true,2001);
  const terminal=h.diagnostics.at(-1);assert.equal(terminal.stage,12);assert.equal(terminal.outcome,6);assert.equal(h.a.mode(),false);assert.equal(h.controller.detached||0,0);
  const count=h.diagnostics.length;finishShow();await tick();assert.equal(h.diagnostics.length,count);assert.equal(h.controller.detached,1);assert.equal(h.operations.length,0);assert.equal(h.diagnostics.some(e=>e.stage===9),false);
  h.a.request({},true,2200);assert.equal(h.a.mode(),false);assert.equal(h.diagnostics.at(-1).outcome,8);
});
for(const sameAdapter of [false,true]) await test('SDK pending show blocks '+(sameAdapter?'same':'cross')+' adapter replacement and old watchdog poisons only waiting request',async()=>{
  const timers=logicalTimers(),h=make({timers});let finishShow;h.controller.showTextInput=()=>new Promise(resolve=>{finishShow=resolve;});
  await published(h);owner(h);await tick();const id=timers.next()[0],first=h.diagnostics[0].attempt,diagnostics=[];
  h.a.close();const oldTerminal=JSON.stringify(terminalEvents(h,first));const b=sameAdapter?h.a:new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),()=>{},e=>diagnostics.push(e));
  b.request({},false,100);b.observe(h.snap,true,true,101);await tick();assert.equal(h.controller.attached,1);assert.equal(h.controller.detached||0,0);
  if(sameAdapter)timers.fire(id);else b.observe(h.snap,true,true,2101);
  const terminal=sameAdapter?h.diagnostics.at(-1):diagnostics.at(-1);assert.equal(terminal.stage,1);assert.equal(terminal.outcome,8);assert.equal(b.mode(),false);
  assert.equal(JSON.stringify(terminalEvents(h,first)),oldTerminal);finishShow();await tick();assert.equal(h.controller.attached,1);assert.equal(h.controller.detached,1);assert.equal(h.operations.length,0);
});
await test('SDK cross adapter replacement waits for original show settle then detaches before new attach without old replay',async()=>{
  const h=make();let finishShow;h.controller.showTextInput=()=>new Promise(resolve=>{finishShow=resolve;});await published(h);owner(h);await tick();
  const oldInsert=h.listeners.get('insertText'),oldQuery=h.listeners.get('getLeftTextOfCursor');h.listeners.get('setPreviewText')('old',{start:-1,end:-1});h.a.close();
  const b=new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),()=>{});
  b.request({},false,10);b.observe(h.snap,true,true,11);await tick();assert.equal(h.controller.attached,1);assert.equal(h.controller.detached||0,0);
  assert.equal(oldQuery(9),'');oldInsert('stale');finishShow();await tick();assert.equal(h.controller.detached,1);assert.equal(h.controller.attached,2);assert.equal(h.listeners.size,0);
  h.resolve();await tick();const current=h.sources.at(-1).ticket;h.snap.ime.ticket=current;b.observe(h.snap,true,true,12);await tick();assert.equal(b.loading(),false);
  oldInsert('stale');h.listeners.get('insertText')('新');assert.equal(h.operations.length,1);assert.equal(h.operations[0].ticket.editorSerial,current.editorSerial);
  assert.equal(h.controller.detached,1);b.close();await tick();assert.equal(h.controller.detached,2);
});
await test('SDK unknown initial selection expires closed and late selection resolution cannot show or restore source',async()=>{
  const h=make();let finishSelection;h.controller.changeSelection=()=>new Promise(resolve=>{finishSelection=resolve;});await published(h);owner(h);await tick();
  assert.equal(h.controller.shown||0,0);h.a.observe(h.snap,true,true,2001);await tick();assert.equal(h.a.mode(),false);assert.equal(h.controller.detached,1);assert.equal(h.sources.at(-1).state,'detached');
  const count=h.diagnostics.length;finishSelection();await tick();assert.equal(h.diagnostics.length,count);assert.equal(h.controller.shown||0,0);assert.equal(h.diagnostics.some(e=>e.stage===9),false);
});
await test('SDK pending show holds 32 staged operations then fails boundedly without replay on late completion',async()=>{
  const h=make();let finishShow;h.controller.showTextInput=()=>new Promise(resolve=>{finishShow=resolve;});await published(h);owner(h);await tick();
  const insert=h.listeners.get('insertText');for(let i=0;i<32;i++)insert('同');assert.equal(h.a.mode(),true);assert.equal(h.operations.length,0);insert('同');
  assert.equal(h.a.mode(),false);assert.equal(h.diagnostics.at(-1).outcome,10);finishShow();await tick();assert.equal(h.operations.length,0);assert.equal(h.controller.detached,1);
});
await test('SDK pending show enforces 16384 byte staging cap and never sends oversized context',async()=>{
  const h=make();let finishShow;h.controller.showTextInput=()=>new Promise(resolve=>{finishShow=resolve;});await published(h);owner(h);await tick();
  const insert=h.listeners.get('insertText');insert('中'.repeat(4096));assert.equal(h.a.mode(),true);insert('中'.repeat(4096));
  assert.equal(h.a.mode(),false);assert.equal(h.diagnostics.at(-1).stage,6);assert.equal(h.diagnostics.at(-1).outcome,10);finishShow();await tick();assert.equal(h.operations.length,0);assert.equal(h.controller.detached,1);
});
await test('SDK immediate subscription callback stays staged until selection Owner and actual show success',async()=>{
  const h=make();const original=h.controller.on;h.controller.on=function(name,callback){original.call(this,name,callback);if(name==='insertText'){callback('同');callback('同');}};
  let finishShow;h.controller.showTextInput=()=>new Promise(resolve=>{finishShow=resolve;});await published(h);assert.equal(h.operations.length,0);owner(h);await tick();assert.equal(h.operations.length,0);
  finishShow();await tick();assert.deepEqual(h.operations.map(o=>o.text),['同','同']);assert.deepEqual(h.operations.map(o=>o.serial),[1,2]);assert.deepEqual(h.operations.map(o=>o.ticket.editorSerial),[h.snap.ime.ticket.editorSerial,h.snap.ime.ticket.editorSerial]);h.a.close();await tick();
});
await test('actual focus and source availability gate, immediate callback staged until exact Owner ticket',async()=>{const h=make();h.a.request({id:1},true,0);h.a.observe(h.snap,false,true,1);await tick();assert.equal(h.listeners.size,0);h.snap.ime.sourceAvailable=false;h.a.observe(h.snap,true,true,2);await tick();assert.equal(h.listeners.size,0);h.snap.ime.sourceAvailable=true;h.a.observe(h.snap,true,true,3);await tick();assert.equal(h.listeners.size,0);h.resolve();await tick();h.listeners.get('setPreviewText')('ni',{start:-1,end:-1});h.listeners.get('insertText')('你');h.listeners.get('finishTextPreview')();assert.equal(h.operations.length,0);assert.equal(h.sources[0].state,'attached');assert.equal(h.operations.length,0);h.snap.ime.state='attached';h.snap.ime.ticket={...h.sources[0].ticket,editorSerial:999};h.a.observe(h.snap,true,true,4);assert.equal(h.operations.length,0);h.snap.ime.ticket=h.sources[0].ticket;h.a.observe(h.snap,true,true,5);await tick();assert.deepEqual(h.operations.map(o=>o.text),['你']);assert.equal(h.a.queuedSerial(),1);assert.equal(h.a.acceptedSerial(),0);h.snap.ime.ackTicket=h.sources[0].ticket;h.snap.ime.lastAcceptedSerial=1;h.snap.ime.lastDispatchedSerial=1;h.a.observe(h.snap,true,true,6);assert.equal(h.a.acceptedSerial(),1);assert.equal(h.a.dispatchedSerial(),1);h.a.close();await tick();assert.equal(h.listeners.size,0);});
await test('invalidates before off/detach; late F never replays the closed published source',async()=>{const h=make();h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();h.resolve();await tick();const old=h.listeners.get('finishTextPreview');h.listeners.get('setPreviewText')('secretfixture',{start:-1,end:-1});h.a.close();old();await tick();assert.equal(h.operations.length,0);assert.equal(h.sources.filter(s=>s.state==='attached').length,1);assert.equal(h.listeners.size,0);assert.equal(h.controller.detached,1);});
await test('same-focus resize and keyboard HIDE preserve source; blur/window/session changes revoke',async()=>{for(const change of ['blur','window','session','source']){const h=make();await attached(h);const t=h.sources[0].ticket;h.snap.geometryRevision='99';h.listeners.get('sendKeyboardStatus')(1);h.a.observe(h.snap,true,true,3);h.listeners.get('insertText')('同');assert.equal(h.operations[0].ticket.editorSerial,t.editorSerial);if(change==='window')h.snap.localDisplayEvidence.windowId=8;if(change==='session')h.snap.generation='3';if(change==='source')h.snap.ime.sourceAvailable=false;h.a.observe(h.snap,change!=='blur',true,4);await tick();assert.equal(h.a.mode(),false);assert.equal(h.sources.at(-1).state,'detached');assert.equal(h.listeners.size,0);}});
await test('serial source grows across new adapter instances and detach completes before new listeners',async()=>{const h=make();await attached(h);const first=h.sources[0].ticket.editorSerial;h.a.close();await tick();const b=new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),()=>{});b.request({},false,5);b.observe(h.snap,true,true,6);await tick();h.resolve();await tick();assert.ok(h.sources.at(-1).ticket.editorSerial>first);assert.equal(h.controller.options.showKeyboard,false);b.close();await tick();});
await test('queue overflow and bridge failure stop source, no automatic replay',async()=>{let h=make();h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();h.resolve();await tick();const insert=h.listeners.get('insertText');for(let i=0;i<33;i++)insert('a');await tick();assert.equal(h.a.mode(),false);assert.equal(h.operations.length,0);assert.ok(h.notices.length);h=make();await attached(h);h.bridge.opCode=-11;const old=h.listeners.get('insertText');old('好');old('好');await tick();assert.equal(h.operations.length,1);assert.equal(h.a.mode(),false);});
await test('attach failure, source bridge error and Owner timeout produce fixed recoverable status',async()=>{for(const kind of ['attach','bridge','timeout']){const h=make();h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();if(kind==='attach')h.reject();else {if(kind==='bridge')h.bridge.code=-11;h.resolve();}await tick();if(kind==='timeout')h.a.observe(h.snap,true,true,3000);await tick();assert.equal(h.a.mode(),false);assert.ok(h.notices.length);assert.equal(h.operations.length,0);}});
await test('stale callbacks synchronously expose empty context and never affect newer editor',async()=>{const h=make();await attached(h);const query=h.listeners.get('getLeftTextOfCursor'),insert=h.listeners.get('insertText');h.listeners.get('setPreviewText')('abcd',{start:-1,end:-1});assert.equal(query(99),'abcd');h.a.close();assert.equal(query(99),'');insert('old');await tick();assert.equal(h.operations.length,0);});
await test('off failure removes remaining own listeners, blocks process IME with explicit restart message',async()=>{const h=make();await attached(h);const original=h.controller.off;h.controller.off=function(n,f){if(n==='insertText')throw Error('fixed');return original.call(this,n,f);};h.a.close();await tick();assert.equal(h.listeners.size,1);assert.ok(h.notices.includes('系统输入法清理失败，请重新打开应用'));h.a.request({},true,9);h.a.observe(h.snap,true,true,10);await tick();assert.equal(h.a.mode(),false);assert.equal(h.sources.filter(s=>s.state==='attached').length,1);});
await test('slow old detach serializes newer attach and cannot remove newer callbacks',async()=>{const h=make();await attached(h);let finishDetach;h.controller.detach=()=>new Promise(r=>{finishDetach=r;});h.a.close();const b=new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),()=>{});b.request({},false,5);b.observe(h.snap,true,true,6);await tick();assert.equal(h.listeners.size,0);finishDetach();await tick();assert.equal(h.listeners.size,0);h.resolve();await tick();assert.equal(h.listeners.size,14);assert.equal(h.sources.filter(s=>s.state==='attached').length,2);h.controller.detach=()=>Promise.resolve();b.close();await tick();});
await test('rejected attach with successful off remains recoverable despite no attachment to detach',async()=>{const h=make();h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();h.controller.detach=()=>Promise.reject(Error('not attached'));h.reject();await tick();assert.equal(h.a.mode(),false);assert.equal(h.listeners.size,0);h.a.request({},true,10);h.a.observe(h.snap,true,true,11);await tick();assert.equal(h.listeners.size,0);h.resolve();await tick();assert.equal(h.listeners.size,14);assert.equal(h.sources.filter(s=>s.state==='attached').length,1);h.controller.detach=()=>Promise.resolve();h.a.close();await tick();});
await test('active detach failure blocks new singleton attach but ordinary adapter mode remains off',async()=>{const h=make();await attached(h);h.controller.detach=()=>Promise.reject(Error('fixed'));h.a.close();await tick();assert.equal(h.a.mode(),false);h.a.request({},true,10);assert.equal(h.a.mode(),false);assert.equal(h.notices.at(-1),'系统输入法清理失败，请重新打开应用');});
await test('never-settled attach timeout blocks process and late completion cannot revive or retag source',async()=>{const h=make();h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();assert.equal(h.listeners.size,0);h.a.observe(h.snap,true,true,2001);assert.equal(h.a.mode(),false);assert.equal(h.notices.at(-1),'系统输入法未响应，请重新打开应用');const b=new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),n=>h.notices.push(n));b.request({},true,2100);assert.equal(b.mode(),false);h.resolve();await tick();assert.equal(h.sources.filter(s=>s.state==='attached').length,0);assert.equal(h.operations.length,0);assert.equal(h.listeners.size,0);assert.equal(h.controller.detached,1);});
await test('real insert/delete/move/function listeners preserve exact single producer and synchronous context',async()=>{const h=make();await attached(h);const get=n=>h.listeners.get(n);get('insertText')('同');get('insertText')('同');get('deleteLeft')(1);get('deleteRight')(1);get('moveCursor')(3);get('sendFunctionKey')({enterKeyType:8});assert.deepEqual(h.operations.map(o=>o.text||o.key),['同','同','Backspace','Delete','ArrowLeft','Enter']);assert.deepEqual(h.operations.map(o=>o.serial),[1,2,3,4,5,6]);get('setPreviewText')('abcd',{start:-1,end:-1});assert.equal(get('getLeftTextOfCursor')(99),'abcd');assert.equal(get('getTextIndexAtCursor')(),4);assert.equal(get('getRightTextOfCursor')(99),'');h.a.close();await tick();});
await test('staging byte cap stops and wipes before Owner confirmation',async()=>{const h=make();h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();h.resolve();await tick();const insert=h.listeners.get('insertText');insert('中'.repeat(4096));insert('中'.repeat(4096));assert.equal(h.a.mode(),false);await tick();assert.equal(h.sources.filter(s=>s.state==='detached').length,1);assert.equal(h.operations.length,0);});
await test('cancelled queued attachment never touches SDK or unrelated editor callbacks',async()=>{const h=make();const foreign=()=>{};h.listeners.set('foreign',foreign);h.a.request({},true,0);h.a.observe(h.snap,true,true,1);h.a.close();await tick();assert.equal(h.controller.ctx,undefined);assert.equal(h.controller.detached||0,0);assert.equal(h.listeners.size,1);assert.equal(h.listeners.get('foreign'),foreign);assert.equal(h.sources.length,0);});
await test('nonsettling detach watchdog blocks fresh adapter and late resolve never resurrects source',async()=>{const h=make();await attached(h);let finishDetach;h.controller.detach=()=>new Promise(r=>{finishDetach=r;});h.a.close();await tick();await new Promise(r=>setTimeout(r,2100));assert.equal(h.notices.at(-1),'系统输入法未响应，请重新打开应用');const b=new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),n=>h.notices.push(n));b.request({},true,2200);assert.equal(b.mode(),false);finishDetach();await tick();assert.equal(h.listeners.size,0);assert.equal(h.sources.filter(s=>s.state==='attached').length,1);assert.equal(h.operations.length,0);});
await test('partial subscription failure removes attempted own callbacks and detaches completed SDK attachment',async()=>{const h=make();const foreign=()=>{};h.listeners.set('foreign',foreign);const original=h.controller.on;h.controller.on=function(n,f){original.call(this,n,f);if(n==='finishTextPreview')throw Error('fixed');};h.a.request({},true,0);h.a.observe(h.snap,true,true,1);await tick();h.resolve();await tick();assert.deepEqual(h.offs,['insertText','setPreviewText','finishTextPreview']);assert.equal(h.listeners.size,1);assert.equal(h.listeners.get('foreign'),foreign);assert.equal(h.controller.detached,1);assert.ok(h.controller.ctx);assert.equal(h.a.mode(),false);});
await test('queued source deadline behind unknown SDK detach blocks process without racing the old cleanup',async()=>{const h=make();await attached(h);let finishDetach;h.controller.detach=()=>new Promise(r=>{finishDetach=r;});h.a.close();await tick();const b=new h.module.RemoteImeAdapter(()=>h.controller,h.bridge,()=>({isBoundary:true,previous:0,next:0}),n=>h.notices.push(n));b.request({},true,10);b.observe(h.snap,true,true,11);await tick();b.observe(h.snap,true,true,2011);assert.equal(b.mode(),false);assert.equal(h.notices.at(-1),'系统输入法未响应，请重新打开应用');finishDetach();await tick();assert.equal(h.listeners.size,0);assert.equal(h.sources.filter(s=>s.state==='attached').length,1);});
// Independent numeric oracle: stage 2 gates, 3 controller, 4 subscribe, 5 attach,
// 6 source, 7 Owner, 8 selection, 9 ready; outcome 3 throw, 4 reject,
// 5 nonzero code, 6 deadline, 7 invalid, 8 process poison. No production enums used.
await test('diagnostic request waits without SDK, reports a gate deadline once and never logs polls', async () => {
  const h = make(); h.a.request({}, true, 0);
  for (let now = 1; now < 1900; now += 100) h.a.observe(h.snap, false, true, now);
  assert.equal(h.listeners.size, 0); assert.equal(h.diagnostics.length, 2);
  h.a.observe(h.snap, false, true, 2001); h.a.observe(h.snap, false, true, 2200);
  const terminal = h.diagnostics.at(-1);
  assert.equal(terminal.stage, 2); assert.equal(terminal.outcome, 6);
  assert.equal(terminal.gateMask & 128, 0); assert.equal(h.diagnostics.length, 3);
});
await test('diagnostic controller and every subscription failure retain only numeric code and exact index', async () => {
  const secret = 'synthetic-secret-body-message-stack';
  for (let index = 0; index <= 14; index++) {
    const error = { code: 12800003, message: secret, stack: secret, context: secret };
    const h = make(index === 0 ? { controllerError: error } : {});
    let calls = 0; const original = h.controller.on;
    h.controller.on = function(name, callback) { original.call(this, name, callback); if (++calls === index) throw error; };
    h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
    if(index>0){h.resolve();await tick();}
    const failure = h.diagnostics.find(event => event.outcome === 3);
    assert.ok(failure); assert.equal(failure.stage, index === 0 ? 3 : 4);
    assert.equal(failure.subscriptionIndex, index); assert.equal(failure.code, 12800003);
    assert.equal(JSON.stringify(h.diagnostics).includes(secret), false);
    assert.equal(h.a.mode(), false); assert.equal(h.controller.detached || 0, index===0?0:1);
    assert.equal(h.controller.attached||0,index===0?0:1);assert.equal(h.offs.length,index);assert.equal(h.sources.length,0);assert.equal(h.controller.shown||0,0);
    assert.equal(h.listeners.size, 0);
  }
});
await test('diagnostic attach sync throw and rejection are distinct and stale completion cannot retag a new request', async () => {
  for (const kind of ['throw', 'reject']) {
    const h = make(); const error = { code: 12800008, message: 'private-fixture', stack: 'private-fixture' };
    if (kind === 'throw') h.controller.attachWithUIContext = () => { throw error; };
    h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
    if (kind === 'reject') { h.reject(error); await tick(); }
    const failure = h.diagnostics.find(event => event.stage === 5 && event.outcome === (kind === 'throw' ? 3 : 4));
    assert.ok(failure); assert.equal(failure.code, 12800008);
    assert.equal(JSON.stringify(h.diagnostics).includes('private-fixture'), false);
  }
  const h = make(); h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
  const first = h.diagnostics[0].attempt; h.a.close(); h.a.request({}, true, 10);
  const second = h.diagnostics.at(-1).attempt; assert.ok(second > first);
  const before = h.diagnostics.length; h.resolve(); await tick();
  assert.equal(h.sources.length, 0); assert.equal(h.diagnostics.length, before);
  h.a.close(); await tick();
});
await test('diagnostic source nonzero and throw differ from Owner exact-ticket timeout', async () => {
  for (const kind of ['code', 'throw', 'owner']) {
    const h = make();
    if (kind === 'code') h.bridge.code = -11;
    if (kind === 'throw') h.bridge.source = () => { throw { code: 71, message: 'private-source' }; };
    h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick(); h.resolve(); await tick();
    if (kind === 'owner') {
      h.snap.ime.state = 'attached'; h.snap.ime.ticket = { ...h.sources[0].ticket, editorSerial: 999 };
      for (let now = 2; now < 1900; now += 100) h.a.observe(h.snap, true, true, now);
      h.a.observe(h.snap, true, true, 2001); await tick();
    }
    const failure = h.diagnostics.find(event => event.stage === (kind === 'owner' ? 7 : 6) && event.outcome === (kind === 'owner' ? 6 : kind === 'throw' ? 3 : 5));
    assert.ok(failure); assert.equal(failure.code, kind === 'code' ? -11 : kind === 'throw' ? 71 : -2147483648);
    if (kind === 'owner') { assert.equal(failure.ownerState, 1); assert.equal(failure.ticketEqual, false);assert.equal(h.controller.shown||0,0); }
    assert.equal(h.a.mode(), false);
  }
});
await test('diagnostic initial selection throws and rejects are isolated from SDK attach and Owner acceptance', async () => {
  for (const kind of ['throw', 'reject']) {
    const h = make(); const error = { code: 401, message: 'selection-private' };
    h.controller.changeSelection = () => { if (kind === 'throw') throw error; return Promise.reject(error); };
    h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick(); h.resolve(); await tick();
    assert.ok(h.diagnostics.some(event => event.stage === 6 && event.outcome === 2));
    const failure = h.diagnostics.find(event => event.stage === 8 && event.outcome === (kind === 'throw' ? 3 : 4));
    assert.ok(failure); assert.equal(failure.code, 401); assert.equal(h.a.mode(), false);assert.equal(h.controller.detached,1);assert.equal(h.controller.shown||0,0);assert.equal(h.diagnostics.some(e=>e.stage===9),false);
  }
});
await test('diagnostic callback throws cannot alter ready text flow or cleanup; repeated selection does not flood records', async () => {
  const h = make({ diagnosticThrows: true }); await attached(h);
  assert.ok(h.diagnostics.some(event => event.stage === 9 && event.outcome === 2));
  const before = h.diagnostics.length;
  for (let i = 0; i < 100; i++) { h.a.observe(h.snap, true, true, 3 + i); h.listeners.get('insertText')('同'); }
  await tick(); assert.equal(h.diagnostics.length, before); assert.equal(h.operations.length, 100);
  h.a.close(); await tick(); assert.equal(h.listeners.size, 0);
  for (const event of h.diagnostics) for (const value of Object.values(event)) assert.ok(typeof value === 'number' || typeof value === 'boolean');
});
await test('diagnostic invalid focus window session source and Owner loss use finite gate comparisons', async () => {
  for (const kind of ['focus', 'window', 'session', 'source', 'owner', 'ack']) {
    const h = make(); await attached(h);
    if (kind === 'window') h.snap.localDisplayEvidence.windowId++;
    if (kind === 'session') h.snap.session = '200';
    if (kind === 'source') h.snap.ime.sourceAvailable = false;
    if (kind === 'owner') h.snap.ime.ticket = null;
    if (kind === 'ack') { h.snap.ime.ackTicket = h.snap.ime.ticket; h.snap.ime.lastAcceptedSerial = 1; }
    h.a.observe(h.snap, kind !== 'focus', true, 5); await tick();
    const failure = h.diagnostics.find(event => event.outcome === (kind === 'ack' ? 10 : 7));
    assert.ok(failure, kind); assert.equal(failure.stage, kind === 'owner' || kind === 'ack' ? 7 : 2);
    const absentGate = { focus: 128, window: 2048, session: 4096, source: 256 }[kind];
    if (absentGate) assert.equal(failure.gateMask & absentGate, 0, kind);
    assert.equal(h.a.mode(), false, kind);
  }
});
await test('diagnostic process poison is distinguishable and unsafe SDK code cannot enter payload', async () => {
  const h = make(); h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
  h.a.observe(h.snap, true, true, 2001); h.a.request({}, true, 2010);
  assert.ok(h.diagnostics.some(event => event.stage === 5 && event.outcome === 6));
  assert.equal(h.diagnostics.at(-1).outcome, 8); h.resolve(); await tick();
  for (const code of [NaN, Infinity, 1.5, Number.MAX_SAFE_INTEGER, '12800003']) {
    const f = make({ controllerError: { code, message: 'secret' } });
    f.a.request({}, true, 0); f.a.observe(f.snap, true, true, 1);
    assert.equal(f.diagnostics.at(-1).code, -2147483648);
  }
  const f = make({ controllerError: Object.defineProperty({}, 'code', { get() { throw Error('private getter'); } }) });
  f.a.request({}, true, 0); f.a.observe(f.snap, true, true, 1);
  assert.equal(f.diagnostics.at(-1).code, -2147483648);
});
// I1: drive the product's actual timer callbacks with an independent logical clock.
// A process poison on a waiting request is stage1/outcome8; stage5/outcome6
// is reserved for that request's own pending attach. No production helper computes expected stages.
function logicalTimers() {
  let now = 0, serial = 0;
  const pending = new Map();
  return {
    setTimeout(callback, milliseconds) { const id = ++serial; pending.set(id, { at: now + milliseconds, callback }); return id; },
    clearTimeout(id) { pending.delete(id); },
    advance(value) { now = value; },
    next() { assert.equal(pending.size, 1); return [...pending.entries()][0]; },
    fire(id) { const timer = pending.get(id); assert.ok(timer); now = timer.at; pending.delete(id); timer.callback(); }
  };
}
function terminalEvents(h, attempt) {
  return h.diagnostics.filter(event => event.attempt === attempt &&
    (event.outcome >= 3 || event.stage === 10));
}
await test('I1 old attach watchdog poisons the newer same-adapter request without claiming its cleanup timed out', async () => {
  const timers = logicalTimers(), h = make({ timers });
  h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
  const first = h.diagnostics[0].attempt, watchdog = timers.next()[0];
  timers.advance(100); h.a.request({}, true, 100); h.a.observe(h.snap, true, true, 101); await tick();
  const second = h.diagnostics.at(-1).attempt;
  const oldTerminal = JSON.stringify(terminalEvents(h, first));
  assert.equal(h.controller.attached, 1); assert.equal(h.controller.detached || 0, 0);
  timers.fire(watchdog);
  const current = terminalEvents(h, second);
  console.log('I1 attach interleave ' + JSON.stringify({ first, second, attachCalls: h.controller.attached,
    detachCalls: h.controller.detached || 0, oldTerminal: terminalEvents(h, first), current }));
  assert.equal(current.length, 1); assert.equal(current[0].stage, 1); assert.equal(current[0].outcome, 8);
  assert.equal(current[0].progressMask & 24, 0, 'new request never began its own SDK attach');
  assert.equal(JSON.stringify(terminalEvents(h, first)), oldTerminal, 'old first terminal stays closed');
  assert.equal(h.a.mode(), false);
  const before = h.diagnostics.length; h.resolve(); await tick();
  assert.equal(h.diagnostics.length, before); assert.equal(h.controller.attached, 1);
  assert.equal(h.controller.detached, 1); assert.equal(h.listeners.size, 0); assert.equal(h.sources.length, 0);
  h.a.request({}, true, 2200); assert.equal(h.a.mode(), false);
  assert.equal(h.diagnostics.at(-1).outcome, 8, 'process remains poisoned after late completion');
});
await test('I1 already-running old detach watchdog is process poison for a newer same-adapter request', async () => {
  const timers = logicalTimers(), h = make({ timers }); await attached(h);
  const first = h.diagnostics[0].attempt;
  let finishDetach;
  h.controller.detach = () => { h.controller.detached = (h.controller.detached || 0) + 1; return new Promise(resolve => { finishDetach = resolve; }); };
  timers.advance(10); h.a.close(); await tick(); const watchdog = timers.next()[0];
  timers.advance(100); h.a.request({}, true, 100); h.a.observe(h.snap, true, true, 101); await tick();
  const second = h.diagnostics.at(-1).attempt, oldTerminal = JSON.stringify(terminalEvents(h, first));
  timers.fire(watchdog);
  const current = terminalEvents(h, second);
  console.log('I1 detach interleave ' + JSON.stringify({ first, second, attachCalls: h.controller.attached,
    detachCalls: h.controller.detached, current }));
  assert.equal(current.length, 1); assert.equal(current[0].stage, 1); assert.equal(current[0].outcome, 8);
  assert.equal(JSON.stringify(terminalEvents(h, first)), oldTerminal);
  assert.equal(h.a.mode(), false); assert.equal(h.controller.attached, 1); assert.equal(h.controller.detached, 1);
  const before = h.diagnostics.length; finishDetach(); await tick();
  assert.equal(h.diagnostics.length, before); assert.equal(h.controller.attached, 1); assert.equal(h.listeners.size, 0);
});
for (const trigger of ['watchdog', 'deadline']) {
  await test('I1 cleanup queued by old attempt retains ownership with ' + trigger, async () => {
    const timers = logicalTimers(), h = make({ timers });
    let finishDetach;
    h.controller.detach = () => { h.controller.detached = (h.controller.detached || 0) + 1; return new Promise(resolve => { finishDetach = resolve; }); };
    h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
    const first = h.diagnostics[0].attempt;
    timers.advance(100); h.a.request({}, true, 100); h.a.observe(h.snap, true, true, 101); await tick();
    const second = h.diagnostics.at(-1).attempt, oldTerminal = JSON.stringify(terminalEvents(h, first));
    assert.equal(h.controller.detached || 0, 0, 'cleanup waits for the original attach Promise');
    timers.advance(200); h.resolve(); await tick();
    const [watchdog, timer] = timers.next(); assert.equal(timer.at, 2200);
    assert.equal(h.controller.attached, 1); assert.equal(h.controller.detached, 1);
    if (trigger === 'watchdog') timers.fire(watchdog);
    else { timers.advance(2101); h.a.observe(h.snap, true, true, 2101); }
    const current = terminalEvents(h, second);
    console.log('I1 queued cleanup ' + JSON.stringify({ trigger, first, second,
      attachCalls: h.controller.attached, detachCalls: h.controller.detached, current }));
    assert.equal(current.length, 1); assert.equal(current[0].stage, 1); assert.equal(current[0].outcome, 8);
    assert.equal(current[0].progressMask & 24, 0); assert.equal(h.a.mode(), false);
    assert.equal(JSON.stringify(terminalEvents(h, first)), oldTerminal);
    const before = h.diagnostics.length;
    if (trigger === 'deadline') timers.fire(watchdog);
    assert.equal(h.diagnostics.length, before, 'later watchdog cannot reopen the waiting attempt terminal');
    finishDetach(); await tick(); assert.equal(h.diagnostics.length, before);
    assert.equal(h.controller.attached, 1); assert.equal(h.listeners.size, 0); assert.equal(h.sources.length, 0);
  });
}
await test('I1 cross-adapter deadline while old detach is busy reports process poison rather than own attach timeout', async () => {
  const timers = logicalTimers(), h = make({ timers }); await attached(h);
  let finishDetach;
  h.controller.detach = () => { h.controller.detached = (h.controller.detached || 0) + 1; return new Promise(resolve => { finishDetach = resolve; }); };
  const diagnostics = [];
  const b = new h.module.RemoteImeAdapter(() => h.controller, h.bridge, () => ({ isBoundary: true, previous: 0, next: 0 }),
    message => h.notices.push(message), event => diagnostics.push(event));
  timers.advance(100); b.request({}, true, 100);
  timers.advance(200); h.a.close(); await tick();
  b.observe(h.snap, true, true, 201); await tick();
  timers.advance(2101); b.observe(h.snap, true, true, 2101);
  console.log('I1 cross-adapter deadline ' + JSON.stringify({ attachCalls: h.controller.attached,
    detachCalls: h.controller.detached, current: diagnostics.at(-1) }));
  assert.equal(diagnostics.at(-1).stage, 1); assert.equal(diagnostics.at(-1).outcome, 8);
  assert.equal(b.mode(), false); assert.equal(h.controller.attached, 1);
  finishDetach(); await tick(); assert.equal(h.controller.attached, 1); assert.equal(h.listeners.size, 0);
});
await test('I1 current pending attach watchdog and observe deadline still report their own attach timeout', async () => {
  for (const trigger of ['watchdog', 'deadline']) {
    const timers = logicalTimers(), h = make({ timers });
    h.a.request({}, true, 0); h.a.observe(h.snap, true, true, 1); await tick();
    const first = h.diagnostics[0].attempt, watchdog = timers.next()[0];
    if (trigger === 'watchdog') timers.fire(watchdog); else h.a.observe(h.snap, true, true, 2001);
    const current = terminalEvents(h, first);
    assert.equal(current.length, 1); assert.equal(current[0].stage, 5); assert.equal(current[0].outcome, 6);
    assert.equal(h.a.mode(), false); assert.equal(h.controller.detached || 0, 0);
    const before = h.diagnostics.length; h.resolve(); await tick();
    assert.equal(h.diagnostics.length, before); assert.equal(h.controller.attached, 1);
    assert.equal(h.controller.detached, 1); assert.equal(h.sources.length, 0);
  }
});
console.log(`${count} real adapter tests passed`);
})().catch(e=>{console.error(e);process.exitCode=1;});
