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

  // A notes-only autosave must accept the authoritative generated title.
  run(`selected={meeting:{id:'titled',title:'Untitled meeting',title_revision:0},notes:''};titleEdited=false;
    $('title').value='Untitled meeting';$('notes').value='Keep my agenda';$('notes').oninput();`);
  const notesSave = run('save()');
  assert.equal(calls[0].args.title_edited, false);
  calls.shift().resolve({meeting:{id:'titled',title:'Product Launch Planning',title_revision:1}});
  await notesSave;
  assert.equal(elements.get('title').value, 'Product Launch Planning');
  assert.equal(run('selected.notes'), 'Keep my agenda');

  // Same-text title input still explicitly claims the title for the user.
  run(`$('title').oninput();`);
  const manualSave = run('save()');
  assert.equal(calls[0].args.title_edited, true);
  run(`$('title').value='My own meeting title';$('title').oninput();`);
  calls.shift().resolve({meeting:{id:'titled',title:'Product Launch Planning',title_revision:2}});
  await tick();
  assert.equal(elements.get('title').value, 'My own meeting title', 'old save replies must preserve newer title input');
  assert.equal(calls[0].args.title, 'My own meeting title');
  assert.equal(calls[0].args.title_edited, true);
  calls.shift().resolve({meeting:{id:'titled',title:'My own meeting title',title_revision:3}});
  await manualSave;
  assert.equal(run('dirty'), false);
  assert.equal(run('titleEdited'), false);

  // Polling can refresh a title without any transcript/status change.
  run(`renderTranscript=()=>{};selected.segments=[];selected.meeting.status='complete';`);
  const titlePoll = run('poll()');
  calls.shift().resolve({});
  await tick();
  assert.equal(calls[0].op, 'get');
  calls.shift().resolve({meeting:{id:'titled',status:'complete',title:'New Automatic Title',title_revision:4},segments:[]});
  await titlePoll;
  assert.equal(elements.get('title').value, 'New Automatic Title');
  const typingPoll = run('poll()');
  calls.shift().resolve({});
  await tick();
  run(`$('title').value='Typing during a poll';$('title').oninput();`);
  calls.shift().resolve({meeting:{id:'titled',status:'complete',title:'Late Automatic Title',title_revision:5},segments:[]});
  await typingPoll;
  assert.equal(elements.get('title').value, 'Typing during a poll');
  run('dirty=false;titleEdited=false;');
  const savedDuringPoll = run('poll()');
  calls.shift().resolve({});
  await tick();
  const olderGet = calls.shift();
  run(`$('title').value='A saved custom title';$('title').oninput();`);
  const savingDuringPoll = run('save()');
  calls.shift().resolve({meeting:{id:'titled',status:'complete',title:'A saved custom title',title_revision:6}});
  await savingDuringPoll;
  olderGet.resolve({meeting:{id:'titled',status:'complete',title:'Old placeholder',title_revision:4},segments:[]});
  await savedDuringPoll;
  assert.equal(elements.get('title').value, 'A saved custom title', 'an older poll must not revert a completed title save');

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
  console.log('Editor checks passed: autosave, navigation, retry, delete confirmation/cancel, active capture guard, empty library, transcript copy success/empty/error/navigation, title ownership and asynchronous title refresh.');
})().catch(error => { console.error(error); process.exitCode = 1; });
