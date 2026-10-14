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

  run(`selected={meeting:{id:'copy-fixture'},notes:'Never copied',segments:[]};updateCopyButton();`);
  assert.equal(elements.get('copy-transcript').disabled, true);
  await run('copyTranscript()');
  assert.equal(calls.length, 0, 'empty transcripts must not replace the clipboard');
  run(`selected.segments=[{start_ms:0,source:'You',text:'Synthetic transcript'}];updateCopyButton();`);
  assert.equal(elements.get('copy-transcript').disabled, false);
  const copying = run('copyTranscript()');
  assert.equal(elements.get('copy-transcript').textContent, 'Copying…');
  assert.equal(elements.get('copy-transcript').disabled, true);
  assert.equal(calls[0].op, 'copy_transcript');
  assert.equal(calls[0].args.id, 'copy-fixture');
  assert.deepEqual(Object.keys(calls[0].args), ['id'], 'notes must not be included in the request');
  await run('copyTranscript()');
  assert.equal(calls.length, 1, 'repeated clicks must not duplicate the clipboard request');
  calls.shift().resolve({copied:true});
  await copying;
  assert.equal(elements.get('copy-transcript').textContent, 'Copied!');
  assert.equal(elements.get('copy-transcript').disabled, false);

  const copyError = run('copyTranscript()');
  calls.shift().reject(new Error('Clipboard unavailable'));
  await copyError;
  assert.equal(elements.get('error').textContent, 'Clipboard unavailable');
  assert.equal(elements.get('copy-transcript').textContent, 'Copy transcript');
  assert.equal(elements.get('copy-transcript').disabled, false, 'a failed copy must allow retry');

  const switchingCopy = run('copyTranscript()');
  run(`selected={meeting:{id:'other-copy-fixture'},segments:[{text:'Another transcript'}]};`);
  calls.shift().resolve({copied:true});
  await switchingCopy;
  assert.equal(elements.get('copy-transcript').textContent, 'Copy transcript', 'old replies must not report a different meeting copied');
  console.log('Editor checks passed: autosave, navigation, retry, delete confirmation/cancel, active capture guard, empty library, transcript copy success/empty/error/navigation.');
})().catch(error => { console.error(error); process.exitCode = 1; });
