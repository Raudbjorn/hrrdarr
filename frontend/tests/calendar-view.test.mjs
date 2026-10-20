import test from 'node:test';
import assert from 'node:assert/strict';
import {addDays,addMonths,daysBetween,startOfWeek,rangeFor,weeksOf,step,rangeTitle,dateLabel,localMidnight,tvQuery,movieQuery,feedPath,webcalUrl,episodeCode,tvStatus,movieStatus,tvItem,movieItem,groupByDate,moveDate,parseLocalDate,defaultOptions,localDateOf,inRange} from '../src/lib/calendar-view.ts';

// Each test pins its own zone; Node re-reads TZ when it changes.
function inZone(zone,fn){const old=process.env.TZ;process.env.TZ=zone;try{return fn();}finally{if(old===undefined)delete process.env.TZ;else process.env.TZ=old;}}
const episode={event_id:'episode-7',target:{media_type:'episode',id:7},series_id:1,series_title:'Harbor',series_type:'standard',series_monitored:true,network:null,season:1,season_monitored:true,number:2,absolute_episode_number:14,title:'Pilot',overview:null,finale_type:null,monitored:true,effective_monitored:true,air_date:'2026-10-31',starts_at:'2026-11-01T02:30:00Z',runtime_minutes:45,runtime_source:'series',ends_at:'2026-11-01T03:15:00Z',has_file:false};
const movie={event_id:'movie-7-digital',target:{media_type:'movie',id:7},metadata_id:3,title:'Harbor',year:2026,tmdb_id:1,imdb_id:null,release_type:'digital',date:'2026-11-01',released_at:'2026-11-01T00:00:00Z',monitored:true,has_file:false,status:null,studio:null,runtime_minutes:null};

test('strict local dates and UTC arithmetic across leap days and DST',()=>{
  assert.deepEqual(parseLocalDate('2024-02-29'),{year:2024,month:2,day:29});
  for(const bad of ['2025-02-29','2026-13-01','2026-1-01','0000-01-01','2026-10-01T00:00']) assert.equal(parseLocalDate(bad),null,bad);
  assert.equal(addDays('2024-02-28',1),'2024-02-29');
  assert.equal(addDays('2026-12-31',1),'2027-01-01');
  // Local DST days are 23/25 hours long; date arithmetic never skips or repeats a day.
  inZone('America/New_York',()=>{assert.equal(addDays('2026-11-01',1),'2026-11-02');assert.equal(addDays('2026-03-08',1),'2026-03-09');});
  assert.equal(daysBetween('2024-01-01','2025-01-01'),366);
  assert.equal(addMonths('2026-01-31',1),'2026-02-28');
  assert.equal(addMonths('2024-01-31',1),'2024-02-29');
  assert.equal(addMonths('2026-12-15',1),'2027-01-15');
  assert.equal(addMonths('2026-01-15',-1),'2025-12-15');
});

test('visible ranges complete Sunday-first weeks and match each view',()=>{
  assert.equal(startOfWeek('2026-10-14'),'2026-10-11');
  assert.equal(startOfWeek('2026-10-11'),'2026-10-11');
  // October 2026 starts on a Thursday and ends on a Saturday.
  assert.deepEqual(rangeFor('month','2026-10-14'),{first:'2026-09-27',end:'2026-11-01'});
  assert.deepEqual(rangeFor('week','2026-10-14'),{first:'2026-10-11',end:'2026-10-18'});
  assert.deepEqual(rangeFor('agenda','2026-10-14'),{first:'2026-10-01',end:'2026-11-01'});
  const weeks=weeksOf(rangeFor('month','2026-10-14'));
  assert.equal(weeks.length,5);assert.deepEqual(weeks.map(w=>w.length),[7,7,7,7,7]);
  assert.equal(weeks[0][0],'2026-09-27');assert.equal(weeks[4][6],'2026-10-31');
  // February 2026 runs Sunday 1st to Saturday 28th: exactly four weeks, no adjacent days.
  assert.equal(weeksOf(rangeFor('month','2026-02-10')).length,4);
  assert.deepEqual(rangeFor('month','2026-02-10'),{first:'2026-02-01',end:'2026-03-01'});
  assert.equal(step('week','2026-10-14',1),'2026-10-21');
  assert.equal(step('month','2026-10-31',-1),'2026-09-30');
  assert.equal(rangeTitle('month','2026-10-14'),'October 2026');
  assert.equal(rangeTitle('week','2026-12-30'),'Week of Dec 27 – Jan 2, 2027');
  assert.equal(dateLabel('2026-10-14'),'Wednesday, October 14, 2026');
  assert.ok(inRange('2026-10-31',{first:'2026-10-01',end:'2026-11-01'}));
  assert.ok(!inRange('2026-11-01',{first:'2026-10-01',end:'2026-11-01'}));
});

