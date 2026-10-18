import test from 'node:test';
import assert from 'node:assert/strict';
import {matchesPendingProvider} from '../src/lib/provider-draft.ts';
test('unknown creation adoption compares all submitted public settings without equating unrelated records',()=>{
 const pending={name:'shared',enabled:true,priority:3,settings:{implementation:'qbittorrent',endpoint:'http://127.0.0.1:1234',tv:{category:'tv'},movies:{category:'movies'}}};
 const stored={id:'new',revision:1,has_credentials:true,...pending,settings:{movies:{category:'movies'},tv:{category:'tv'},endpoint:pending.settings.endpoint,implementation:'qbittorrent'}};
 assert.equal(matchesPendingProvider(stored,pending),true);
 for(const change of [{name:'other'},{enabled:false},{priority:4},{settings:{...pending.settings,endpoint:'http://127.0.0.1:4321'}},{settings:{...pending.settings,movies:null}}])assert.equal(matchesPendingProvider({...stored,...change},pending),false);
 // An identical concurrent/preexisting config is only an adoption candidate, not creation attribution.
 assert.equal(matchesPendingProvider({...stored,id:'preexisting',revision:8},pending),true);
});
