const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync(`${__dirname}/../ui/index.html`, 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const elements = new Map();
const context = vm.createContext({
  document: {getElementById(id) {
    if (!elements.has(id)) elements.set(id, {value:'',style:{},focus(){},showModal(){this.open=true;},close(){this.open=false;this.onclose?.();}});
    return elements.get(id);
  }}, window:{}, setTimeout:()=>1, clearTimeout:()=>{},
});
vm.runInContext(script.slice(0, script.lastIndexOf('(async()=>{await list();')), context);
const run = code=>vm.runInContext(code, context);
const calls=[];
context.fakeApi=(op,args)=>new Promise((resolve,reject)=>calls.push({op,args,resolve,reject}));
run('api=fakeApi');
const settings={terms:['SampleName','Example Product'],use_meeting_context:true};
const element=id=>elements.get(`vocabulary-${id}`);
async function open() {
  const promise=run('showVocabulary()');
  assert.equal(calls[0].op,'vocabulary');
  calls.shift().resolve(settings);
  await promise;
}
(async()=>{
  const loading=run('showVocabulary()');
  assert.equal(element('dialog').open,true);
  assert.equal(element('save').disabled,true);
  const oldLoad=calls.shift();
  element('cancel').onclick();
  const reopened=run('showVocabulary()');
  calls.shift().resolve(settings);
  await reopened;
  oldLoad.resolve({terms:['Stale'],use_meeting_context:false});
  await loading;
  assert.equal(element('terms').value,'SampleName\nExample Product','late loading replies must not overwrite a reopened editor');
  assert.equal(element('context').checked,true);
  element('cancel').onclick();
  assert.equal(calls.length,0,'Cancel must not write settings');

  await open();
  // A CLI change to terms while this dialog is open must survive a context-only save.
  element('context').checked=false;
  const saving=run('saveVocabulary()');
  assert.equal(calls[0].op,'save_vocabulary');
  assert.equal(calls[0].args.use_meeting_context,false);
  assert.equal('terms' in calls[0].args,false);
  assert.equal(element('save').disabled,true);
  let prevented=false;
  element('dialog').oncancel({preventDefault(){prevented=true;}});
  assert.equal(prevented,true);
  await run('saveVocabulary()');
  assert.equal(calls.length,1,'double Save must not duplicate writes');
  calls.shift().resolve({...settings,use_meeting_context:false});
  await saving;
  assert.equal(element('dialog').open,false);

  await open();
  element('terms').value='Another Product';
  const failing=run('saveVocabulary()');
  assert.equal('use_meeting_context' in calls[0].args,false);
  calls.shift().reject(new Error('Disk unavailable'));
  await failing;
  assert.equal(element('dialog').open,true);
  assert.equal(element('terms').value,'Another Product');
  assert.equal(element('state').textContent,'Disk unavailable');
  assert.equal(element('save').disabled,false);
  const retry=run('saveVocabulary()');
  calls.shift().resolve(settings);
  await retry;

  const badLoad=run('showVocabulary()');
  calls.shift().reject(new Error('Invalid local settings'));
  await badLoad;
  element('terms').value='Repaired Product';
  const repair=run('saveVocabulary()');
  assert.equal(calls[0].args.terms,'Repaired Product');
  assert.equal(calls[0].args.use_meeting_context,true);
  calls.shift().resolve(settings);
  await repair;
  await open();
  await run('saveVocabulary()');
  assert.equal(calls.length,0,'unchanged settings must not overwrite concurrent CLI updates');
  console.log('Vocabulary UI checks passed: loading/cancel races, partial updates, save/retry, duplicate-click guard, invalid-file repair.');
})().catch(error=>{console.error(error);process.exitCode=1;});
