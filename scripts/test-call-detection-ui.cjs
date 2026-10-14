const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

// Run the actual shipped banner controls with synthetic activity. No microphone,
// app GUI, user archive, or browser permissions are used by this check.
const html = fs.readFileSync(`${__dirname}/../ui/index.html`, 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const elements = new Map();
const context = vm.createContext({
  document: {getElementById(id) {
    if (!elements.has(id)) elements.set(id, {value:'',style:{},click(){return this.onclick?.();}});
    return elements.get(id);
  }},
  window: {}, setTimeout:()=>1, clearTimeout:()=>{},
});
vm.runInContext(script.slice(0, script.lastIndexOf('(async()=>{await list();')), context);
const run = code => vm.runInContext(code, context);
const calls = [];
let recordClicks = 0;
let dismissResponse = {call_detection:{enabled:true,possible_call:false}};
const shippedRecordHandler = elements.get('record').onclick;
elements.get('record').onclick = () => { recordClicks++; };
context.fakeApi = async (op, args) => {
  calls.push({op,args});
  if (op === 'call_dismiss') return dismissResponse;
  if (op === 'call_detection') return {call_detection:{enabled:args.enabled,possible_call:false}};
  if (op === 'start') return {};
  throw new Error(`Unexpected action: ${op}`);
};
run('api=fakeApi');
(async()=>{
  run('lastState={call_detection:{enabled:true,possible_call:true}};updateCallDetection()');
  assert.equal(elements.get('call-hint').hidden, false);
  assert.equal(recordClicks, 0, 'detection must never start a recording');
  assert.equal(calls.length, 0);
  await elements.get('call-ignore').onclick();
  assert.equal(calls.shift().op, 'call_dismiss');
  assert.equal(elements.get('call-hint').hidden, true);
  assert.equal(recordClicks, 0);
  for (const flag of ['recording','preparing','transcribing']) {
    run(`lastState={${flag}:true,call_detection:{enabled:true,possible_call:true}};updateCallDetection()`);
    assert.equal(elements.get('call-hint').hidden, true, `${flag} suppresses the prompt`);
  }
  run('lastState={call_detection:{enabled:true,possible_call:true}};updateCallDetection()');
  await elements.get('call-start').onclick();
  assert.equal(calls.shift().op, 'call_dismiss');
  assert.equal(recordClicks, 1, 'recording requires the explicit Start recording click');
  for (const flag of ['recording','preparing','transcribing']) {
    dismissResponse = {[flag]:true,call_detection:{enabled:true,possible_call:false}};
    await elements.get('call-start').onclick();
    assert.equal(calls.shift().op, 'call_dismiss');
    assert.equal(recordClicks, 1, 'a stale Start hint must never stop or duplicate an active capture');
  }
  elements.get('detect-calls').checked = false;
  await elements.get('detect-calls').onchange();
  const preference = calls.shift();
  assert.equal(preference.op, 'call_detection');
  assert.equal(preference.args.enabled, false);
  assert.equal(elements.get('detect-calls').checked, false);
  run('lastState={call_detection:{enabled:true,possible_call:false,unavailable:true}};updateCallDetection()');
  assert.match(elements.get('detection-detail').textContent, /unavailable/);
  assert.equal(elements.get('call-hint').hidden, true);
  assert.equal(recordClicks, 1);
  // If polling observes a CLI-started recording while notes are saving, the
  // toolbar's original Start intent must not turn into a Stop command.
  elements.get('record').onclick = shippedRecordHandler;
  run(`selected={meeting:{id:'synthetic',status:'draft'}};lastState={recording:false};
    $('title').value='Synthetic';save=async()=>{lastState={recording:true};};
    poll=async()=>{};list=async()=>{};`);
  await elements.get('record').onclick();
  assert.equal(calls.shift().op, 'start');
  assert.equal(calls.length, 0);
  console.log('Call detection UI checks passed: explicit consent, ignore, busy suppression, opt-out, unavailable state.');
})().catch(error=>{console.error(error);process.exitCode=1;});
