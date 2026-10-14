const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

// Exercise the shipped editor code with delayed IPC replies, without native UI.
const html = fs.readFileSync(`${__dirname}/../ui/index.html`, 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const elements = new Map();
const context = vm.createContext({
  document: { getElementById(id) {
    if (!elements.has(id)) elements.set(id, { value: '', textContent: '', style: {}, focus(){}, showModal(){this.open=true;}, close(){this.open=false;this.onclose?.();} });
    return elements.get(id);
  } },
  window: {}, setTimeout: () => 1, clearTimeout: () => {},
});
vm.runInContext(script.slice(0, script.lastIndexOf('(async()=>{await list();')), context);
const run = code => vm.runInContext(code, context);
const tick = () => new Promise(resolve => setImmediate(resolve));
const calls = [];
context.fakeApi = (op, args) => new Promise((resolve, reject) => calls.push({ op, args, resolve, reject }));
run(`api=fakeApi;list=async()=>{};renderDetail=t=>{selected=t;dirty=false;};
  selected={meeting:{id:'first',title:'First'},notes:''};
  $('title').value='First';$('notes').value='Initial notes';$('notes').oninput();`);

(async () => {
  const saving = run('save()');
  assert.equal(calls[0].args.notes, 'Initial notes');
  run(`$('notes').value='Newest notes';$('notes').oninput();`);
  calls.shift().resolve({});
  await tick();
  assert.equal(run('dirty'), true, 'an older reply must not mark newer typing saved');
  assert.equal(calls[0].args.notes, 'Newest notes');
  calls.shift().resolve({});
  await saving;
  assert.equal(run('dirty'), false);
  assert.equal(run('selected.notes'), 'Newest notes');

  run(`$('notes').value='Save before leaving';$('notes').oninput();`);
  const switching = run(`choose('second')`);
  assert.equal(calls[0].op, 'save');
  assert.equal(run('selected.meeting.id'), 'first');
  calls.shift().resolve({});
  await tick();
  assert.equal(calls[0].op, 'get');
  calls.shift().resolve({ meeting: { id: 'second' }, notes: 'Other meeting' });
  await switching;
  assert.equal(run('selected.meeting.id'), 'second');

  run(`$('title').value='Second';$('notes').value='Keep after failure';$('notes').oninput();`);
  const failing = run('save()');
  calls.shift().reject(new Error('disk unavailable'));
  await assert.rejects(failing, /disk unavailable/);
  assert.equal(run('dirty'), true);
  const retry = run('save()');
  assert.equal(calls[0].args.notes, 'Keep after failure');
  calls.shift().resolve({});
  await retry;
  assert.equal(run('dirty'), false);
  run('showDelete()');
  assert.equal(elements.get('delete-dialog').open, true);
  run("$('delete-cancel').onclick()");
  assert.equal(calls.length, 0, 'cancelling must never send a deletion');
  assert.equal(run('selected.meeting.id'), 'second');
  run("lastState={recording:true};showDelete()");
  assert.equal(elements.get('delete-dialog').open, false, 'active capture disables deletion');
  run('lastState={};showDelete()');
  const deletion = run('confirmDelete()');
  assert.equal(calls[0].op, 'delete');
  assert.equal(calls[0].args.id, 'second');
  assert.equal(elements.get('delete-cancel').disabled, true);
  calls.shift().resolve({deleted:true});
  await deletion;
  assert.equal(run('selected'), null);
  assert.equal(elements.get('empty').hidden, false);
  assert.equal(elements.get('detail').hidden, true);
  assert.equal(elements.get('delete-dialog').open, false);
  console.log('Editor checks passed: autosave, navigation, retry, delete confirmation/cancel, active capture guard, empty library.');
})().catch(error => { console.error(error); process.exitCode = 1; });
