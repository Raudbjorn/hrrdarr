// Owned library_ui_fixture only: real HTTP, no installed services. Calendar is read-only, so
// the command worker is stopped first and no provider exists. The browser runs in a fixed
// non-UTC zone so TV air times must move to the viewer's local day while all-day movie release
// dates must not; every visible claim is read back through the production calendar/feed APIs.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,fixture=JSON.parse(process.env.UI_FIXTURE_JSON||'{}');
assert.equal(new URL(origin).hostname,'127.0.0.1');
assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration110(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration110 artifact directory');
await mkdir(artifacts,{recursive:true});
const ZONE='America/New_York';
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const context=await browser.newContext({timezoneId:ZONE,locale:'en-US',viewport:{width:1280,height:900}});
const page=await context.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
async function api(method,path,data,status=200){
 const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
 assert.equal(response.status(),status,`${method} ${path}: ${await response.text()}`);return status===204?null:response.json();
}
async function until(read,accept,label,timeoutMs=20000){
 const deadline=Date.now()+timeoutMs;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
const MONTHS=['January','February','March','April','May','June','July','August','September','October','November','December'];
const WEEKDAYS=['Sunday','Monday','Tuesday','Wednesday','Thursday','Friday','Saturday'];
const monthName=d=>`${MONTHS[Number(d.slice(5,7))-1]} ${Number(d.slice(0,4))}`;
const dateLabel=d=>{const w=new Date(`${d}T00:00:00Z`).getUTCDay();return `${WEEKDAYS[w]}, ${MONTHS[Number(d.slice(5,7))-1]} ${Number(d.slice(8))}, ${Number(d.slice(0,4))}`;};
const addDays=(d,n)=>new Date(Date.parse(`${d}T00:00:00Z`)+n*86400000).toISOString().slice(0,10);
const localDate=iso=>new Intl.DateTimeFormat('en-CA',{timeZone:ZONE,year:'numeric',month:'2-digit',day:'2-digit'}).format(new Date(iso));
const localTime=iso=>new Intl.DateTimeFormat('en-GB',{timeZone:ZONE,hour:'2-digit',minute:'2-digit',hourCycle:'h23'}).format(new Date(iso));
const cal=page.getByRole('region',{name:'Calendar',exact:true});
const grid=()=>cal.getByRole('grid');
const busy=async()=>{await until(async()=>(await cal.getByRole('status').filter({hasText:'Loading calendar'}).count()),n=>n===0,'Calendar stayed loading');};
const dayButton=d=>grid().locator(`button[data-date="${d}"]`);
const cellText=async d=>{await busy();return dayButton(d).locator('xpath=..').innerText();};
async function showMonthOf(date){
 const want=monthName(date);
 for(let i=0;i<24;i++){
  await busy();const title=(await cal.locator('#calendar-range').innerText()).trim();
  if(title===want)return;
  const [name,year]=title.split(' '),month=MONTHS.indexOf(name)+1;
  assert.ok(month>0&&Number(year)>0,`Unexpected month title ${title}`);
  const forward=Number(date.slice(0,4))*12+Number(date.slice(5,7))>Number(year)*12+month;
  await cal.getByRole('button',{name:forward?'Next month':'Previous month',exact:true}).click();
 }
 assert.fail(`Could not navigate to ${want}`);
}
async function feedText(path){
 const response=await page.request.get(origin+path,{timeout:10000});
 assert.equal(response.status(),200,`${path}: ${await response.text()}`);
 assert.equal(response.headers()['content-type'],'text/calendar; charset=utf-8');
 const text=await response.text();
 assert.ok(text.startsWith('BEGIN:VCALENDAR\r\n')&&text.endsWith('END:VCALENDAR\r\n'));
 for(const line of text.split('\r\n'))assert.ok(Buffer.byteLength(line)<=75,line);
 return text.replace(/\r\n[ \t]/g,'');
}
const uids=text=>[...text.matchAll(/^UID:(.+)$/gm)].map(m=>m[1]);
try{
 await api('post','/api/fixture/stop-command-worker',undefined,204);
 const seed=await api('post','/api/fixture/calendar-seed');
 const {today,dates,episodes,movies}=seed;
 assert.equal(episodes.pilot,movies.triple,'Fixture keeps colliding numeric episode/movie IDs');

 // API readback first: typed targets and three separate movie release entries.
 const tvApi=await api('get',`/api/v1/tv/calendar?start=${today}&end=${addDays(today,7)}`);
 assert.deepEqual(tvApi.items.map(i=>i.target),[{media_type:'episode',id:episodes.pilot},{media_type:'episode',id:episodes.second}]);
 assert.equal(tvApi.diagnostics.date_only_air_dates,1);
 const pilotAt=tvApi.items[0].starts_at;
 assert.equal(pilotAt,`${dates.d1}T02:30:00Z`);
 const movieApi=await api('get',`/api/v1/movies/calendar?start=${today}&end=${addDays(today,7)}`);
 assert.deepEqual(movieApi.items.map(i=>[i.target.media_type,i.target.id,i.release_type,i.date]),[['movie',movies.triple,'in_cinemas',today],['movie',movies.triple,'digital',today],['movie',movies.triple,'physical',dates.d5]]);

 await page.goto(`${origin}/#/calendar`);
 await cal.getByRole('heading',{name:'Calendar',exact:true}).waitFor();
 assert.equal(await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Calendar',exact:true}).getAttribute('aria-pressed'),'true');
 assert.equal(await cal.getByRole('navigation',{name:'Calendar media'}).getByRole('button',{name:'Both',exact:true}).getAttribute('aria-pressed'),'true');
 await cal.getByRole('region',{name:'Legend'}).getByText('Movie: Digital release (all day)',{exact:true}).waitFor();

 // The pilot airs 02:30 UTC the next day: local (New York) evening of the seed day.
 const pilotDay=localDate(pilotAt);
 assert.equal(pilotDay,today,'America/New_York is behind UTC by 4-5 hours');
 await showMonthOf(pilotDay);
 let text=await cellText(pilotDay);
 assert.ok(text.includes(localTime(pilotAt))&&text.includes('Calendar Harbor')&&text.includes('S01E01 · Calendar pilot'),text);
 assert.ok(text.includes('Not aired yet'),text);
 // Two movie releases on the same date are two labelled entries; dates are not zone-shifted.
 assert.ok(text.includes('Calendar Movie (2026)')&&text.includes('In cinemas')&&text.includes('Digital release'),text);
 assert.equal((text.match(/Calendar Movie \(2026\)/g)||[]).length,2,text);
 assert.ok(!text.includes('Calendar hidden')&&!text.includes('Calendar Unmonitored Movie'),'Unmonitored entries are hidden by default');
 assert.match(await dayButton(pilotDay).getAttribute('aria-label'),new RegExp(`^${dateLabel(pilotDay)}.*: 3 entries$`));
 if(artifacts)await page.screenshot({path:join(artifacts,'calendar-month-desktop.png'),fullPage:true});

 // Keyboard: roving focus across days, Enter opens details with status text.
 await dayButton(pilotDay).focus();
 await page.keyboard.press('ArrowRight');
 await until(()=>page.evaluate(()=>document.activeElement?.getAttribute('data-date')),d=>d===dates.d1,'ArrowRight focus');
 await page.keyboard.press('ArrowRight');
 await until(()=>page.evaluate(()=>document.activeElement?.getAttribute('data-date')),d=>d===dates.d2,'Second ArrowRight focus');
 await busy();
 await page.keyboard.press('Enter');
 const details=cal.getByRole('region',{name:dateLabel(dates.d2),exact:true});
 await details.waitFor();
 const second=await details.getByRole('listitem').allInnerTexts();
 assert.equal(second.length,1,second.join('\n'));
 assert.ok(second[0].includes('Calendar second')&&second[0].includes('Downloaded')&&second[0].includes(localTime(`${dates.d2}T20:00:00Z`)),second[0]);
 await details.getByRole('button',{name:'Close day details',exact:true}).click();
 await until(()=>page.evaluate(()=>document.activeElement?.getAttribute('data-date')),d=>d===dates.d2,'Focus returns to the day');
 await page.keyboard.press('ArrowLeft');await page.keyboard.press('ArrowLeft');
 await until(()=>page.evaluate(()=>document.activeElement?.getAttribute('data-date')),d=>d===pilotDay,'ArrowLeft focus');

 // Date-only episodes are reported, never placed at an invented time.
 await showMonthOf(dates.d3);
 await cal.getByRole('note').filter({hasText:'1 episode has an air date in this range but no known air time'}).waitFor();
 assert.ok(!(await cellText(dates.d3)).includes('Calendar date only'));

 // Unmonitored entries appear only when requested, labelled as such.
 await showMonthOf(dates.d1);
 await cal.getByRole('checkbox',{name:'Include unmonitored'}).check();
 await until(()=>cellText(localDate(`${dates.d1}T12:00:00Z`)),t=>t.includes('Calendar hidden')&&t.includes('Unmonitored'),'Unmonitored episode');
 await until(()=>cellText(dates.d1),t=>t.includes('Calendar Unmonitored Movie (2026)'),'Unmonitored movie');
 const hidden=await api('get',`/api/v1/tv/calendar?start=${today}&end=${addDays(today,7)}&unmonitored=true`);
 assert.ok(hidden.items.some(i=>i.target.id===episodes.hidden&&i.effective_monitored===false));
 await cal.getByRole('checkbox',{name:'Include unmonitored'}).uncheck();
 await until(()=>cellText(dates.d1),t=>!t.includes('Calendar Unmonitored Movie'),'Unmonitored hidden again');

 // Release-type filter is server-side and keeps the other type of the same movie.
 await showMonthOf(today);
 await cal.getByRole('group',{name:'Movie release types'}).getByRole('checkbox',{name:'In cinemas'}).uncheck();
 await until(()=>cellText(today),t=>!t.includes('In cinemas')&&t.includes('Digital release'),'In cinemas filtered');
 const movieFeedLink=await cal.getByLabel('Movies feed link',{exact:true}).inputValue();
 assert.equal(new URL(movieFeedLink).pathname,'/feed/v1/calendar/movies.ics');
 assert.equal(new URL(movieFeedLink).searchParams.get('release_types'),'digital,physical');
 const movieFeed=await feedText(new URL(movieFeedLink).pathname+new URL(movieFeedLink).search);
 assert.deepEqual(uids(movieFeed),[`movie-${movies.triple}-digital@hrrdarr`,`movie-${movies.triple}-physical@hrrdarr`]);
 assert.ok(movieFeed.includes(`DTSTART;VALUE=DATE:${today.replaceAll('-','')}`));
 assert.ok(movieFeed.includes('CATEGORIES:Harbor Studio'));
 await cal.getByRole('group',{name:'Movie release types'}).getByRole('checkbox',{name:'In cinemas'}).check();
 await until(()=>cellText(today),t=>t.includes('In cinemas'),'In cinemas restored');

 // TV feed link follows the UI filters; the same typed numeric ID never crosses domains.
 const tvFeedLink=await cal.getByLabel('TV feed link',{exact:true}).inputValue();
 const tvFeed=await feedText(new URL(tvFeedLink).pathname+new URL(tvFeedLink).search);
 assert.deepEqual(uids(tvFeed),[`episode-${episodes.pilot}@hrrdarr`,`episode-${episodes.second}@hrrdarr`]);
 assert.ok(tvFeed.includes(`DTSTART:${dates.d1.replaceAll('-','')}T023000Z`)&&tvFeed.includes('DURATION:PT45M'));
 assert.ok(tvFeed.includes('SUMMARY:Calendar Harbor - 1x01 - Calendar pilot'));
 assert.equal(await cal.getByRole('link',{name:'Subscribe to TV feed',exact:true}).getAttribute('href'),tvFeedLink.replace(/^http:/,'webcal:'));

 // Scope switch: each domain alone.
 await cal.getByRole('navigation',{name:'Calendar media'}).getByRole('button',{name:'TV',exact:true}).click();
 await until(()=>cellText(today),t=>t.includes('Calendar pilot')&&!t.includes('Calendar Movie'),'TV only');
 assert.equal(await cal.getByRole('group',{name:'Movie release types'}).count(),0);
 await cal.getByRole('navigation',{name:'Calendar media'}).getByRole('button',{name:'Movies',exact:true}).click();
 await until(()=>cellText(today),t=>!t.includes('Calendar pilot')&&t.includes('Calendar Movie'),'Movies only');
 assert.equal(await cal.getByRole('checkbox',{name:'Include specials'}).count(),0);
 await cal.getByRole('navigation',{name:'Calendar media'}).getByRole('button',{name:'Both',exact:true}).click();

 // Agenda lists each day with its entries; week view shows one week.
 await cal.getByRole('navigation',{name:'Calendar view'}).getByRole('button',{name:'Agenda',exact:true}).click();
 await busy();
 const agenda=cal.getByRole('region',{name:'Agenda',exact:true});
 const dayList=agenda.getByRole('list',{name:`Entries for ${dateLabel(today)}`,exact:true});
 await until(async()=>(await dayList.count())?(await dayList.getByRole('listitem').allInnerTexts()):[],rows=>rows.length===3,'Agenda day');
 const agendaRows=await dayList.getByRole('listitem').allInnerTexts();
 assert.ok(agendaRows[0].includes('All day')&&agendaRows[0].includes('In cinemas'),agendaRows[0]);
 assert.ok(agendaRows[1].includes('Digital release'),agendaRows[1]);
 assert.ok(agendaRows[2].includes('Calendar pilot'),agendaRows[2]);
 await busy();
 if(artifacts)await page.screenshot({path:join(artifacts,'calendar-agenda-desktop.png'),fullPage:true});
 await cal.getByRole('navigation',{name:'Calendar view'}).getByRole('button',{name:'Week',exact:true}).click();
 await busy();
 assert.match(await cal.locator('#calendar-range').innerText(),/^Week of /);
 assert.equal(await grid().getByRole('row').count(),2,'Header row plus one week');
 await cal.getByRole('button',{name:'Next week',exact:true}).click();await busy();
 await cal.getByRole('button',{name:'Today',exact:true}).click();await busy();

 // Error state: a failing movie read is an alert with retry, not an empty calendar.
 const failMovies=url=>url.pathname==='/api/v1/movies/calendar';
 await page.route(failMovies,route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'calendar_unavailable',message:'Calendar storage is unavailable'}})}));
 await cal.getByRole('button',{name:'Refresh calendar',exact:true}).click();
 await cal.getByRole('alert').filter({hasText:'Movie calendar could not be loaded: Calendar storage is unavailable'}).waitFor();
 await page.unroute(failMovies);
 await cal.getByRole('button',{name:'Retry loading calendar',exact:true}).click();
 await until(()=>cal.getByRole('alert').count(),n=>n===0,'Alert cleared after retry');

 // Strict server validation remains visible to clients.
 const invalid=await page.request.get(`${origin}/api/v1/tv/calendar?start=${today}`);
 assert.equal(invalid.status(),400);assert.equal((await invalid.json()).error.code,'invalid_calendar_query');

 await page.setViewportSize({width:390,height:844});
 await cal.getByRole('navigation',{name:'Calendar view'}).getByRole('button',{name:'Month',exact:true}).click();
 await busy();
 if(artifacts)await page.screenshot({path:join(artifacts,'calendar-month-mobile.png'),fullPage:true});
 assert.deepEqual(errors,[]);
 console.log(JSON.stringify({calendar_browser:'passed',zone:ZONE,tv_events:uids(tvFeed),movie_events:movieApi.items.map(i=>i.event_id)}));
}finally{await browser.close();}
