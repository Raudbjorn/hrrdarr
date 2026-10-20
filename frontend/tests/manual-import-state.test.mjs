import test from 'node:test';
import assert from 'node:assert/strict';
import * as state from '../src/lib/manual-import-state.ts';
const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const entry = (n = 1, media = 'episode', extra = {}) => {
  const result = state.createManualImportEntry(id(n), { target: {media_type:media,id:n}, source:`/incoming/${n}.mkv`, destination:`/library/${n}.mkv`, mode:'copy', ...extra }, `Item ${n}`, {domain:media === 'movie' ? 'movies' : 'tv',id:media === 'movie' ? (extra.target?.id ?? n) : n});
  assert.equal(result.ok, true); return result.data;
};
const operation = e => ({id:e.operationId,target:{...e.request.target},status:'preview',message:'Ready',error_code:null,path_failure_code:null,recovery_required:false});
const command = (e, n = 11) => ({id:id(n),batch_id:id(900),operation_id:e.operationId,priority:'normal',status:'queued',attempts:0,next_attempt_at:10,created_at:10,started_at:null,completed_at:null,error_code:null});
const receipt = entries => ({version:2,entries});
const ok = r => { assert.equal(r.ok, true, r.error); return r.data; };
const bad = r => assert.equal(r.ok, false);

test('receipt round-trip preserves identities and intent, never restored freshness or observations', () => {
  let e = state.markExecutionUncertain(state.markAdmissionUncertain(entry()));
  e = ok(state.recordKnownCommand(e, command(e)));
  e = ok(state.markCancelUncertain(e,id(11)));
  const encoded = ok(state.encodeManualImportReceipt(receipt([e])));
  assert.deepEqual(ok(state.decodeManualImportReceipt(encoded)),receipt([e]));
  assert.equal(encoded.includes('status'),false);
  bad(state.decodeManualImportReceipt(JSON.stringify({...receipt([e]),fresh:true})));
  bad(state.decodeManualImportReceipt(JSON.stringify(receipt([{...e,fresh:true}]))));
  bad(state.decodeManualImportReceipt('{'));
  bad(state.decodeManualImportReceipt(JSON.stringify({version:1,entries:[]})));
});

test('captured request and owner are copied and frozen', () => {
  const req = {target:{media_type:'movie',id:4},source:'/a',destination:'/b',mode:'move'};
  const owner = {domain:'movies',id:4};
  const e = ok(state.createManualImportEntry(id(4),req,'Movie',owner));
  req.target.id=8; req.source='/changed';owner.id=8;
  assert.equal(e.request.target.id,4);assert.equal(e.request.source,'/a');assert.equal(e.owner.id,4);
  assert.ok(Object.isFrozen(e.request));assert.ok(Object.isFrozen(e.request.target));
});

test('typed owners, safe integers, UUIDs, normalized paths and label limits fail closed', () => {
  const e=entry();
  for(const operationId of ['x'.repeat(36),'00000000-0000-4000-8000-00000000000z','']) bad(state.createManualImportEntry(operationId,e.request,'ok',e.owner));
  for(const target of [{media_type:'movie',id:1.5},{media_type:'episode',id:0},{media_type:'episode',id:Number.MAX_SAFE_INTEGER+1},{media_type:'tv',id:1}]) bad(state.createManualImportEntry(id(1),{...e.request,target},'ok',null));
  for(const source of ['relative','/','/a//b','/a/../b','/a/./b','/a/','/a\u0000b','/'+ 'é'.repeat(2048),'/\ud800']) bad(state.createManualImportEntry(id(1),{...e.request,source},'ok',null));
  for(const label of ['x'.repeat(257),'line\nbreak']) bad(state.createManualImportEntry(id(1),e.request,label,null));
  bad(state.createManualImportEntry(id(1),e.request,'ok',{domain:'movies',id:1}));
  bad(state.createManualImportEntry(id(1),{...e.request,target:{media_type:'movie',id:1}},'ok',{domain:'movies',id:2}));
  ok(state.createManualImportEntry(id(1),e.request,'🙂'.repeat(256),null));
});

