import test from 'node:test';
import assert from 'node:assert/strict';
import {defaultOptions,sortChoices,wantedQuery,episodeLabel,formatDate,formatDateTime,lastSearchLabel,searchBody,selectedIds,receiptSummary,retainVisible,cutoffLabel,monitoredLabel,availabilityLabel} from '../src/lib/wanted-view.ts';

const episode={target:{media_type:'episode',id:7},series_id:1,series_title:'Harbor',series_monitored:true,series_type:'standard',season:1,season_monitored:true,number:2,absolute_episode_number:14,scene_season_number:null,scene_episode_number:null,scene_absolute_episode_number:null,title:'Pilot',monitored:true,effective_monitored:true,air_date:null,air_date_utc:'2020-01-01T00:00:00Z',aired_end_at:1577838600,last_search_time:null,last_search_source:null,profile_id:1,file:null,cutoff:null};

test('defaults differ by domain and match the server',()=>{
  assert.deepEqual(defaultOptions('tv'),{limit:50,offset:0,monitored:'monitored',sort:'air_date',direction:'asc',availability:'all'});
  assert.equal(defaultOptions('movies').sort,'title');
});

test('sort choices only include server-supported keys',()=>{
  assert.deepEqual(sortChoices('tv','missing').map(c=>c.value),['air_date','series_title','last_search']);
  assert.deepEqual(sortChoices('movies','cutoff').map(c=>c.value),['title','year','in_cinemas','digital_release','physical_release','last_search','quality']);
  for(const domain of ['tv','movies'])for(const kind of ['missing','cutoff'])assert.ok(sortChoices(domain,kind).some(c=>c.value==='last_search'&&c.label==='Last searched'));
  assert.ok(sortChoices('tv','cutoff').some(c=>c.value==='quality'));
  assert.ok(!sortChoices('movies','missing').some(c=>c.value==='quality'||c.value==='air_date'));
});

test('query strings never send inapplicable keys and clamp bounds',()=>{
  assert.equal(wantedQuery('tv','missing',{...defaultOptions('tv'),availability:'available'}),'limit=50&offset=0&monitored=monitored&sort=air_date&direction=asc');
  assert.equal(wantedQuery('movies','missing',{...defaultOptions('movies'),availability:'unavailable'}),'limit=50&offset=0&monitored=monitored&sort=title&direction=asc&availability=unavailable');
  assert.equal(new URLSearchParams(wantedQuery('movies','cutoff',{...defaultOptions('movies'),availability:'available'})).has('availability'),false);
  const q=new URLSearchParams(wantedQuery('tv','missing',{...defaultOptions('tv'),limit:1000,offset:-5,sort:'quality'}));
  assert.deepEqual([q.get('limit'),q.get('offset'),q.get('sort')],['100','0','air_date']);
});

test('episode labels preserve specials, anime absolute and scene numbering',()=>{
  assert.equal(episodeLabel(episode),'S01E02');
  assert.equal(episodeLabel({...episode,season:0,number:3}),'Special 3');
  assert.equal(episodeLabel({...episode,series_type:'anime'}),'S01E02 · absolute 14');
  assert.equal(episodeLabel({...episode,scene_season_number:2,scene_episode_number:5}),'S01E02 · scene S02E05');
});

test('dates render as UTC days and unknown stays unknown',()=>{
  assert.equal(formatDate('2020-01-01T23:30:00Z'),'2020-01-01');
  assert.equal(formatDate('2020-01-01 23:30:00'),'2020-01-01');
  assert.equal(formatDate(null),'Unknown');
  assert.equal(formatDate('garbage'),'Unknown');
});

test('labels explain monitoring, cutoff and availability facts',()=>{
  assert.equal(monitoredLabel({...episode,effective_monitored:false,series_monitored:false}),'Series unmonitored');
  assert.equal(monitoredLabel({...episode,effective_monitored:false,monitored:false}),'Episode unmonitored');
  assert.equal(cutoffLabel({...episode,file:{id:1,path:'/x',quality_id:4,quality_name:'HDTV-720p'},cutoff:{profile_id:1,quality_rank:1,threshold_rank:2,basis:'first_allowed'}}),'HDTV-720p (rank 1 below 2, upgrades disabled)');
  assert.equal(availabilityLabel({availability:'unavailable',minimum_availability:'released'}),'Not yet available (released)');
  assert.equal(availabilityLabel({availability:'ungated',minimum_availability:null}),'No availability policy');
});

test('search bodies and selections are canonical',()=>{
  assert.deepEqual(selectedIds(new Set([9,2,-1,2.5,4])),[2,4,9]);
  const body=searchBody('r',"missing",{kind:'ids',ids:[1]},{id:'i',revision:3},{kind:'default'});
  assert.deepEqual(body,{request_id:'r',kind:'missing',selection:{kind:'ids',ids:[1]},indexer_id:'i',indexer_revision:3,client_selection:{kind:'default'},priority:'normal'});
  assert.deepEqual([...retainVisible(new Set([1,2,3]),[2,3,4])],[2,3]);
});

test('receipt summaries state counts and replay honestly',()=>{
  assert.equal(receiptSummary({request_id:'r',media_type:'tv',kind:'missing',replayed:false,matched:3,skipped_active:1,admitted:2,commands:[]}),'Queued 2 episodes for automatic search; 3 matched; 1 skipped because a search is already active.');
  assert.equal(receiptSummary({request_id:'r',media_type:'movies',kind:'cutoff',replayed:true,matched:null,skipped_active:null,admitted:1,commands:[]}),'Earlier identical request read back: 1 movie already queued for search.');
});

test('last search labels distinguish never, native and imported facts in UTC',()=>{
  assert.equal(lastSearchLabel(episode),'Never searched');
  assert.equal(lastSearchLabel({...episode,last_search_time:'2023-11-14T22:13:20Z',last_search_source:'native'}),'2023-11-14 22:13 UTC');
  assert.equal(lastSearchLabel({...episode,last_search_time:'2024-01-01T00:00:00Z',last_search_source:'imported'}),'2024-01-01 00:00 UTC (imported)');
  assert.equal(lastSearchLabel({...episode,last_search_time:'garbage',last_search_source:'native'}),'Never searched');
  assert.equal(formatDateTime(null),null);
  assert.equal(new URLSearchParams(wantedQuery('movies','cutoff',{...defaultOptions('movies'),sort:'last_search',direction:'desc'})).get('sort'),'last_search');
});