test('TV bounds are local midnights with the offset in force on that date',()=>{
  inZone('America/New_York',()=>{
    assert.equal(localMidnight('2026-10-01'),'2026-10-01T00:00:00-04:00');
    assert.equal(localMidnight('2026-11-02'),'2026-11-02T00:00:00-05:00');
  });
  inZone('Asia/Kolkata',()=>assert.equal(localMidnight('2026-10-01'),'2026-10-01T00:00:00+05:30'));
  inZone('America/St_Johns',()=>assert.equal(localMidnight('2026-01-01'),'2026-01-01T00:00:00-03:30'));
  inZone('UTC',()=>assert.equal(localMidnight('2026-10-01'),'2026-10-01T00:00:00+00:00'));
  inZone('Asia/Kolkata',()=>{
    const q=new URLSearchParams(tvQuery({first:'2026-10-01',end:'2026-10-08'},defaultOptions()));
    assert.deepEqual([...q.keys()],['start','end']);
    assert.equal(q.get('start'),'2026-10-01T00:00:00+05:30');
    // '+' is percent-encoded so the server never decodes it as a space.
    assert.ok(tvQuery({first:'2026-10-01',end:'2026-10-08'},defaultOptions()).includes('%2B05%3A30'));
  });
  const q=new URLSearchParams(tvQuery({first:'2026-10-01',end:'2026-10-08'},{...defaultOptions(),unmonitored:true,specials:false}));
  assert.equal(q.get('unmonitored'),'true');assert.equal(q.get('specials'),'false');
});

test('movie queries use floating dates and only send a release-type subset',()=>{
  assert.equal(movieQuery({first:'2026-10-01',end:'2026-11-01'},defaultOptions()),'start=2026-10-01&end=2026-11-01');
  assert.equal(new URLSearchParams(movieQuery({first:'2026-10-01',end:'2026-11-01'},{...defaultOptions(),releaseTypes:['physical','in_cinemas']})).get('release_types'),'in_cinemas,physical');
  assert.equal(movieQuery({first:'2026-10-01',end:'2026-11-01'},{...defaultOptions(),releaseTypes:[]}),null);
  assert.equal(new URLSearchParams(movieQuery({first:'2026-10-01',end:'2026-11-01'},{...defaultOptions(),unmonitored:true})).get('unmonitored'),'true');
});

test('feed links carry the chosen filters and only become webcal for http(s)',()=>{
  assert.equal(feedPath('tv',defaultOptions()),'/feed/v1/calendar/tv.ics?past_days=7&future_days=28');
  assert.equal(feedPath('tv',{...defaultOptions(),specials:false,unmonitored:true}),'/feed/v1/calendar/tv.ics?past_days=7&future_days=28&unmonitored=true&specials=false');
  assert.equal(feedPath('movies',{...defaultOptions(),releaseTypes:['digital']}),'/feed/v1/calendar/movies.ics?past_days=7&future_days=28&release_types=digital');
  assert.equal(feedPath('movies',{...defaultOptions(),releaseTypes:[]}),null);
  assert.ok(!feedPath('movies',{...defaultOptions(),specials:false}).includes('specials'));
  assert.equal(webcalUrl('http://127.0.0.1:8760/feed/v1/calendar/tv.ics'),'webcal://127.0.0.1:8760/feed/v1/calendar/tv.ics');
  assert.equal(webcalUrl('https://x.example/f.ics'),'webcal://x.example/f.ics');
  assert.equal(webcalUrl('javascript:alert(1)'),null);
});