test('collection bounds, duplicate operations and cross-owner command identities are enforced', () => {
  const rows=Array.from({length:100},(_,i)=>entry(i+1));
  ok(state.encodeManualImportReceipt(receipt(rows)));
  bad(state.encodeManualImportReceipt(receipt([...rows,entry(101)])));
  bad(state.encodeManualImportReceipt(receipt([entry(),entry()])));
  let a=ok(state.recordKnownCommand(entry(),command(entry()))), b=entry(2);
  b=ok(state.recordKnownCommand(b,command(b)));
  bad(state.encodeManualImportReceipt(receipt([a,b])));
  bad(state.decodeManualImportReceipt(' '.repeat(state.MANUAL_IMPORT_RECEIPT_BYTES+1)));
  // UTF-8 count, not JS character count, bounds parsing before JSON allocation.
  bad(state.decodeManualImportReceipt('é'.repeat(state.MANUAL_IMPORT_RECEIPT_BYTES/2+1)));
  bad(state.encodeManualImportReceipt(receipt([{...a,cancelUncertainIds:[id(12)]}])));
  bad(state.encodeManualImportReceipt(receipt([{...a,authorized:false}])));
});

test('selection rejects duplicates and any shared move, permits shared copy/hardlink and typed equal IDs', () => {
  const a=entry(), movie=entry(2,'movie');
  bad(state.validateManualImportSelection([]));bad(state.validateManualImportSelection([a,a]));
  bad(state.validateManualImportSelection([a,entry(2,'episode',{target:a.request.target})]));
  bad(state.validateManualImportSelection([a,entry(2,'episode',{destination:a.request.destination})]));
  for(const modes of [['move','copy'],['copy','move'],['hardlink','move'],['move','hardlink'],['move','move']]) {
    bad(state.validateManualImportSelection([entry(1,'episode',{mode:modes[0]}),entry(2,'movie',{source:a.request.source,mode:modes[1]})]));
  }
  ok(state.validateManualImportSelection([a,entry(2,'movie',{source:a.request.source,mode:'hardlink'})]));
  ok(state.validateManualImportSelection([a,entry(2,'movie',{target:{media_type:'movie',id:1}})]));
  ok(state.validateManualImportSelection([a,movie]));
});

test('fresh operation validation fences target/UUID and distinguishes absent legacy recovery projection', () => {
  const e=entry(), op=operation(e);
  assert.equal(ok(state.validateManualImportOperation(e,op)).recoveryProjectionKnown,true);
  const {path_failure_code,recovery_required,...legacy}=op;
  assert.equal(ok(state.validateManualImportOperation(e,legacy)).recoveryProjectionKnown,false);
  for(const change of [{id:id(2)},{target:{media_type:'movie',id:1}},{status:'healthy'},{error_code:undefined},{path_failure_code:'unknown'},{path_failure_code:'retirement_missing_precommit'},{recovery_required:undefined},{status:'complete',recovery_required:true}]) bad(state.validateManualImportOperation(e,{...op,...change}));
  ok(state.validateManualImportOperation(e,{...op,status:'staging',path_failure_code:'unresolved_attempt',recovery_required:false}));
  ok(state.validateManualImportOperation(e,{...op,status:'staging',path_failure_code:'source_missing_precommit',recovery_required:true}));
});

test('reconciliation validates exact ordered membership, complete nulls, active command and typed target', () => {
  const a=entry(),b=entry(2,'movie');
  const rows=[{operation_id:a.operationId,operation:operation(a),active_command:command(a)},{operation_id:b.operationId,operation:null,active_command:null}];
  ok(state.validateManualImportReconciliation([a,b],rows));
  for(const changed of [[],[rows[0]],rows.toReversed(),[rows[0],rows[0]],[rows[0],{...rows[1],active_command:command(b)}],[{...rows[0],active_command:{...command(a),status:'failed'}},rows[1]],[{...rows[0],operation:operation(b)},rows[1]],[{...rows[0],active_command:undefined},rows[1]]]) bad(state.validateManualImportReconciliation([a,b],changed));
  const known=ok(state.recordKnownCommand(a,command(a)));
  bad(state.validateManualImportReconciliation([known],[{...rows[0],active_command:{...command(a),batch_id:id(901)}}]));
});

