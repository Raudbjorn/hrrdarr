import test from 'node:test';
import assert from 'node:assert/strict';
import {hasRecoveryState,manualImportAction,canResumeProcessing,pathDiagnostic,processingDiagnostic} from '../src/lib/pathDiagnostics.ts';
const operation={status:'preview',error_code:null,path_failure_code:null,recovery_required:false};
const processing={status:'importing',import_phase:'staging',operation_id:'operation',resume_requested:false,path_failure_code:'unresolved_attempt',recovery_required:false,reasons:['hc021_unresolved_attempt']};

test('saved receipts require fresh complete recovery details before another authorization',()=>{
  assert.equal(hasRecoveryState({status:'preview',error_code:null}),false);
  assert.equal(manualImportAction(operation,false),null);
  assert.equal(manualImportAction({...operation,recovery_required:undefined},true),null);
  assert.equal(manualImportAction({...operation,path_failure_code:undefined},true),null);
  assert.equal(manualImportAction(operation,true),'execute');
  // An execute response can be lost before admission; retry the same untouched preview.
  assert.equal(manualImportAction({...operation,authorized:true},true),'execute');
  assert.equal(manualImportAction({...operation,status:'staging'},true),null);
  assert.equal(manualImportAction({...operation,status:'staging',path_failure_code:'unresolved_attempt'},true),null);
  assert.equal(manualImportAction({...operation,status:'staging',path_failure_code:'unresolved_attempt',recovery_required:true},true),'resume');
  assert.equal(manualImportAction({...operation,status:'complete',recovery_required:true},true),null);
});

test('linked receipt recovery uses backend lease projection and preserves pending request protection',()=>{
  assert.equal(canResumeProcessing(processing),false);
  const abandoned={...processing,recovery_required:true,error_code:null};
  assert.equal(canResumeProcessing(abandoned),true);
  for(const change of [{resume_requested:true},{operation_id:null},{status:'queued'},{status:'imported'},{import_phase:'complete'},{recovery_required:undefined}]) assert.equal(canResumeProcessing({...abandoned,...change}),false);
});

test('retained journal and receipt diagnoses render independently of control errors',()=>{
  assert.match(processingDiagnostic({...processing,path_failure_code:'source_missing_precommit',error_code:null}),/approved source.*missing/i);
  assert.match(processingDiagnostic({...processing,path_failure_code:null,reasons:['hc021_mapping_changed'],error_code:null}),/mapping/i);
  assert.match(processingDiagnostic({...processing,recovery_required:true}),/previous import attempt.*no confirmed outcome/i);
  const active=processingDiagnostic(processing);
  assert.match(active,/unresolved evidence/i);
  assert.doesNotMatch(active,/is running|still running|explicitly resume/i);
  assert.equal(processingDiagnostic({...processing,status:'imported'}),null);
  assert.equal(processingDiagnostic({...processing,import_phase:'complete'}),null);
  assert.equal(pathDiagnostic('source_denied_precommit',true,true),null);
});

test('all finite causal codes describe their role without inventing permission or source removal',()=>{
  for(const role of ['source','root','destination','stage','retirement']) {
    for(const kind of ['missing','denied','changed','invalid','io']) {
      for(const boundary of role==='retirement'?['cleanup']:['precommit','cleanup']) {
        const text=pathDiagnostic(`${role}_${kind}_${boundary}`,true);
        assert.doesNotMatch(text,/unrecognized/);
        if(kind!=='denied') assert.doesNotMatch(text,/denied access/);
        if(boundary==='cleanup') assert.match(text,/not evidence of source loss before import/);
      }
    }
  }
  for(const role of ['source','root','destination']) for(const kind of ['missing','denied','changed','invalid','io']) assert.doesNotMatch(pathDiagnostic(`hc021_${role}_${kind}_prepare`,false),/unrecognized|approved source.*missing/i);
  assert.match(pathDiagnostic('source_missing_precommit',true),/approved source.*missing/i);
});

test('unknown or misplaced marker content never echoes paths, endpoints or supplied text',()=>{
  const privateText='PRIVATE_PATH_ENDPOINT_TOKEN';
  for(const code of [`/downloads/${privateText}`,`hc021_${privateText}`,`source_${privateText}_precommit`,'retirement_missing_precommit','hc021_stage_missing_prepare','constructor_missing_precommit','source_missing_precommit_extra',{}]) {
    const text=pathDiagnostic(code,true);
    assert.match(text,/unrecognized diagnostic/);
    assert.ok(!text.includes(privateText));
  }
  for(const reasons of [['ordinary','hc021_mapping_missing'],['hc021_mapping_missing','hc021_mapping_changed']]) assert.match(processingDiagnostic({...processing,path_failure_code:null,reasons}),/unrecognized diagnostic/);
  assert.equal(processingDiagnostic({...processing,path_failure_code:null,reasons:[]}),null);
});