test('TV entries land on the viewer-local day; movie dates never shift',()=>{
  inZone('America/New_York',()=>{
    const item=tvItem(episode,Date.parse('2026-10-01T00:00:00Z'));
    assert.deepEqual([item.date,item.time,item.key,item.status],['2026-10-31','22:30','tv:episode-7','unaired']);
    assert.equal(localDateOf(new Date('2026-11-01T04:30:00Z')),'2026-11-01');
    assert.equal(movieItem(movie,'2026-10-01').date,'2026-11-01');
  });
  inZone('Asia/Tokyo',()=>{
    assert.deepEqual([tvItem(episode,0).date,tvItem(episode,0).time],['2026-11-01','11:30']);
    assert.equal(movieItem(movie,'2026-10-01').date,'2026-11-01');
  });
});

test('status labels combine file, monitoring and time; never color alone',()=>{
  const at=Date.parse('2026-11-01T02:30:00Z');
  assert.equal(tvStatus(episode,at-1),'unaired');
  assert.equal(tvStatus(episode,at+60_000),'airing');
  assert.equal(tvStatus(episode,Date.parse('2026-11-01T03:15:00Z')),'missing');
  assert.equal(tvStatus({...episode,ends_at:null},at),'missing');
  assert.equal(tvStatus({...episode,has_file:true,effective_monitored:false},at),'downloaded');
  assert.equal(tvStatus({...episode,effective_monitored:false},at),'unmonitored');
  assert.equal(movieStatus(movie,'2026-10-31'),'upcoming');
  assert.equal(movieStatus(movie,'2026-11-01'),'missing');
  assert.equal(movieItem(movie,'2026-11-02').statusLabel,'Released, no file');
  assert.equal(movieStatus({...movie,monitored:false},'2026-12-01'),'unmonitored');
  assert.equal(movieStatus({...movie,has_file:true},'2026-10-01'),'downloaded');
});

test('episode codes preserve specials, daily and anime numbering',()=>{
  assert.equal(episodeCode(episode),'S01E02');
  assert.equal(episodeCode({...episode,season:0,number:3}),'Special 3');
  assert.equal(episodeCode({...episode,series_type:'daily'}),'2026-10-31');
  assert.equal(episodeCode({...episode,series_type:'anime'}),'S01E02 (14)');
});

test('grouping keeps three same-day releases distinct and all-day entries first',()=>{
  inZone('UTC',()=>{
    const items=[tvItem({...episode,starts_at:'2026-11-01T00:30:00Z',ends_at:null},0),...['physical','in_cinemas','digital'].map(t=>movieItem({...movie,event_id:`movie-7-${t}`,release_type:t},'2026-10-01')),tvItem({...episode,event_id:'episode-9',target:{media_type:'episode',id:9},starts_at:'2026-12-01T00:00:00Z'},0)];
    const groups=groupByDate(items,{first:'2026-11-01',end:'2026-12-01'});
    assert.deepEqual([...groups.keys()],['2026-11-01']);
    assert.deepEqual(groups.get('2026-11-01').map(i=>i.key),['movies:movie-7-in_cinemas','movies:movie-7-digital','movies:movie-7-physical','tv:episode-7']);
    // Equal numeric IDs in both domains keep distinct keys.
    assert.equal(new Set(items.map(i=>i.key)).size,items.length);
  });
});

test('keyboard movement follows the date-grid pattern',()=>{
  assert.equal(moveDate('2026-10-14','ArrowLeft'),'2026-10-13');
  assert.equal(moveDate('2026-10-31','ArrowRight'),'2026-11-01');
  assert.equal(moveDate('2026-10-14','ArrowUp'),'2026-10-07');
  assert.equal(moveDate('2026-10-14','ArrowDown'),'2026-10-21');
  assert.equal(moveDate('2026-10-14','Home'),'2026-10-11');
  assert.equal(moveDate('2026-10-14','End'),'2026-10-17');
  assert.equal(moveDate('2026-03-31','PageUp'),'2026-02-28');
  assert.equal(moveDate('2026-10-14','PageDown'),'2026-11-14');
  assert.equal(moveDate('2026-10-14','Enter'),null);
});