test('admission is one exact ordered batch with distinct commands and safe numeric fields', () => {
  const a=entry(),b=entry(2),rows=[command(a),command(b,12)];
  ok(state.validateManualImportAdmission([a,b],rows,'normal'));
  bad(state.validateManualImportAdmission([a,b],rows,'high'));
  bad(state.validateManualImportAdmission([a,b],[{...rows[0],status:'running'},rows[1]],'normal'));
  for(const changed of [[rows[0]],rows.toReversed(),[rows[0],{...rows[1],id:rows[0].id}],[rows[0],{...rows[1],batch_id:id(901)}],[{...rows[0],created_at:Number.MAX_SAFE_INTEGER+1},rows[1]],[{...rows[0],attempts:-1},rows[1]]]) bad(state.validateManualImportAdmission([a,b],changed,'normal'));
});

test('legacy authorized clean preview migrates as unverified identity, with no inferred series owner', () => {
  const e=entry();
  for(const authorized of [false,true]) {
    const migrated=ok(state.migrateLegacyManualImportReceipt(JSON.stringify({request:e.request,label:e.label,operation:operation(e),authorized})));
    assert.equal(migrated.entries[0].authorized,authorized);assert.equal(migrated.entries[0].executionUncertain,authorized);assert.equal(migrated.entries[0].owner,null);
    assert.equal('operation' in migrated.entries[0],false);assert.equal('fresh' in migrated.entries[0],false);
    assert.equal(migrated.entries[0].admissionUncertain,false);
  }
  bad(state.migrateLegacyManualImportReceipt(JSON.stringify({request:e.request,label:e.label,operation:operation(entry(2)),authorized:true})));
});

test('command-specific cancellation never resolves an outstanding admission or another cancellation', () => {
  let e=state.markExecutionUncertain(state.markAdmissionUncertain(entry()));
  e=ok(state.recordKnownCommand(e,command(e)));e=ok(state.recordKnownCommand(e,command(e,12)));
  e=ok(state.markCancelUncertain(e,id(11)));e=ok(state.markCancelUncertain(e,id(12)));
  bad(state.resolveCancelUncertain(e,id(11),command(e)));
  const next=ok(state.resolveCancelUncertain(e,id(11),{...command(e),status:'cancelled',completed_at:12}));
  assert.equal(next.admissionUncertain,true);assert.equal(next.executionUncertain,true);assert.equal(next.authorized,true);assert.deepEqual(next.cancelUncertainIds,[id(12)]);
  bad(state.markCancelUncertain(e,id(13)));
  bad(state.recordKnownCommand(e,{...command(e),batch_id:id(999)}));
  assert.deepEqual(e.cancelUncertainIds,[id(11),id(12)]);
});


test('saved command bounds never truncate history and UTF-8 serialization counts escaped bytes', () => {
  let e=entry();
  for(let n=0;n<100;n++) e=ok(state.recordKnownCommand(e,command(e,100+n)));
  assert.equal(e.commands.length,100);
  bad(state.recordKnownCommand(e,command(e,200)));
  ok(state.recordKnownCommand(e,command(e,100)));
  const oversized=[];
  for(let n=1;n<=100;n++) {
    const base=entry(n,'episode',{source:'/'+ '"'.repeat(4000)+n,destination:'/'+ '\\'.repeat(4000)+n});
    let row=base;
    for(let c=0;c<100;c++) row=ok(state.recordKnownCommand(row,command(row,10000+n*100+c)));
    oversized.push(row);
  }
  // Every individual field and row is valid, but JSON escaping pushes the whole receipt over 2 MiB.
  bad(state.encodeManualImportReceipt(receipt(oversized)));
});


