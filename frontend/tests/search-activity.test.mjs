import test from 'node:test';
import assert from 'node:assert/strict';
import {SEARCH_ACTIVITY_ACTIVE_POLL_MS,SEARCH_ACTIVITY_IDLE_POLL_MS,SEARCH_STATUSES,isActiveSearch,nextPollMs,outcomeLabel,searchActivityQuery,statusSummary,targetLabel} from '../src/lib/search-activity.ts';

test('query lists every target with a bounded page and an optional known status',()=>{
  assert.equal(searchActivityQuery('',0),'limit=25&offset=0');
  assert.equal(searchActivityQuery('running',50),'limit=25&offset=50&status=running');
  // Never sends a target filter: the API requires target_type and target_id together.
  assert.doesNotMatch(searchActivityQuery('queued',0),/target_/);
  for(const bad of [-1,1.5,Number.NaN,2**32])assert.equal(searchActivityQuery('',bad),'limit=25&offset=0');
  assert.equal(searchActivityQuery('bogus',0),'limit=25&offset=0');
  assert.deepEqual([...SEARCH_STATUSES],['queued','running','retry_wait','succeeded','failed','cancelled']);
});
test('poll cadence is faster only while a listed search is still active',()=>{
  assert.equal(nextPollMs([]),SEARCH_ACTIVITY_IDLE_POLL_MS);
  assert.equal(nextPollMs([{status:'failed'},{status:'succeeded'},{status:'cancelled'}]),SEARCH_ACTIVITY_IDLE_POLL_MS);
  for(const status of ['queued','running','retry_wait']){
    assert.equal(isActiveSearch({status}),true);
    assert.equal(nextPollMs([{status:'failed'},{status}]),SEARCH_ACTIVITY_ACTIVE_POLL_MS);
  }
  assert.ok(SEARCH_ACTIVITY_ACTIVE_POLL_MS<SEARCH_ACTIVITY_IDLE_POLL_MS);
});
test('targets with equal numeric IDs stay distinguishable',()=>{
  assert.equal(targetLabel({media_type:'episode',id:1}),'Episode 1');
  assert.equal(targetLabel({media_type:'movie',id:1}),'Movie 1');
});
test('outcome keeps the stable code beside readable text',()=>{
  assert.equal(outcomeLabel({error_code:'no_eligible_release',selected_candidate_status:null}),'No eligible release found (no_eligible_release)');
  assert.equal(outcomeLabel({error_code:'refresh_timeout',selected_candidate_status:null}),'refresh_timeout');
  assert.equal(outcomeLabel({error_code:null,selected_candidate_status:'submitted'}),'Selected release: submitted');
  assert.equal(outcomeLabel({error_code:null,selected_candidate_status:null}),'No recorded error');
});
test('live summary counts statuses in lifecycle order and discloses paging',()=>{
  assert.equal(statusSummary(0,[]),'No search commands.');
  assert.equal(statusSummary(1,[{status:'queued'}]),'1 search command: 1 queued.');
  assert.equal(statusSummary(3,[{status:'failed'},{status:'queued'},{status:'running'}]),'3 search commands: 1 queued, 1 running, 1 failed.');
  assert.equal(statusSummary(30,[{status:'retry_wait'}]),'30 search commands (1 on this page): 1 waiting to retry.');
});
