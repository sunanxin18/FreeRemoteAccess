// SDK ArkTS parser/transpiler + real bounded editor. All strings are synthetic fixtures.
const assert = require('node:assert/strict'), fs=require('node:fs'), path=require('node:path'), vm=require('node:vm');
const ts=require(process.env.FRD_ARKTS_MODULE); assert.ok(ts.ScriptKind.ETS!==undefined);
function load(name, modules={}) { const p=path.resolve(__dirname, '../main/ets/common/'+name+'.ets');
 const source=fs.existsSync(p)?fs.readFileSync(p,'utf8'):'';
 const ast=ts.createSourceFile(p,source,ts.ScriptTarget.Latest,true,ts.ScriptKind.ETS); assert.equal(ast.parseDiagnostics.length,0);
 const out=ts.transpileModule(source,{fileName:p,compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020},reportDiagnostics:true}); assert.equal(out.diagnostics.length,0);
 const exports={}; vm.runInNewContext(out.outputText,{exports,require:n=>modules[n]}); return exports; }
const policy=load('RemoteImePolicy');
let count=0; function test(n,f){f(); count++; console.log('PASS '+n);}
function neighbors(text,cursor){const bounds=[0,...Array.from(new Intl.Segmenter('und',{granularity:'grapheme'}).segment(text),s=>s.index+s.segment.length)]; return {isBoundary:bounds.includes(cursor),previous:bounds.filter(b=>b<cursor).at(-1)??0,next:bounds.find(b=>b>cursor)??text.length};}
function editor(g=neighbors){const output=[], notices=[]; assert.equal(typeof policy.RemoteImePolicy,'function','normal realtime editor must exist'); const p=new policy.RemoteImePolicy(g,o=>{output.push(JSON.parse(JSON.stringify(o))); return true;},()=>notices.push('context')); return {p,output,notices};}
function texts(output){return output.filter(o=>o.kind==='text').map(o=>o.text);}
test('P/I/F independently distinguish repeated text and cancellation',()=>{
 const cases=[[[['i','你'],['i','你']],['你','你']],[[['p','ni'],['p','你'],['f']],['你']],[[['p','ni'],['i','你'],['f']],['你']],[[['p','你'],['f'],['i','你']],['你','你']],[[['p','你'],['f'],['f']],['你']],[[['p','ni'],['p',''],['f']],[]],[[['p','ni'],['x'],['f']],[]]];
 for(const [ops,expected] of cases){const {p,output}=editor(); for(const [kind,value] of ops){if(kind==='i')p.insert(value);if(kind==='p')p.preview(value,{start:-1,end:-1});if(kind==='f')p.finish();if(kind==='x')p.invalidate();} assert.deepEqual(texts(output),expected); assert.deepEqual(output.map(o=>o.serial),expected.map((_,i)=>i+1));}
});
test('half-open UTF16 replacement and selected local preview priority',()=>{
 let {p,output}=editor(); p.preview('abcd',{start:0,end:0});p.preview('X',{start:1,end:3});p.finish();assert.deepEqual(texts(output),['aXd']);
 ({p,output}=editor());p.preview('abcd',{start:-1,end:-1});p.select({start:1,end:3});p.insert('好');p.finish();assert.deepEqual(texts(output),['a好d']);
 ({p,output}=editor());p.preview('A😀中',{start:-1,end:-1});p.preview('X',{start:1,end:3});p.finish();assert.deepEqual(texts(output),['AX中']);
});
test('invalid ranges reset local context without remote output and next input works',()=>{
 for(const range of [{start:-1,end:0},{start:2,end:3},{start:3,end:1},{start:0,end:99},{start:NaN,end:0},{start:0.5,end:1}]){const {p,output,notices}=editor();p.preview('A😀中',{start:-1,end:-1});p.preview('x',range);assert.equal(p.index(),0);assert.equal(notices.length,1);p.insert('好');assert.deepEqual(texts(output),['好']);}
 const {p,output}=editor(); p.insert('\ud800');p.insert('a'.repeat(4097));p.insert('可');assert.deepEqual(texts(output),['可']);
});
test('fixed EGC fixtures local delete and move use complete system boundaries',()=>{
 for(const [text,bounds] of [['A😀中',[0,1,3,4]],['e\u0301',[0,2]],['👩‍💻',[0,5]],['👍🏽',[0,4]],['🇨🇳',[0,4]],['\r\n',[0,2]],['각',[0,3]]]) {
  for(const c of bounds){const n=neighbors(text,c);assert.equal(n.isBoundary,true);assert.equal(n.previous,bounds[Math.max(0,bounds.indexOf(c)-1)]);assert.equal(n.next,bounds[Math.min(bounds.length-1,bounds.indexOf(c)+1)]);}
  const {p,output}=editor();p.preview(text,{start:-1,end:-1});p.delete(1,true); assert.equal(p.index(),bounds.at(-2)); p.finish();assert.deepEqual(texts(output),text.slice(0,bounds.at(-2))?[text.slice(0,bounds.at(-2))]:[]);
 }
 const {p,output,notices}=editor();p.preview('e\u0301',{start:-1,end:-1});p.select({start:1,end:1});p.delete(1,true);assert.equal(notices.length,1);assert.equal(output.length,0);p.insert('次');assert.deepEqual(texts(output),['次']);
});
test('selection deletion once, local movement and immediate scalar-safe context',()=>{
 const {p,output}=editor();p.preview('A😀中',{start:-1,end:-1});p.move(3,false);assert.equal(p.index(),3);assert.equal(p.left(1),'');assert.equal(p.left(2),'😀');assert.equal(p.right(9),'中');p.select({start:1,end:3});assert.equal(p.left(9),'A');assert.equal(p.right(9),'中');p.delete(99,true);p.finish();assert.deepEqual(texts(output),['A中']);assert.equal(p.index(),0);assert.equal(p.left(99),'');
});
test('unknown remote edits refuse once; ordinary control pairs and text remain available',()=>{
 const {p,output,notices}=editor();p.delete(2,true);p.select({start:0,end:1});p.move(3,true);p.extend(3);p.delete(1,true);p.delete(1,false);p.move(1,false);p.functionKey(8);p.functionKey(1);p.insert('同');assert.deepEqual(output.map(o=>o.key||o.text),['Backspace','Delete','ArrowUp','Enter','同']);assert.equal(notices.length,4);
});
test('ICU failure limits local control only and explicit scalar range remains allowed',()=>{
 const {p,output}=editor(()=>{throw Error('fixed');});p.preview('e\u0301',{start:-1,end:-1});p.move(3,false);p.preview('e\u0301',{start:-1,end:-1});p.preview('X',{start:1,end:2});p.finish();p.delete(1,true);assert.deepEqual(output.map(o=>o.text||o.key),['eX','Backspace']);
});
test('left selection exposes ordered SDK range while preserving real caret and selected preview edits',()=>{
 const {p,output}=editor();p.preview('abcd',{start:-1,end:-1});p.move(3,true);p.move(3,true);assert.deepEqual(JSON.parse(JSON.stringify(p.selection())),{start:2,end:4});assert.equal(p.index(),2);p.preview('',{start:-1,end:-1});p.finish();assert.deepEqual(texts(output),['ab']);
});
console.log(`${count} real editor tests passed`);
module.exports={load,policy,neighbors};