test('enum validators reject JSON arrays and objects without string coercion', () => {
  const e=entry(), op=operation(e), c=command(e);
  for(const media_type of [['episode'],{toString:()=>'episode'}]) bad(state.createManualImportEntry(id(1),{...e.request,target:{media_type,id:1}},'ok',null));
  for(const mode of [['copy'],{toString:()=>'copy'}]) bad(state.createManualImportEntry(id(1),{...e.request,mode},'ok',null));
  for(const status of [['preview'],{toString:()=>'preview'}]) bad(state.validateManualImportOperation(e,{...op,status}));
  for(const change of [{status:['queued']},{priority:['normal']},{status:{toString:()=>'queued'}},{priority:{toString:()=>'normal'}}]) {
    bad(state.validateManualImportCommand(e,c.id,{...c,...change}));
    bad(state.validateManualImportAdmission([e],[{...c,...change}],'normal'));
  }
  bad(state.validateManualImportAdmission([e],[c],['normal']));
});

test('command state coherence matches migration0030 including zero-attempt failed and cancelled', () => {
  const e=entry(), c=command(e);
  const rows=[
    c,
    {...c,status:'running',attempts:1,started_at:11},
    {...c,status:'retry_wait',attempts:2,started_at:11,error_code:'import_busy'},
    {...c,status:'succeeded',attempts:3,started_at:11,completed_at:12},
    {...c,status:'failed',completed_at:12,error_code:'interrupted'},
    {...c,status:'cancelled',completed_at:12},
  ];
  for(const row of rows) ok(state.validateManualImportCommand(e,c.id,row));
  const invalid=[
    {...c,attempts:4}, {...c,attempts:1}, {...c,started_at:11}, {...c,completed_at:12}, {...c,error_code:'unexpected'},
    {...rows[1],attempts:0}, {...rows[1],started_at:null}, {...rows[1],completed_at:12}, {...rows[1],error_code:'unexpected'},
    {...rows[2],attempts:3}, {...rows[2],error_code:null}, {...rows[2],started_at:null},
    {...rows[3],completed_at:null}, {...rows[3],attempts:0}, {...rows[3],started_at:null}, {...rows[3],error_code:'unexpected'},
    {...rows[4],completed_at:null}, {...rows[4],error_code:null}, {...rows[5],error_code:'unexpected'}, {...rows[5],completed_at:null},
  ];
  for(const row of invalid) bad(state.validateManualImportCommand(e,c.id,row));
});

test('normalized newline and tab filenames survive capture and legacy migration without normalization', () => {
  const e=entry(1,'episode',{source:'/incoming/line\nname\t.mkv',destination:'/library/tab\tand\nnewline.mkv'});
  const legacy={request:e.request,label:e.label,operation:operation(e),authorized:true};
  const migrated=ok(state.migrateLegacyManualImportReceipt(JSON.stringify(legacy)));
  assert.deepEqual(migrated.entries[0].request,e.request);
  assert.deepEqual(ok(state.decodeManualImportReceipt(ok(state.encodeManualImportReceipt(migrated)))),migrated);
  bad(state.createManualImportEntry(id(1),{...e.request,source:'/incoming/nul\0name'},'ok',null));
});


test('execution uncertainty requires authorization and cannot be omitted from v2 receipts', () => {
  const e=entry(), marked=state.markExecutionUncertain(e);
  assert.equal(marked.authorized,true);assert.equal(marked.executionUncertain,true);assert.equal(e.executionUncertain,false);
  assert.equal(ok(state.decodeManualImportReceipt(ok(state.encodeManualImportReceipt(receipt([marked]))))).entries[0].executionUncertain,true);
  bad(state.encodeManualImportReceipt(receipt([{...marked,authorized:false}])));
  const {executionUncertain,...missing}=e;
  bad(state.decodeManualImportReceipt(JSON.stringify(receipt([missing]))));
});
